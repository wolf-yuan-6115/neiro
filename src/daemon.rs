//! `neiro daemon`: owns the parameter state, serves IPC and runs the PipeWire node.

use crate::clip::{self, Clip, Player};
use crate::dsp::chain::{self, Config, Engine, PersonalCfg, SharedMeters};
use crate::ipc::{self, Handler, Request, Response, StateView, TunerSource, TunerView};
use crate::params::{self, factory, Params, PersonalEq, Saved, PERSONAL_PREFIX};
use crate::tuner::Tuner;
use crate::pw::{self, RtState, RATE, SINK_NAME};
use crate::system;
use crate::tuning::{self, Tuning};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SINK_KEY: &str = "default.configured.audio.sink";

struct Inner {
    tuning: Tuning,
    /// Profiles this tuning offers, in display order.
    profiles: Vec<String>,
    saved: Saved,
    profile: String,
    bypass: bool,
    working: BTreeMap<String, Params>,
    tx: rtrb::Producer<Box<Config>>,
    back: rtrb::Consumer<Box<Config>>,
    meters: Arc<SharedMeters>,
    target: String,
    hw_comp_db: f32,
    pinned: bool,
    session: Option<Session>,
    speaker_is_default: bool,
    /// A client asked to make neiro the default; the monitor thread acts on it.
    want_default: bool,
}

/// A running blind A/B tuner session.
struct Session {
    tuner: Tuner,
    _player: Player,
    spectrum: Vec<f32>,
    clip: String,
    segment: String,
    listen: usize,
    auto: Option<f32>,
    last_switch: Instant,
}

impl Session {
    fn view(&self) -> TunerView {
        let (trial, total) = self.tuner.progress();
        TunerView {
            clip: self.clip.clone(),
            segment: self.segment.clone(),
            listening: self.listen,
            auto_secs: self.auto,
            trial,
            total,
            done: self.tuner.done,
            low_confidence: self.tuner.low_confidence(),
            result: self.tuner.done.then(|| self.tuner.current.clone()),
        }
    }
}

impl Inner {
    fn params(&self) -> &Params {
        &self.working[&self.profile]
    }

    fn config(&self) -> Config {
        let fs = RATE as f32;
        let cfg = Config::build(self.params(), &self.tuning, fs, self.bypass, self.hw_comp_db);
        match &self.session {
            Some(s) => {
                let mut cfg = cfg;
                // Each side is loudness-matched on the clip so louder never wins by default.
                cfg.personal = s.tuner.sides().map(|eq| PersonalCfg::new(&eq, fs, -clip::loudness_delta(&s.spectrum, &eq)));
                cfg.side = s.listen;
                cfg
            }
            None => cfg.with_personal(&self.saved.personal, fs),
        }
    }

    fn push(&mut self) {
        // Configs come back from the audio thread here so they are freed off the RT path.
        while self.back.pop().is_ok() {}
        let cfg = Box::new(self.config());
        if self.tx.push(cfg).is_err() {
            eprintln!("neiro: config queue full, dropping update");
        }
    }

    fn baseline(&self, profile: &str) -> Params {
        self.saved
            .overrides
            .get(profile)
            .cloned()
            .unwrap_or_else(|| factory(&self.tuning, profile).expect("known profile"))
    }

    fn persist(&self) {
        if let Err(e) = self.saved.store() {
            eprintln!("neiro: saving state: {e:#}");
        }
    }

    fn view(&self) -> StateView {
        StateView {
            profile: self.profile.clone(),
            profiles: self.profiles.clone(),
            bypass: self.bypass,
            dirty: *self.params() != self.baseline(&self.profile),
            customised: self.saved.overrides.contains_key(&self.profile),
            params: self.params().clone(),
            sections: params::sections(self.params(), &self.tuning.band_freqs),
            band_freqs: self.tuning.band_freqs.to_vec(),
            latency_ms: chain::LATENCY as f32 * 1000.0 / RATE as f32,
            target: self.target.clone(),
            hw_comp_db: self.hw_comp_db,
            pinned: self.pinned,
            personal: self.saved.personal.clone(),
            tuner: self.session.as_ref().map(Session::view),
            speaker_is_default: self.speaker_is_default,
        }
    }

    fn session(&mut self) -> Result<&mut Session> {
        self.session.as_mut().ok_or_else(|| anyhow!("no tuner session running"))
    }

