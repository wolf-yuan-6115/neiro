//! The complete speaker chain and the derived, allocation-free `Config` the
//! audio thread consumes.

use super::bands::{self, BandConfig, BandProcessor, MbcGroupCfg, MAX_GROUPS};
use super::biquad::{butterworth_qs, Coefs, State, BUTTER2_Q};
use super::limiter::{self, Limiter};
use super::vbass::{VbConfig, VirtualBass};
use crate::params::{Params, PersonalEq, PE_BANDS};
use crate::tuning::{Tuning, NBANDS};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Biquads per speaker; an order-8 pass filter uses four of them.
pub const MAX_PEQ: usize = 24;
pub const MAX_BLOCK: usize = 1024;
/// Total processing latency in samples (STFT + limiter lookahead).
pub const LATENCY: usize = bands::LATENCY + limiter::LOOKAHEAD;
const XFADE_MS: f32 = 20.0;

fn db(v: f32) -> f32 {
    10f32.powf(v / 20.0)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub bypass: bool,
    pub pregain: f32,
    pub postgain: f32,
    pub widener: bool,
    pub width: f32,
    pub wide_hp: Coefs,
    /// Speaker PEQ biquads per channel (left, right).
    pub peq: [[Coefs; MAX_PEQ]; 2],
    pub peq_count: [usize; 2],
    pub vb: VbConfig,
    pub bands: BandConfig,
    pub limiter: bool,
    pub ceiling: f32,
    pub lim_rel: f32,
    /// Personal EQ for audition sides A (0) and B (1); identical outside a tuner session.
    pub personal: [PersonalCfg; 2],
    pub side: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PersonalCfg {
    pub enable: bool,
    pub coefs: [Coefs; PE_BANDS],
    /// Linear gain: pre-gain plus any level-match correction.
    pub gain: f32,
}

impl PersonalCfg {
    pub const OFF: PersonalCfg = PersonalCfg { enable: false, coefs: [Coefs::IDENTITY; PE_BANDS], gain: 1.0 };

    pub fn new(eq: &PersonalEq, fs: f32, extra_gain_db: f32) -> PersonalCfg {
        if !eq.enable {
            return PersonalCfg::OFF;
        }
        PersonalCfg { enable: true, coefs: eq.coefs(fs as f64), gain: db(eq.pregain_db + extra_gain_db) }
    }
}

impl Config {
    /// Same personal EQ on both audition sides.
    pub fn with_personal(mut self, eq: &PersonalEq, fs: f32) -> Config {
        let c = PersonalCfg::new(eq, fs, 0.0);
        self.personal = [c, c];
        self.side = 0;
        self
    }

    /// `hw_comp_db` raises the regulator thresholds when the hardware speaker
    /// volume is below 100%, keeping them referenced to the amplifier output.
    pub fn build(p: &Params, t: &Tuning, fs: f32, bypass: bool, hw_comp_db: f32) -> Config {
        let fsd = fs as f64;
        let mut peq = [[Coefs::IDENTITY; MAX_PEQ]; 2];
        let mut peq_count = [0; 2];
        if p.peq.enable {
            let sides = [&p.peq.filters, p.peq.right.as_ref().unwrap_or(&p.peq.filters)];
            for (ch, list) in sides.into_iter().enumerate() {
                let mut push = |c: Coefs| {
                    if peq_count[ch] < MAX_PEQ {
                        peq[ch][peq_count[ch]] = c;
                        peq_count[ch] += 1;
                    }
                };
                for f in list.iter().filter(|f| f.enabled) {
                    let (f0, q, g) = (f.f0 as f64, f.q as f64, f.gain_db as f64);
                    match f.kind.as_str() {
                        "highpass" => butterworth_qs(f.order).for_each(|q| push(Coefs::highpass(fsd, f0, q))),
                        "lowpass" => butterworth_qs(f.order).for_each(|q| push(Coefs::lowpass(fsd, f0, q))),
                        "notch" => push(Coefs::notch(fsd, f0, q)),
                        "lowshelf" => push(Coefs::lowshelf(fsd, f0, q, g)),
                        "highshelf" => push(Coefs::highshelf(fsd, f0, q, g)),
                        "peak" => push(Coefs::peaking(fsd, f0, q, g)),
                        _ => {}
                    }
                }
            }
        }
        let vb = if p.vbass.enable {
            let v = &p.vbass;
            VbConfig::new(
                fsd,
                (v.src_lo, v.src_hi),
                (v.mix_lo, v.mix_hi),
                v.gain_db,
                [v.h2_db, v.h3_db, v.h4_db],
                v.hp_enable.then_some(v.hp_freq),
            )
        } else {
            VbConfig::disabled()
        };

        let mut b = BandConfig::default();
        b.volmax_db = if p.volmax.enable { p.volmax.boost_db } else { 0.0 };
        for i in 0..NBANDS {
            let (ol, or) = if p.optimizer.enable { (p.optimizer.left[i], p.optimizer.right[i]) } else { (0.0, 0.0) };
            b.static_db[0][i] = ol;
            b.static_db[1][i] = or;
        }
        let lb = &p.loudness;
        b.lb_enable = lb.enable;
        b.lb_max_db = lb.max_db.max(0.0);
        // The speaker's own volume lowers the real playback level too.
        b.lb_ref_db = lb.ref_db + hw_comp_db.clamp(0.0, 60.0);
        b.lb_slope = lb.slope.max(0.0);
        b.lb_weight = t.band_freqs.map(|f| 1.0 / (1.0 + (f / lb.corner_hz.max(1.0)).powi(2)));
        b.lb_time_s = lb.time_s;
        b.ieq_enable = p.ieq.enable;
        if let Some(curve) = t.ieq_curves.get(&p.ieq.curve) {
            b.ieq_target = *curve;
        } else {
            b.ieq_enable = false;
        }
        b.ieq_amount = (p.ieq.amount / 16.0).clamp(0.0, 1.0);
        b.ieq_max_db = p.ieq.max_db;
        b.ieq_time_s = p.ieq.time_s;
        b.mbc_enable = p.mbc.enable;
        let mut start = 0;
        for (g, grp) in p.mbc.groups.iter().take(MAX_GROUPS).enumerate() {
            b.mbc[g] = MbcGroupCfg {
                start,
                end: grp.end_band.min(NBANDS),
                threshold_db: grp.threshold_db,
                slope: 1.0 / grp.ratio.max(1.0),
                attack_ms: grp.attack_ms,
                release_ms: grp.release_ms,
                makeup_db: grp.makeup_db,
            };
            start = grp.end_band.min(NBANDS);
            b.mbc_count = g + 1;
        }
        let r = &p.regulator;
        b.reg_enable = r.enable;
        b.reg_threshold = r.threshold.map(|v| v + r.overdrive_db + hw_comp_db.clamp(0.0, 60.0));
        b.reg_knee = r.knee_db.max(0.0);
        b.reg_timbre = r.timbre.clamp(0.0, 1.0);
        b.reg_isolated = r.isolated;
        b.reg_attack_ms = r.attack_ms;
        b.reg_release_ms = r.release_ms;

        Config {
            bypass,
            pregain: db(p.pregain_db),
            postgain: db(p.postgain_db),
            widener: p.widener.enable,
            width: p.widener.width.clamp(0.0, 1.0),
            wide_hp: Coefs::highpass(fsd, 300.0, BUTTER2_Q),
            peq,
            peq_count,
            vb,
            bands: b,
            limiter: p.limiter.enable,
            ceiling: db(p.limiter.ceiling_db),
            lim_rel: (-1.0 / (p.limiter.release_ms.max(1.0) * 1e-3 * fs)).exp(),
            personal: [PersonalCfg::OFF; 2],
            side: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MeterSnapshot {
    pub gr_db: [f32; NBANDS],
    pub ieq_db: [f32; NBANDS],
    pub level_db: [f32; NBANDS],
    pub in_peak_db: f32,
    pub out_peak_db: f32,
    pub limiter_gr_db: f32,
    /// Current bass-loudness boost, dB (0 when the stage is off).
    #[serde(default)]
    pub bass_db: f32,
    /// VU (≈300 ms RMS) in dBFS: in L, in R, out L, out R.
    #[serde(default = "silent4")]
    pub vu_db: [f32; 4],
    /// Sample peak in dBFS since the previous read, same order as `vu_db`.
    #[serde(default = "silent4")]
    pub peak_db: [f32; 4],
}

impl Default for MeterSnapshot {
    fn default() -> Self {
        MeterSnapshot {
            gr_db: [0.0; NBANDS],
            ieq_db: [0.0; NBANDS],
            level_db: [-120.0; NBANDS],
            in_peak_db: -120.0,
            out_peak_db: -120.0,
            limiter_gr_db: 0.0,
            bass_db: 0.0,
            vu_db: silent4(),
            peak_db: silent4(),
        }
    }
}

/// Older daemons don't send VU data; show silence rather than full scale.
fn silent4() -> [f32; 4] {
    [-120.0; 4]
}

fn to_db(v: f32) -> f32 {
    20.0 * v.max(1e-6).log10()
}

/// Lock-free meter mailbox written by the audio thread.
pub struct SharedMeters {
    vals: Vec<AtomicU32>,
    /// Linear peaks, max-accumulated until a reader takes them.
    peaks: [AtomicU32; 4],
    /// Bumped by every store.
    seq: AtomicU64,
    /// Reader side only: the last `seq` seen and when it changed.
    seen: Mutex<(u64, Instant)>,
}

/// With nothing playing into it, PipeWire pauses neiro's streams and the audio
/// thread stops storing: meters older than this read as silence, not frozen.
const METERS_STALE: Duration = Duration::from_millis(250);

impl Default for SharedMeters {
    fn default() -> Self {
        let m = SharedMeters {
            vals: (0..NBANDS * 3 + 2 + 4).map(|_| AtomicU32::new(0)).collect(),
            peaks: Default::default(),
            seq: AtomicU64::new(0),
            seen: Mutex::new((0, Instant::now())),
        };
        // Start at silence: all-zero bits would read as 0 dBFS until audio runs.
        m.store(&MeterSnapshot::default(), &[0.0; 4]);
        m
    }
}

impl SharedMeters {
    /// `peaks` are linear block peaks; non-negative f32 bit patterns order like
    /// integers, so `fetch_max` on the bits keeps the true maximum.
    pub fn store(&self, m: &MeterSnapshot, peaks: &[f32; 4]) {
        let tail = [m.limiter_gr_db, m.bass_db];
        let it = m.gr_db.iter().chain(&m.ieq_db).chain(&m.level_db).chain(tail.iter()).chain(&m.vu_db);
        for (slot, v) in self.vals.iter().zip(it) {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
        for (slot, p) in self.peaks.iter().zip(peaks) {
            slot.fetch_max(p.abs().to_bits(), Ordering::Relaxed);
        }
        self.seq.fetch_add(1, Ordering::Release);
    }

    pub fn load(&self) -> MeterSnapshot {
        let seq = self.seq.load(Ordering::Acquire);
        let stale = {
            let mut seen = self.seen.lock().unwrap();
            if seen.0 != seq {
                *seen = (seq, Instant::now());
            }
            seen.1.elapsed() > METERS_STALE
        };
        if stale {
            for slot in &self.peaks {
                slot.store(0, Ordering::Relaxed);
            }
            return MeterSnapshot::default();
        }
        let v: Vec<f32> = self.vals.iter().map(|a| f32::from_bits(a.load(Ordering::Relaxed))).collect();
        let mut m = MeterSnapshot::default();
        m.gr_db.copy_from_slice(&v[0..NBANDS]);
        m.ieq_db.copy_from_slice(&v[NBANDS..2 * NBANDS]);
        m.level_db.copy_from_slice(&v[2 * NBANDS..3 * NBANDS]);
        m.limiter_gr_db = v[3 * NBANDS];
        m.bass_db = v[3 * NBANDS + 1];
        m.vu_db.copy_from_slice(&v[3 * NBANDS + 2..3 * NBANDS + 6]);
        for (k, slot) in self.peaks.iter().enumerate() {
            m.peak_db[k] = to_db(f32::from_bits(slot.swap(0, Ordering::Relaxed)));
        }
        m.in_peak_db = m.peak_db[0].max(m.peak_db[1]);
        m.out_peak_db = m.peak_db[2].max(m.peak_db[3]);
        m
    }
}

/// Two personal-EQ chains run in parallel and are crossfaded, so A/B switches are click-free.
pub struct PersonalChain {
    st: [[[State; PE_BANDS]; 2]; 2],
    /// 0 = side A, 1 = side B.
    mix: f32,
    step: f32,
}

impl PersonalChain {
    fn new(fs: f32) -> PersonalChain {
        PersonalChain { st: Default::default(), mix: 0.0, step: 1.0 / (XFADE_MS * 1e-3 * fs) }
    }

    #[inline]
    fn run(st: &mut [[State; PE_BANDS]; 2], c: &PersonalCfg, l: f32, r: f32) -> (f32, f32) {
        if !c.enable {
            return (l, r);
        }
        let (mut a, mut b) = (l * c.gain, r * c.gain);
        for k in 0..PE_BANDS {
            a = st[0][k].run(&c.coefs[k], a);
            b = st[1][k].run(&c.coefs[k], b);
        }
        (a, b)
    }

    fn process(&mut self, cfg: &Config, l: &mut [f32], r: &mut [f32]) {
        let [pa, pb] = &cfg.personal;
        let target = if cfg.side == 1 { 1.0 } else { 0.0 };
        if pa == pb && self.mix == 0.0 {
            if pa.enable {
                if pa.gain != 1.0 {
                    for i in 0..l.len() {
                        l[i] *= pa.gain;
                        r[i] *= pa.gain;
                    }
                }
                let [sl, sr] = &mut self.st[0];
                for k in 0..PE_BANDS {
                    let c = &pa.coefs[k];
                    // A 0 dB band at rest is an exact pass-through.
                    if c.is_unity() && sl[k].is_zero() && sr[k].is_zero() {
                        continue;
                    }
                    for i in 0..l.len() {
                        l[i] = sl[k].run(c, l[i]);
                        r[i] = sr[k].run(c, r[i]);
                    }
                }
            }
            return;
        }
        let [sa, sb] = &mut self.st;
        for i in 0..l.len() {
            let (al, ar) = Self::run(sa, pa, l[i], r[i]);
            let (bl, br) = Self::run(sb, pb, l[i], r[i]);
            if self.mix != target {
                self.mix = if target > self.mix { (self.mix + self.step).min(1.0) } else { (self.mix - self.step).max(0.0) };
            }
            l[i] = al + (bl - al) * self.mix;
            r[i] = ar + (br - ar) * self.mix;
        }
    }
}

pub struct Engine {
    cfg: Config,
    personal: PersonalChain,
    wide: State,
    peq: [[State; MAX_PEQ]; 2],
    vb: VirtualBass,
    bands: BandProcessor,
    lim: Limiter,
    dry: [Vec<f32>; 2],
    dry_idx: usize,
    /// 0 = fully processed, 1 = fully bypassed.
    mix: f32,
    mix_step: f32,
    buf: [Vec<f32>; 2],
    fs: f32,
    /// Mean-square envelopes for the VU meters (in L/R, out L/R).
    vu_ms: [f32; 4],
    pub meters: MeterSnapshot,
    /// Linear peaks of the last `process` call (in L/R, out L/R).
    pub peaks: [f32; 4],
}

impl Engine {
    pub fn new(fs: f32, t: &Tuning, cfg: Config) -> Engine {
        let mut bands = BandProcessor::new(fs, &t.band_freqs);
        bands.set_config(&cfg.bands);
        Engine {
            wide: State::default(),
            peq: [[State::default(); MAX_PEQ]; 2],
            vb: VirtualBass::new(fs),
            personal: PersonalChain::new(fs),
            bands,
            lim: Limiter::new(),
            dry: [vec![0.0; LATENCY], vec![0.0; LATENCY]],
            dry_idx: 0,
            mix: if cfg.bypass { 1.0 } else { 0.0 },
            mix_step: 1.0 / (XFADE_MS * 1e-3 * fs),
            buf: [vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]],
            fs,
            vu_ms: [0.0; 4],
            meters: MeterSnapshot::default(),
            peaks: [0.0; 4],
            cfg,
        }
    }

    pub fn set_config(&mut self, cfg: &Config) {
        for ch in 0..2 {
            if cfg.peq_count[ch] > self.cfg.peq_count[ch] {
                for s in &mut self.peq[ch][self.cfg.peq_count[ch]..cfg.peq_count[ch]] {
                    s.reset();
                }
            }
        }
        self.cfg = *cfg;
        self.bands.set_config(&cfg.bands);
    }

    /// Process any number of frames. Input and output slices must be the same length.
    pub fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let mut peak = [0f32; 4];
        let mut sumsq = [0f32; 4];
        for start in (0..in_l.len()).step_by(MAX_BLOCK) {
            let end = (start + MAX_BLOCK).min(in_l.len());
            let n = end - start;
            let [bl, br] = &mut self.buf;
            let (l, r) = (&mut bl[..n], &mut br[..n]);
            l.copy_from_slice(&in_l[start..end]);
            r.copy_from_slice(&in_r[start..end]);
            for i in 0..n {
                peak[0] = peak[0].max(l[i].abs());
                peak[1] = peak[1].max(r[i].abs());
                sumsq[0] += l[i] * l[i];
                sumsq[1] += r[i] * r[i];
            }
            run_wet(&self.cfg, &mut self.wide, &mut self.peq, &mut self.vb, &mut self.personal, &mut self.bands, &mut self.lim, l, r);

            let target = if self.cfg.bypass { 1.0 } else { 0.0 };
            let (out_l, out_r) = (&mut out_l[start..end], &mut out_r[start..end]);
            if self.mix == target {
                // Settled on one path: block copies, up to the wrap of the dry delay line.
                let mut i = 0;
                while i < n {
                    let (a, m) = (self.dry_idx, (n - i).min(LATENCY - self.dry_idx));
                    for (c, (input, out, wet)) in [(in_l, &mut *out_l, &*l), (in_r, &mut *out_r, &*r)].into_iter().enumerate() {
                        let delayed = &mut self.dry[c][a..a + m];
                        out[i..i + m].copy_from_slice(if target == 1.0 { delayed } else { &wet[i..i + m] });
                        delayed.copy_from_slice(&input[start + i..start + i + m]);
                    }
                    self.dry_idx = (a + m) % LATENCY;
                    i += m;
                }
            } else {
                for i in 0..n {
                    let (dl, dr) = (self.dry[0][self.dry_idx], self.dry[1][self.dry_idx]);
                    self.dry[0][self.dry_idx] = in_l[start + i];
                    self.dry[1][self.dry_idx] = in_r[start + i];
                    self.dry_idx = (self.dry_idx + 1) % LATENCY;
                    if self.mix != target {
                        self.mix = if target > self.mix {
                            (self.mix + self.mix_step).min(1.0)
                        } else {
                            (self.mix - self.mix_step).max(0.0)
                        };
                    }
                    (out_l[i], out_r[i]) = if self.mix >= 1.0 {
                        (dl, dr)
                    } else if self.mix <= 0.0 {
                        (l[i], r[i])
                    } else {
                        (l[i] + (dl - l[i]) * self.mix, r[i] + (dr - r[i]) * self.mix)
                    };
                }
            }
            for i in 0..n {
                peak[2] = peak[2].max(out_l[i].abs());
                peak[3] = peak[3].max(out_r[i].abs());
                sumsq[2] += out_l[i] * out_l[i];
                sumsq[3] += out_r[i] * out_r[i];
            }
        }
        let n = in_l.len().max(1) as f32;
        let a = (-n / (0.3 * self.fs)).exp();
        for k in 0..4 {
            self.vu_ms[k] = a * self.vu_ms[k] + (1.0 - a) * sumsq[k] / n;
        }
        self.peaks = peak;
        let bm = &self.bands.meters;
        self.meters = MeterSnapshot {
            gr_db: bm.gr_db,
            ieq_db: bm.ieq_db,
            level_db: bm.level_db,
            in_peak_db: to_db(peak[0].max(peak[1])),
            out_peak_db: to_db(peak[2].max(peak[3])),
            limiter_gr_db: self.lim.gr_db,
            bass_db: bm.bass_db,
            vu_db: self.vu_ms.map(|ms| 10.0 * ms.max(1e-12).log10()),
            peak_db: peak.map(to_db),
        };
    }
}

#[allow(clippy::too_many_arguments)]
fn run_wet(
    cfg: &Config,
    wide: &mut State,
    peq: &mut [[State; MAX_PEQ]; 2],
    vb: &mut VirtualBass,
    personal: &mut PersonalChain,
    bands: &mut BandProcessor,
    lim: &mut Limiter,
    l: &mut [f32],
    r: &mut [f32],
) {
    let n = l.len();
    if cfg.pregain != 1.0 {
        for i in 0..n {
            l[i] *= cfg.pregain;
            r[i] *= cfg.pregain;
        }
    }
    if cfg.widener && cfg.width > 0.0 {
        for i in 0..n {
            let m = 0.5 * (l[i] + r[i]);
            let s = 0.5 * (l[i] - r[i]);
            let s = s + cfg.width * wide.run(&cfg.wide_hp, s);
            l[i] = m + s;
            r[i] = m - s;
        }
    }
    // Both channels in one loop: two independent recursions run faster than one.
    let both = cfg.peq_count[0].min(cfg.peq_count[1]);
    let [pl, pr] = peq;
    for k in 0..both {
        let (cl, cr) = (&cfg.peq[0][k], &cfg.peq[1][k]);
        for i in 0..n {
            l[i] = pl[k].run(cl, l[i]);
            r[i] = pr[k].run(cr, r[i]);
        }
    }
    for (ch, buf, st) in [(0, &mut *l, &mut *pl), (1, &mut *r, &mut *pr)] {
        for k in both..cfg.peq_count[ch] {
            let c = &cfg.peq[ch][k];
            for x in buf.iter_mut() {
                *x = st[k].run(c, *x);
            }
        }
    }
    if cfg.vb.enable {
        vb.process(&cfg.vb, l, r);
    }
    // Before the band stage so the regulator and limiter still guard the speaker.
    personal.process(cfg, l, r);
    bands.process(l, r);
    if cfg.postgain != 1.0 {
        for i in 0..n {
            l[i] *= cfg.postgain;
            r[i] *= cfg.postgain;
        }
    }
    let ceiling = if cfg.limiter { cfg.ceiling } else { f32::MAX };
    lim.process(ceiling, cfg.lim_rel, l, r);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meters_read_silence_until_audio_runs_and_after_it_stops() {
        let m = SharedMeters::default();
        let fresh = m.load();
        assert_eq!(fresh.vu_db, [-120.0; 4], "a new meter must not read 0 dBFS");
        assert!(fresh.level_db.iter().all(|&l| l <= -120.0));
        let mut snap = MeterSnapshot::default();
        snap.vu_db = [-12.0; 4];
        m.store(&snap, &[0.5; 4]);
        let live = m.load();
        assert_eq!(live.vu_db, [-12.0; 4]);
        assert!((live.peak_db[0] - -6.02).abs() < 0.01);
        // The stream paused: no stores, so the meters fall to silence.
        std::thread::sleep(METERS_STALE + Duration::from_millis(50));
        assert_eq!(m.load().vu_db, [-120.0; 4]);
    }
    use crate::params::factory;
    use crate::tuning::test_tuning;

    fn signal(n: usize) -> Vec<f32> {
        let mut seed = 1u32;
        (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                0.3 * noise + 0.4 * (i as f32 * 0.01).sin()
            })
            .collect()
    }

    #[test]
    fn audition_crossfade_is_smooth() {
        let t = test_tuning();
        let p = factory(&t, "off").unwrap();
        let mut a = PersonalEq::default();
        let mut b = PersonalEq::default();
        a.bands[0].gain_db = 0.0;
        b.bands[0].gain_db = 9.0;
        let base = Config::build(&p, &t, 48000.0, false, 0.0);
        let mut cfg = base;
        cfg.personal = [PersonalCfg::new(&a, 48000.0, 0.0), PersonalCfg::new(&b, 48000.0, 0.0)];
        let mut e = Engine::new(48000.0, &t, cfg);
        let x: Vec<f32> = (0..96000).map(|i| 0.3 * (2.0 * std::f32::consts::PI * 80.0 * i as f32 / 48000.0).sin()).collect();
        let (mut ol, mut or) = (vec![0.0; x.len()], vec![0.0; x.len()]);
        e.process(&x[..48000], &x[..48000], &mut ol[..48000], &mut or[..48000]);
        cfg.side = 1;
        e.set_config(&cfg);
        e.process(&x[48000..], &x[48000..], &mut ol[48000..], &mut or[48000..]);
        let max_step = ol.windows(2).skip(LATENCY + 4800).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        // An 80 Hz sine at <= ~0.85 amplitude moves at most ~0.009 per sample; a click would be far larger.
        assert!(max_step < 0.02, "max step {max_step}");
        let before = ol[30000..40000].iter().fold(0f32, |m, v| m.max(v.abs()));
        let after = ol[80000..90000].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(after > before * 2.0, "side B (+9 dB shelf) should be louder: {before} -> {after}");
    }

    #[test]
    fn identical_sides_match_single_chain() {
        let t = test_tuning();
        let p = factory(&t, "music").unwrap();
        let mut eq = PersonalEq::default();
        eq.bands[1].gain_db = 4.0;
        let cfg = Config::build(&p, &t, 48000.0, false, 0.0).with_personal(&eq, 48000.0);
        let x = signal(20000);
        let mut outs = vec![];
        for side in [0, 1] {
            let mut c = cfg;
            c.personal[1] = c.personal[0];
            c.side = side;
            let mut e = Engine::new(48000.0, &t, c);
            let (mut ol, mut or) = (vec![0.0; x.len()], vec![0.0; x.len()]);
            e.process(&x, &x, &mut ol, &mut or);
            outs.push(ol);
        }
        // Side 1 with identical configs crossfades between identical signals.
        let err = outs[0].iter().zip(&outs[1]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(err < 1e-6, "{err}");
    }

    #[test]
    fn shared_meters_keep_max_peak_until_read() {
        let m = SharedMeters::default();
        let snap = MeterSnapshot::default();
        m.store(&snap, &[0.5, 0.1, 0.0, 0.0]);
        m.store(&snap, &[0.2, 0.9, 0.0, 0.0]);
        let r = m.load();
        assert!((r.peak_db[0] - to_db(0.5)).abs() < 1e-4 && (r.peak_db[1] - to_db(0.9)).abs() < 1e-4);
        assert!(m.load().peak_db[0] <= -119.0, "peaks reset after read");
    }

    #[test]
    fn bypass_is_bit_exact_delay() {
        let t = test_tuning();
        let p = factory(&t, "music").unwrap();
        let mut e = Engine::new(48000.0, &t, Config::build(&p, &t, 48000.0, true, 0.0));
        let x = signal(20000);
        let (mut ol, mut or) = (vec![0.0; x.len()], vec![0.0; x.len()]);
        e.process(&x, &x, &mut ol, &mut or);
        for i in LATENCY..x.len() {
            assert_eq!(ol[i], x[i - LATENCY]);
        }
    }

    #[test]
    fn all_profiles_finite_and_bounded() {
        let t = test_tuning();
        for name in t.profile_names() {
            let mut p = factory(&t, &name).unwrap();
            p.vbass.enable = true;
            let mut e = Engine::new(48000.0, &t, Config::build(&p, &t, 48000.0, false, 0.0));
            let mut x = signal(48000);
            x[1000] = 1.0; // impulse
            x[20000..30000].fill(0.0); // silence
            let (mut ol, mut or) = (vec![0.0; x.len()], vec![0.0; x.len()]);
            // odd block sizes like PipeWire quanta
            let mut s = 0;
            for blk in [128usize, 1024, 333, 2048].iter().cycle() {
                if s >= x.len() {
                    break;
                }
                let e2 = (s + blk).min(x.len());
                e.process(&x[s..e2], &x[s..e2], &mut ol[s..e2], &mut or[s..e2]);
                s = e2;
            }
            assert!(ol.iter().chain(&or).all(|v| v.is_finite()), "{name}");
            if p.limiter.enable {
                let peak = ol.iter().chain(&or).fold(0f32, |m, v| m.max(v.abs()));
                assert!(peak <= 10f32.powf(p.limiter.ceiling_db / 20.0) * 1.001, "{name} peak {peak}");
            }
        }
    }
}
