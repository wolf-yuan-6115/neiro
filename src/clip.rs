//! Audio excerpts for the A/B tuner: decoding, picking the fullest section of a
//! song, loudness matching and looped playback through the neiro sink.

use crate::params::PersonalEq;
use crate::pw::SINK_NAME;
use anyhow::{anyhow, Context, Result};
use realfft::RealFftPlanner;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub const FS: f32 = 48000.0;
pub const SEGMENT_S: usize = 10;
const OCTAVES: [f32; 8] = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];
const FFT: usize = 8192;

pub struct Clip {
    pub name: String,
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    /// Where the excerpt starts in the source, seconds.
    pub start_s: f32,
    /// Long-term power spectrum of the excerpt, one value per FFT bin.
    pub spectrum: Vec<f32>,
    /// Extra samples after the loop unit, crossfaded into each repeat (0 for a seamless loop).
    pub tail: usize,
}

impl Clip {
    pub fn demo(demo: crate::demo::Demo) -> Clip {
        let (l, r) = crate::demo::generate(demo);
        let spectrum = power_spectrum(&l, &r);
        Clip { name: format!("Demo: {}", demo.label()), l, r, start_s: 0.0, spectrum, tail: 0 }
    }

    /// An embedded excerpt: already cut to the section to play, plus a short tail
    /// for the loop crossfade.
    pub fn from_sample(s: &crate::samples::Sample) -> Result<Clip> {
        let (l, r) = decode_bytes(s.data)?;
        let unit = (SEGMENT_S * FS as usize).min(l.len());
        let tail = (l.len() - unit).min(XFADE);
        let (l, r) = (l[..unit + tail].to_vec(), r[..unit + tail].to_vec());
        let spectrum = power_spectrum(&l, &r);
        Ok(Clip { name: format!("{} — {}", s.title, s.artist), l, r, start_s: s.start_s, spectrum, tail })
    }

    pub fn from_file(path: &Path) -> Result<Clip> {
        let (l, r) = decode(path)?;
        let start = best_segment(&l, &r, FS, SEGMENT_S);
        let len = (SEGMENT_S * FS as usize).min(l.len() - start);
        // Keep a little extra after the excerpt for the loop crossfade.
        let end = (start + len + XFADE).min(l.len());
        let tail = end - (start + len);
        let (l, r) = (l[start..end].to_vec(), r[start..end].to_vec());
        let spectrum = power_spectrum(&l, &r);
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Clip { name, l, r, start_s: start as f32 / FS, spectrum, tail })
    }

    pub fn segment_label(&self) -> String {
        let fmt = |s: f32| format!("{}:{:02}", s as u32 / 60, s as u32 % 60);
        let len = (self.l.len() - self.tail) as f32 / FS;
        format!("{}–{}", fmt(self.start_s), fmt(self.start_s + len))
    }
}

