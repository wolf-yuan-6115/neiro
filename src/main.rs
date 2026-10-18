mod daemon;
mod clip;
mod demo;
mod dsp;
mod ipc;
mod params;
mod pw;
mod samples;
mod system;
mod tuner;
mod tui;
mod tuning;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use dsp::chain::{Config, Engine, LATENCY};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "neiro", about = "neiro (音色): speaker enhancement for the Zenbook S14 on PipeWire")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the PipeWire DSP node and control socket.
    Daemon {
        /// Use a different tuning.xml instead of the embedded one.
        #[arg(long)]
        tuning: Option<PathBuf>,
    },
    /// Interactive terminal UI (default).
    Tui,
    /// Toggle (or set) bypass on the running daemon.
    Toggle {
        #[arg(value_parser = ["on", "off"])]
        state: Option<String>,
    },
    /// Switch profile on the running daemon.
    Profile { name: String },
    /// Print daemon status.
    Status,
    /// Process a 48 kHz WAV file offline for A/B listening.
    Render {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, default_value = "music")]
        profile: String,
        /// Enable the virtual bass stage.
        #[arg(long)]
        vbass: bool,
        /// Use saved user overrides for the profile if present.
        #[arg(long)]
        saved: bool,
        /// Print average per-band IEQ correction and dynamics gain reduction.
        #[arg(long)]
        stats: bool,
    },
    /// Print the parsed tuning profiles.
    DumpTuning {
        #[arg(long)]
        tuning: Option<PathBuf>,
    },
    /// Install and start the systemd user service.
    Install,
    /// Show which section of a song the A/B tuner would play.
    #[command(hide = true)]
    Segment { file: PathBuf },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Tui) {
        Cmd::Daemon { tuning } => daemon::run(tuning),
        Cmd::Tui => tui::run(),
        Cmd::Toggle { state } => {
            let on = state.map(|s| s == "off"); // "off" means effect off -> bypass on
            let r = ipc::Client::connect()?.call(&ipc::Request::Bypass { on })?;
            let st = r.state.unwrap();
            println!("neiro: {}", if st.bypass { "bypassed" } else { "on" });
            Ok(())
        }
        Cmd::Profile { name } => {
            let r = ipc::Client::connect()?.call(&ipc::Request::SetProfile { profile: name })?;
            println!("neiro: profile {}", r.state.unwrap().profile);
            Ok(())
        }
        Cmd::Status => {
            let r = ipc::Client::connect()?.call(&ipc::Request::GetState)?;
            let st = r.state.unwrap();
            println!(
                "neiro: {} | profile {}{} | target {} | latency {:.1} ms",
                if st.bypass { "BYPASS" } else { "ON" },
                st.profile,
                if st.dirty { " (unsaved changes)" } else { "" },
                st.target,
                st.latency_ms
            );
            Ok(())
        }
        Cmd::Render { input, output, profile, vbass, saved, stats } => render(&input, &output, &profile, vbass, saved, stats),
        Cmd::DumpTuning { tuning } => {
            let t = tuning::load(tuning.as_deref())?;
            println!("bands (Hz): {:?}", t.band_freqs);
            for (k, v) in &t.ieq_curves {
                println!("ieq {k}: {v:?}");
            }
            for p in &t.profiles {
                println!("\n== {} ==\n{:#?}", p.name, params::Params::from_profile(p));
            }
            Ok(())
        }
        Cmd::Install => install(),
        Cmd::Segment { file } => {
            let c = clip::Clip::from_file(&file)?;
            println!("{}: {}", c.name, c.segment_label());
            Ok(())
        }
    }
}