    fn handle(&mut self, req: Request) -> Result<Response> {
        match req {
            Request::Meters => return Ok(Response { meters: Some(self.meters.load()), ..Default::default() }),
            Request::GetState => {}
            Request::Set { path, value } => {
                if let Some(rest) = path.strip_prefix(PERSONAL_PREFIX) {
                    if self.session.is_some() {
                        return Err(anyhow!("personal EQ is locked while the tuner runs"));
                    }
                    let mut v = serde_json::to_value(&self.saved.personal)?;
                    *v.pointer_mut(rest).ok_or_else(|| anyhow!("no such parameter {path}"))? = value;
                    self.saved.personal = serde_json::from_value(v).map_err(|e| anyhow!("invalid value for {path}: {e}"))?;
                    self.persist();
                } else {
                    let p = self.working.get_mut(&self.profile).unwrap();
                    p.set(&path, value)?;
                }
                self.push();
            }
            Request::TunerStart { .. } => unreachable!("handled by Controller"),
            Request::TunerListen { side } => {
                let s = self.session()?;
                s.listen = side.min(1);
                s.last_switch = Instant::now();
                self.push();
            }
            Request::TunerAnswer { choice } => {
                let s = self.session()?;
                s.tuner.answer(choice);
                s.listen = 0;
                s.last_switch = Instant::now();
                self.push();
            }
            Request::TunerAutoSwitch { secs } => {
                let s = self.session()?;
                s.auto = secs.filter(|v| *v > 0.5);
                s.last_switch = Instant::now();
            }
            Request::TunerFinish => {
                let s = self.session()?;
                s.tuner.finish();
                s.listen = 0;
                self.push();
            }
            Request::TunerStop { keep } => {
                let s = self.session.take().ok_or_else(|| anyhow!("no tuner session running"))?;
                if keep {
                    self.saved.personal = PersonalEq { enable: true, ..s.tuner.current.clone() };
                    self.persist();
                }
                self.push();
            }
            Request::SetProfile { profile } => {
                if !self.profiles.contains(&profile) {
                    return Err(anyhow!("this tuning has no profile named {profile} (has: {})", self.profiles.join(", ")));
                }
                self.profile = profile;
                self.saved.profile = Some(self.profile.clone());
                self.persist();
                self.push();
            }
            Request::Bypass { on } => {
                self.bypass = on.unwrap_or(!self.bypass);
                self.saved.bypass = self.bypass;
                self.persist();
                self.push();
            }
            Request::MakeDefault => self.want_default = true,
            Request::Save => {
                let p = self.params().clone();
                self.saved.overrides.insert(self.profile.clone(), p);
                self.persist();
            }
            Request::Reset => {
                self.saved.overrides.remove(&self.profile);
                let f = factory(&self.tuning, &self.profile)?;
                self.working.insert(self.profile.clone(), f);
                self.persist();
                self.push();
            }
        }
        Ok(Response { state: Some(self.view()), ..Default::default() })
    }
}

pub struct Controller(Mutex<Inner>);

impl Controller {
    /// Records whether the speaker is the default; returns a pending make-default request.
    fn routing(&self, speaker_is_default: bool) -> bool {
        let mut i = self.0.lock().unwrap();
        i.speaker_is_default = speaker_is_default;
        std::mem::take(&mut i.want_default)
    }

    fn set_hw(&self, comp_db: f32, pinned: bool) {
        let mut i = self.0.lock().unwrap();
        i.pinned = pinned;
        if (i.hw_comp_db - comp_db).abs() > 0.05 {
            i.hw_comp_db = comp_db;
            i.push();
        }
    }
}

impl Controller {
    /// Decoding a song takes a moment, so it happens before taking the lock.
    fn start_tuner(&self, source: TunerSource) -> Result<Response> {
        let clip = match source {
            TunerSource::Demo { demo } => Clip::demo(demo),
            TunerSource::File { path } => Clip::from_file(std::path::Path::new(&path))?,
            TunerSource::Sample { id } => {
                Clip::from_sample(crate::samples::find(&id).ok_or_else(|| anyhow!("unknown sample {id}"))?)?
            }
        };
        let mut i = self.0.lock().unwrap();
        i.session = None; // stops any previous player first
        let player = Player::start(&clip)?;
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
        i.session = Some(Session {
            tuner: Tuner::new(i.saved.personal.clone(), seed),
            _player: player,
            segment: clip.segment_label(),
            clip: clip.name,
            spectrum: clip.spectrum,
            listen: 0,
            auto: None,
            last_switch: Instant::now(),
        });
        if i.bypass {
            // A/B through a bypassed chain would compare nothing.
            i.bypass = false;
            i.saved.bypass = false;
            i.persist();
        }
        i.push();
        Ok(Response { state: Some(i.view()), ..Default::default() })
    }