/// Decode any format ffmpeg understands to 48 kHz stereo f32.
pub fn decode(path: &Path) -> Result<(Vec<f32>, Vec<f32>)> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-ac", "2", "-ar", "48000", "-"])
        .stdin(Stdio::null())
        .output()
        .context("running ffmpeg")?;
    if !out.status.success() {
        return Err(anyhow!("ffmpeg could not decode {}: {}", path.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    to_stereo(&out.stdout).with_context(|| path.display().to_string())
}

/// Decode an in-memory file (e.g. an embedded sample) through ffmpeg's stdin.
pub fn decode_bytes(data: &'static [u8]) -> Result<(Vec<f32>, Vec<f32>)> {
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-i", "pipe:0", "-f", "f32le", "-ac", "2", "-ar", "48000", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running ffmpeg")?;
    let mut stdin = child.stdin.take().unwrap();
    // Feed stdin from another thread so a full stdout pipe can't deadlock us.
    let feeder = std::thread::spawn(move || {
        use std::io::Write;
        let _ = stdin.write_all(data);
    });
    let out = child.wait_with_output()?;
    let _ = feeder.join();
    if !out.status.success() {
        return Err(anyhow!("ffmpeg could not decode sample: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    to_stereo(&out.stdout)
}

fn to_stereo(bytes: &[u8]) -> Result<(Vec<f32>, Vec<f32>)> {
    let out = bytes;
    let frames = out.len() / 8;
    if frames < FS as usize {
        return Err(anyhow!("audio is shorter than one second"));
    }
    let (mut l, mut r) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    for c in out.chunks_exact(8) {
        l.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
        r.push(f32::from_le_bytes([c[4], c[5], c[6], c[7]]));
    }
    Ok((l, r))
}

fn octave_of(f: f32) -> Option<usize> {
    OCTAVES.iter().position(|&c| f >= c / std::f32::consts::SQRT_2 && f < c * std::f32::consts::SQRT_2)
}

/// Octave-band energies in dB for each `hop`-sample block.
fn octave_frames(l: &[f32], r: &[f32], fs: f32, hop: usize) -> Vec<[f32; 8]> {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT);
    let mut input = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let window: Vec<f32> = (0..FFT).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT as f32).cos()).collect();
    let bin_oct: Vec<Option<usize>> = (0..spec.len()).map(|k| octave_of(k as f32 * fs / FFT as f32)).collect();
    let mut frames = vec![];
    let mut s = 0;
    while s + hop <= l.len() {
        let mut e = [0f32; 8];
        // A few FFT frames per hop are plenty for a section-level estimate.
        let mut off = 0;
        while off + FFT <= hop.max(FFT) && s + off + FFT <= l.len() {
            for i in 0..FFT {
                input[i] = 0.5 * (l[s + off + i] + r[s + off + i]) * window[i];
            }
            fft.process(&mut input, &mut spec).unwrap();
            for (k, x) in spec.iter().enumerate() {
                if let Some(o) = bin_oct[k] {
                    e[o] += x.norm_sqr();
                }
            }
            off += FFT;
        }
        frames.push(e.map(|v| 10.0 * (v + 1e-12).log10()));
        s += hop;
    }
    frames
}

/// Average octave-band energies (dB) over the whole signal.
#[cfg(test)]
pub fn octave_energy(l: &[f32], r: &[f32], fs: f32) -> [f32; 8] {
    let frames = octave_frames(l, r, fs, FFT);
    let mut sum = [0f64; 8];
    for f in &frames {
        for b in 0..8 {
            sum[b] += 10f64.powf(f[b] as f64 / 10.0);
        }
    }
    sum.map(|v| (10.0 * (v / frames.len().max(1) as f64 + 1e-12).log10()) as f32)
}

/// Start sample of the `secs`-long window where bass, mids and highs are all
/// active: each octave is normalised to its loudest second, and a window scores
/// the mean over its seconds of the *weakest* octave.
pub fn best_segment(l: &[f32], r: &[f32], fs: f32, secs: usize) -> usize {
    let hop = fs as usize;
    let frames = octave_frames(l, r, fs, hop);
    if frames.len() <= secs {
        return 0;
    }
    let mut maxes = [f32::MIN; 8];
    for f in &frames {
        for b in 0..8 {
            maxes[b] = maxes[b].max(f[b]);
        }
    }
    let score: Vec<f32> = frames
        .iter()
        .map(|f| (0..8).map(|b| (f[b] - maxes[b]).max(-60.0)).fold(f32::MAX, f32::min))
        .collect();
    let best = (0..=frames.len() - secs)
        .max_by(|&a, &b| {
            let sa: f32 = score[a..a + secs].iter().sum();
            let sb: f32 = score[b..b + secs].iter().sum();
            sa.total_cmp(&sb)
        })
        .unwrap_or(0);
    best * hop
}

/// Long-term average power per FFT bin (mono sum).
pub fn power_spectrum(l: &[f32], r: &[f32]) -> Vec<f32> {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT);
    let mut input = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut acc = vec![0f32; spec.len()];
    let mut s = 0;
    while s + FFT <= l.len() {
        for i in 0..FFT {
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT as f32).cos();
            input[i] = 0.5 * (l[s + i] + r[s + i]) * w;
        }
        fft.process(&mut input, &mut spec).unwrap();
        for (a, x) in acc.iter_mut().zip(&spec) {
            *a += x.norm_sqr();
        }
        s += FFT / 2;
    }
    acc
}

/// ITU-R BS.1770 K-weighting (the curve behind LUFS) as a power weight. Unlike
/// A-weighting it doesn't discount bass, so a bass boost can't win just by being louder.
fn k_weight(f: f32) -> f32 {
    use crate::dsp::biquad::Coefs;
    let fs = FS as f64;
    let shelf = Coefs::highshelf(fs, 1681.97, std::f64::consts::FRAC_1_SQRT_2, 4.0);
    let rlb = Coefs::highpass(fs, 38.13, 0.5003);
    10f32.powf((shelf.response_db(fs, f as f64) + rlb.response_db(fs, f as f64)) as f32 / 10.0)
}

/// K-weighted loudness change (dB) that `eq` causes on content with `spectrum`.
pub fn loudness_delta(spectrum: &[f32], eq: &PersonalEq) -> f32 {
    if !eq.enable {
        return 0.0;
    }
    let coefs = eq.coefs(FS as f64);
    let (mut num, mut den) = (0f64, 0f64);
    for (k, p) in spectrum.iter().enumerate().skip(1) {
        let f = k as f32 * FS / FFT as f32;
        let w = (*p * k_weight(f)) as f64;
        let h_db: f64 = coefs.iter().map(|c| c.response_db(FS as f64, f as f64)).sum();
        num += w * 10f64.powf(h_db / 10.0);
        den += w;
    }
    if den <= 0.0 {
        return 0.0;
    }
    (10.0 * (num / den).log10()) as f32
}