fn render(input: &PathBuf, output: &PathBuf, profile: &str, vbass: bool, saved: bool, stats: bool) -> Result<()> {
    let t = tuning::load(None)?;
    let mut p = params::factory(&t, profile)?;
    if saved {
        if let Some(o) = params::Saved::load().overrides.get(profile) {
            p = o.clone();
        }
    }
    p.vbass.enable |= vbass;
    let mut rd = hound::WavReader::open(input).with_context(|| format!("opening {}", input.display()))?;
    let spec = rd.spec();
    if spec.sample_rate != 48000 {
        return Err(anyhow!("input must be 48 kHz (got {}); resample with: sox in.wav -r 48000 out.wav", spec.sample_rate));
    }
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => rd.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            rd.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    let ch = spec.channels as usize;
    let frames = samples.len() / ch;
    let get = |c: usize| -> Vec<f32> {
        let mut v: Vec<f32> = (0..frames).map(|i| samples[i * ch + c.min(ch - 1)]).collect();
        v.extend(std::iter::repeat_n(0.0, LATENCY));
        v
    };
    let (l, r) = (get(0), get(1));
    let personal = if saved { params::Saved::load().personal } else { params::PersonalEq { enable: false, ..Default::default() } };
    let mut e = Engine::new(48000.0, &t, Config::build(&p, &t, 48000.0, false, 0.0).with_personal(&personal, 48000.0));
    let (mut ol, mut or) = (vec![0.0; l.len()], vec![0.0; l.len()]);
    // Block-wise like the live node, so meters can be sampled.
    let nb = tuning::NBANDS;
    let (mut ieq, mut gr, mut lvl, mut count) = (vec![0f64; nb], vec![0f64; nb], vec![0f64; nb], 0usize);
    for s in (0..l.len()).step_by(1024) {
        let e2 = (s + 1024).min(l.len());
        e.process(&l[s..e2], &r[s..e2], &mut ol[s..e2], &mut or[s..e2]);
        if s > 48000 * 3 && e.meters.in_peak_db > -60.0 {
            for b in 0..nb {
                ieq[b] += e.meters.ieq_db[b] as f64;
                gr[b] += e.meters.gr_db[b] as f64;
                lvl[b] += e.meters.level_db[b] as f64;
            }
            count += 1;
        }
    }
    if stats && count > 0 {
        let row = |name: &str, v: &[f64]| {
            println!("{name:<8}{}", v.iter().map(|x| format!("{:>6.1}", x / count as f64)).collect::<String>())
        };
        println!("{:<8}{}", "band", t.band_freqs.iter().map(|f| format!("{f:>6.0}")).collect::<String>());
        row("level", &lvl);
        row("ieq", &ieq);
        row("dyn gr", &gr);
    }
    let out_spec = hound::WavSpec { channels: 2, sample_rate: 48000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(output, out_spec)?;
    for i in LATENCY..l.len() {
        w.write_sample(ol[i])?;
        w.write_sample(or[i])?;
    }
    w.finalize()?;
    println!("wrote {} ({} frames, profile {profile}{})", output.display(), frames, if p.vbass.enable { ", virtual bass" } else { "" });
    Ok(())
}

fn install() -> Result<()> {
    let exe = std::env::current_exe()?;
    let dir = dirs::config_dir().ok_or_else(|| anyhow!("no config dir"))?.join("systemd/user");
    std::fs::create_dir_all(&dir)?;
    let unit = format!(
        "[Unit]\nDescription=neiro speaker enhancement (PipeWire DSP)\nAfter=pipewire.service wireplumber.service\nRequires=pipewire.service\n\n[Service]\nExecStart={} daemon\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=pipewire.service\n",
        exe.display()
    );
    let path = dir.join("neiro.service");
    std::fs::write(&path, unit)?;
    println!("wrote {}", path.display());
    for args in [&["daemon-reload"][..], &["enable", "--now", "neiro.service"][..]] {
        let st = std::process::Command::new("systemctl").arg("--user").args(args).status()?;
        if !st.success() {
            return Err(anyhow!("systemctl --user {} failed", args.join(" ")));
        }
    }
    println!("neiro.service enabled and started");
    Ok(())
}