    /// Periodic housekeeping: loop playback and automatic A/B alternation.
    fn tick(&self) {
        let mut i = self.0.lock().unwrap();
        let mut switched = false;
        if let Some(s) = &mut i.session {
            s._player.poll();
            if let Some(secs) = s.auto {
                if !s.tuner.done && s.last_switch.elapsed().as_secs_f32() >= secs {
                    s.listen ^= 1;
                    s.last_switch = Instant::now();
                    switched = true;
                }
            }
        }
        if switched {
            i.push();
        }
    }
}

impl Handler for Controller {
    fn handle(&self, req: Request) -> Response {
        let res = match req {
            Request::TunerStart { source } => self.start_tuner(source),
            req => self.0.lock().unwrap().handle(req),
        };
        res.unwrap_or_else(|e| Response { error: Some(format!("{e:#}")), ..Default::default() })
    }
}

/// What we changed in the session so it can be undone, also kept on disk for crash recovery.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Restore {
    /// Speaker (volume, muted) before we pinned it.
    speaker_volume: Option<(f32, bool)>,
    /// Some(previous value) if we replaced the configured default sink; inner None = key was unset.
    default_sink: Option<Option<String>>,
    /// We became the default through WirePlumber's history, standing in for the
    /// speaker, while the configured default stayed the user's (absent) device.
    #[serde(default)]
    stand_in: bool,
    /// Last seen (volume, muted) of the virtual sink, handed back to the speaker on exit.
    sink_state: Option<(f32, bool)>,
}

fn restore_path() -> std::path::PathBuf {
    ipc::socket_path().with_file_name("neiro.restore.json")
}

fn apply_restore(r: &Restore, target: &str) {
    // If we were the default, the speaker takes over the volume/mute the user last
    // set on the virtual sink; otherwise just undo our pinning.
    let took_over = r.default_sink.is_some() || r.stand_in;
    let state = if took_over { r.sink_state.or(r.speaker_volume) } else { r.speaker_volume };
    if let (Some((v, m)), Some(id)) = (state, system::node_id(target)) {
        let _ = system::set_volume(id, v);
        let _ = system::set_mute(id, m);
    }
    if let Some(prev) = &r.default_sink {
        // Only undo if the default still points at us.
        if system::default_name(SINK_KEY).as_deref() == Some(SINK_NAME) {
            let _ = system::set_default(SINK_KEY, prev.as_deref());
        }
    }
}