const XFADE: usize = 2400; // 50 ms

/// Write the excerpt looped `reps` times with crossfaded joins (and gentle fades
/// at both ends) as a float WAV.
pub fn write_loop(path: &Path, clip: &Clip, reps: usize) -> Result<()> {
    let n = clip.l.len();
    let unit = (n - clip.tail).max(1);
    let x = n - unit;
    let spec = hound::WavSpec { channels: 2, sample_rate: FS as u32, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec)?;
    let total = unit * reps;
    let edge = (FS * 0.1) as usize;
    for i in 0..total {
        let j = i % unit;
        let (mut a, mut b) = (clip.l[j], clip.r[j]);
        // First `x` samples of every repetition blend in the tail that follows the excerpt.
        if j < x && i >= unit {
            let t = j as f32 / x as f32;
            let (fi, fo) = ((t * std::f32::consts::FRAC_PI_2).sin(), (t * std::f32::consts::FRAC_PI_2).cos());
            a = a * fi + clip.l[unit + j] * fo;
            b = b * fi + clip.r[unit + j] * fo;
        }
        let g = (i.min(total - 1 - i) as f32 / edge as f32).min(1.0);
        w.write_sample(a * g)?;
        w.write_sample(b * g)?;
    }
    w.finalize()?;
    Ok(())
}

pub fn clip_path() -> PathBuf {
    crate::ipc::socket_path().with_file_name("neiro-clip.wav")
}

/// Plays the looped clip into the neiro sink with `pw-play`, restarting it if it ends.
pub struct Player {
    child: Option<Child>,
    path: PathBuf,
}

impl Player {
    pub fn start(clip: &Clip) -> Result<Player> {
        let path = clip_path();
        let unit_s = (clip.l.len() - clip.tail) as f32 / FS;
        let reps = (180.0 / unit_s.max(1.0)).ceil() as usize;
        write_loop(&path, clip, reps)?;
        let mut p = Player { child: None, path };
        p.spawn()?;
        Ok(p)
    }

    fn spawn(&mut self) -> Result<()> {
        let child = Command::new("pw-play")
            .args(["--target", SINK_NAME, "--media-role", "Music"])
            .arg(&self.path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("starting pw-play")?;
        self.child = Some(child);
        Ok(())
    }

    /// Restart playback if the loop file finished.
    pub fn poll(&mut self) {
        if let Some(c) = &mut self.child {
            if matches!(c.try_wait(), Ok(Some(_))) {
                let _ = self.spawn();
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_full_section_after_quiet_intro() {
        // 20 s of soft high-only intro, then 20 s of the full-range demo, then 10 s intro again.
        let (dl, dr) = crate::demo::generate(crate::demo::Demo::Electronic);
        let fs = FS as usize;
        let mut l = vec![];
        let mut r = vec![];
        let mut seed = 7u32;
        let mut intro = |l: &mut Vec<f32>, r: &mut Vec<f32>, secs: usize| {
            let mut prev = 0.0;
            for _ in 0..secs * fs {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let x = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                let hp = 0.05 * (x - prev);
                prev = x;
                l.push(hp);
                r.push(hp);
            }
        };
        intro(&mut l, &mut r, 20);
        for k in 0..20 * fs {
            l.push(dl[k % dl.len()]);
            r.push(dr[k % dr.len()]);
        }
        intro(&mut l, &mut r, 10);
        let start = best_segment(&l, &r, FS, SEGMENT_S) / fs;
        assert!((20..=30).contains(&start), "picked {start}s");
    }

    #[test]
    fn level_match_cancels_bass_boost() {
        let clip = Clip::demo(crate::demo::Demo::Electronic);
        let mut eq = PersonalEq::default();
        eq.bands[0].gain_db = 6.0;
        let d = loudness_delta(&clip.spectrum, &eq);
        // The demo carries most energy below 250 Hz, so a +6 dB shelf adds several dB.
        assert!((1.5..6.0).contains(&d), "+6 dB bass shelf on the demo: {d}");
        eq.bands[0].gain_db = -6.0;
        assert!(loudness_delta(&clip.spectrum, &eq) < -1.5);
        assert!(loudness_delta(&clip.spectrum, &PersonalEq::default()).abs() < 1e-3);
    }

    #[test]
    fn loop_file_has_no_clicks() {
        let clip = Clip::demo(crate::demo::Demo::Electronic);
        let dir = std::env::temp_dir().join(format!("neiro-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("loop.wav");
        write_loop(&p, &clip, 3).unwrap();
        let s: Vec<f32> = hound::WavReader::open(&p).unwrap().samples::<f32>().map(|v| v.unwrap()).collect();
        let max_jump = s.chunks(2).map(|c| c[0]).collect::<Vec<_>>().windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        let src_jump = clip.l.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        assert!(max_jump <= src_jump * 1.05 + 1e-3, "{max_jump} vs {src_jump}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