pub fn run(tuning_override: Option<std::path::PathBuf>) -> Result<()> {
    // Block SIGINT/SIGTERM before any thread exists so every thread inherits the
    // mask; PipeWire's signalfd sources then receive them and we can clean up.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    let saved = Saved::load();
    let tuning_path = tuning::resolve(tuning_override.as_deref(), saved.tuning_path.as_deref())?;
    let tuning = tuning::load(&tuning_path, saved.mode.as_deref())?;
    eprintln!("neiro: tuning {} (mode {})", tuning_path.display(), tuning.mode);
    let target = system::find_speaker(saved.speaker_sink.as_deref())?;
    eprintln!("neiro: target speaker sink {target}");

    // Recover from a previous crash before taking new snapshots.
    if let Ok(s) = std::fs::read_to_string(restore_path()) {
        if let Ok(r) = serde_json::from_str::<Restore>(&s) {
            eprintln!("neiro: restoring state left by a previous run");
            apply_restore(&r, &target);
        }
        let _ = std::fs::remove_file(restore_path());
    }

    let profiles = tuning.profile_names();
    let mut working = BTreeMap::new();
    for p in &profiles {
        let base = saved.overrides.get(p).cloned().map_or_else(|| factory(&tuning, p), Ok)?;
        working.insert(p.clone(), base);
    }
    let profile = saved.profile.clone().filter(|p| profiles.contains(p)).unwrap_or_else(|| tuning.initial_profile());
    let (tx, rx) = rtrb::RingBuffer::<Box<Config>>::new(64);
    let (back_tx, back) = rtrb::RingBuffer::<Box<Config>>::new(64);
    let meters = Arc::new(SharedMeters::default());
    let hw_volume = saved.hw_volume.unwrap_or(1.0);
    // Assume the current speaker volume until the monitor has looked.
    let initial_comp = system::node_id(&target).and_then(system::get_volume).map_or(0.0, |(v, _)| -system::volume_db(v));
    let inner = Inner {
        bypass: saved.bypass,
        tuning: tuning.clone(),
        profiles,
        saved,
        profile,
        working,
        tx,
        back,
        meters: meters.clone(),
        target: target.clone(),
        hw_comp_db: initial_comp,
        pinned: false,
        session: None,
        speaker_is_default: false,
        want_default: false,
    };
    let engine = Engine::new(RATE as f32, &tuning, inner.config());
    let controller = Arc::new(Controller(Mutex::new(inner)));
    ipc::serve(controller.clone())?;
    let ticker = controller.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(100));
        ticker.tick();
    });

    let restore = Arc::new(Mutex::new(Restore::default()));
    let stopping = Arc::new(AtomicBool::new(false));
    let faded = Arc::new(AtomicBool::new(false));
    let r2 = restore.clone();
    let t2 = target.clone();
    let s2 = stopping.clone();
    let on_started = move || {
        std::thread::spawn(move || monitor(&t2, hw_volume, &r2, &controller, &s2));
    };

    let rt = RtState::new(engine, rx, back_tx, meters, stopping.clone(), faded.clone());
    let r3 = restore.clone();
    let t3 = target.clone();
    // Runs after the main loop stops but while our nodes still exist. Order matters to
    // avoid a pop or a burst at full volume:
    //   1. fade our output to silence (the real-time thread keeps running),
    //   2. give the speaker back its volume and mute while nothing plays through it,
    //   3. restore the default sink so apps move onto an already-quieted speaker,
    //   4. only then let pw::run disconnect.
    let on_quit = move || {
        stopping.store(true, Ordering::Relaxed);
        let t0 = Instant::now();
        // If nothing is playing the graph may be idle and never call us; don't wait long.
        while !faded.load(Ordering::Acquire) && t0.elapsed() < Duration::from_millis(300) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut r = std::mem::take(&mut *r3.lock().unwrap());
        if let Some(st) = system::node_id(SINK_NAME).and_then(system::get_volume) {
            r.sink_state = Some(st);
        }
        apply_restore(&r, &t3);
        let _ = std::fs::remove_file(restore_path());
        // Let WirePlumber move the streams before our (silent) sink goes away.
        std::thread::sleep(Duration::from_millis(250));
    };
    let res = pw::run(rt, &target, on_started, on_quit);
    eprintln!("neiro: shut down");
    // Normally already done (and emptied) in on_quit; this covers an early exit.
    apply_restore(&restore.lock().unwrap(), &target);
    let _ = std::fs::remove_file(restore_path());
    let _ = std::fs::remove_file(ipc::socket_path());
    res
}

fn save_restore(r: &Restore) {
    let _ = std::fs::write(restore_path(), serde_json::to_string(r).unwrap_or_default());
}

/// Makes the virtual sink the default in place of the speaker, carrying over the
/// speaker's volume and mute. Call only while the speaker (or neiro) is the default.
fn become_default(target: &str, sink_id: u32, spk_id: u32, r: &mut Restore) {
    // While pinned, the speaker reads 100%; the user's level is the saved one.
    if let Some((v, m)) = r.speaker_volume.or_else(|| system::get_volume(spk_id)) {
        let _ = system::set_volume(sink_id, v);
        let _ = system::set_mute(sink_id, m);
    }
    let prev = system::get_default(SINK_KEY);
    let json = |n: &str| serde_json::json!({ "name": n }).to_string();
    let _ = system::set_default(SINK_KEY, Some(&json(SINK_NAME)));
    let prev_name = prev.as_deref().and_then(|p| {
        serde_json::from_str::<serde_json::Value>(p).ok()?.get("name")?.as_str().map(String::from)
    });
    match prev_name {
        // The user chose a device that isn't plugged in (else it would be the
        // default). Take the speaker's place in WirePlumber's history instead of
        // the configured slot, so that device still takes over when it returns,
        // as it does without neiro.
        Some(chosen) if chosen != target && chosen != SINK_NAME => {
            let mut pushes = system::history_pushes(&system::default_sink_history(), target, SINK_NAME);
            if pushes.last() != Some(&chosen) {
                pushes.push(chosen);
            }
            for n in &pushes {
                let _ = system::set_default(SINK_KEY, Some(&json(n)));
            }
            std::thread::sleep(Duration::from_millis(300));
            if system::default_name("default.audio.sink").as_deref() == Some(SINK_NAME) {
                r.stand_in = true;
            } else {
                // History restore is off in WirePlumber: hold the configured slot.
                r.default_sink.get_or_insert(prev);
                let _ = system::set_default(SINK_KEY, Some(&json(SINK_NAME)));
            }
        }
        // Our own sink was configured (a stale entry): on exit, let WirePlumber choose.
        Some(n) if n == SINK_NAME => {
            r.default_sink.get_or_insert(None);
        }
        _ => {
            r.default_sink.get_or_insert(prev);
        }
    }
    save_restore(r);
}

/// Makes the virtual sink the default (if the speaker was), then keeps the
/// hardware speaker volume pinned only while nothing but neiro feeds it.
fn monitor(target: &str, hw_volume: f32, restore: &Mutex<Restore>, ctl: &Controller, stopping: &AtomicBool) {
    let mut sink_id = None;
    for _ in 0..50 {
        sink_id = system::node_id(SINK_NAME);
        if sink_id.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let Some(sink_id) = sink_id else {
        eprintln!("neiro: virtual sink did not appear");
        return;
    };
    let Some(spk_id) = system::node_id(target) else { return };
    // WirePlumber may already have promoted our new sink to the effective default.
    let eff = system::default_name("default.audio.sink");
    if eff.as_deref() == Some(target) || eff.as_deref() == Some(SINK_NAME) {
        let mut r = restore.lock().unwrap();
        become_default(target, sink_id, spk_id, &mut r);
    }
    eprintln!("neiro: running (sink {SINK_NAME} id {sink_id})");

    let mut out_id = None;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        // Shutdown owns the volumes from here; re-pinning now would undo the restore.
        if stopping.load(Ordering::Relaxed) {
            return;
        }
        if out_id.is_none() {
            out_id = system::node_id("output.neiro");
        }
        let eff = system::default_name("default.audio.sink");
        let speaker_default = eff.as_deref() == Some(target);
        if ctl.routing(speaker_default) && speaker_default {
            become_default(target, sink_id, spk_id, &mut restore.lock().unwrap());
        }
        let Ok(links) = system::links() else { continue };
        let foreign = links.iter().any(|&(o, i)| i == spk_id && Some(o) != out_id);
        let mut r = restore.lock().unwrap();
        let pinned = r.speaker_volume.is_some();
        if pinned && foreign {
            // Something else plays straight to the speaker: give it back its volume right away.
            if let Some((v, m)) = r.speaker_volume.take() {
                let _ = system::set_volume(spk_id, v);
                let _ = system::set_mute(spk_id, m);
                eprintln!("neiro: another stream uses the speaker directly, unpinned volume");
            }
            save_restore(&r);
        } else if !pinned && !foreign && hw_volume > 0.0 {
            // Volume and mute now live on the virtual sink; the amp runs at the calibrated level.
            if let Some((v, m)) = system::get_volume(spk_id) {
                if (v - hw_volume).abs() > 1e-3 || m {
                    r.speaker_volume = Some((v, m));
                    save_restore(&r);
                    let _ = system::set_volume(spk_id, hw_volume);
                    let _ = system::set_mute(spk_id, false);
                }
            }
        }
        // Track the virtual sink so a crash can still hand its volume/mute back.
        let sink_state = system::get_volume(sink_id);
        if sink_state.is_some() && sink_state != r.sink_state {
            r.sink_state = sink_state;
            save_restore(&r);
        }
        let pinned = r.speaker_volume.is_some();
        drop(r);
        if let Some((v, _)) = system::get_volume(spk_id) {
            ctl.set_hw(-system::volume_db(v), pinned);
        }
    }
}
