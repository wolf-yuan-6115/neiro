//! 20-band STFT processor: static EQ (audio optimizer), adaptive IEQ, volmax,
//! multiband compressor and the per-band regulator, all applied as smooth
//! per-bin gains in one WOLA filterbank (sqrt-Hann, N=1024, hop=256).

use crate::tuning::NBANDS;
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub const FFT_N: usize = 1024;
pub const HOP: usize = FFT_N / 4;
/// End-to-end latency in samples of the band processor.
pub const LATENCY: usize = FFT_N;
pub const MAX_GROUPS: usize = 4;
/// dB to log2 of the linear amplitude gain: 10^(dB/20) = 2^(dB * log2(10) / 20).
const DB_TO_LOG2: f32 = std::f32::consts::LOG2_10 / 20.0;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MbcGroupCfg {
    pub start: usize,
    pub end: usize,
    pub threshold_db: f32,
    /// 1/ratio
    pub slope: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandConfig {
    /// Static per-channel gains (audio optimizer) in dB, seen by the dynamics.
    pub static_db: [[f32; NBANDS]; 2],
    /// Volmax boost in dB, applied *after* the compressor and regulator: Dolby
    /// limits at the decoded threshold and then adds the boost (measured on
    /// Windows by speaker-tuning-to-easyeffects), so the boost must not push
    /// the bands into limiting earlier.
    pub volmax_db: f32,
    /// Bass loudness: boost `weight[b] * min(max, slope * (ref - level))` dB,
    /// where `level` is the smoothed level of the bands above 200 Hz.
    pub lb_enable: bool,
    pub lb_max_db: f32,
    pub lb_ref_db: f32,
    pub lb_slope: f32,
    pub lb_weight: [f32; NBANDS],
    pub lb_time_s: f32,
    pub ieq_enable: bool,
    pub ieq_target: [f32; NBANDS],
    /// 0..1
    pub ieq_amount: f32,
    pub ieq_max_db: f32,
    pub ieq_time_s: f32,
    pub mbc_enable: bool,
    pub mbc: [MbcGroupCfg; MAX_GROUPS],
    pub mbc_count: usize,
    pub reg_enable: bool,
    pub reg_threshold: [f32; NBANDS],
    pub reg_knee: f32,
    pub reg_timbre: f32,
    pub reg_isolated: [bool; NBANDS],
    pub reg_attack_ms: f32,
    pub reg_release_ms: f32,
}

impl Default for BandConfig {
    fn default() -> Self {
        BandConfig {
            static_db: [[0.0; NBANDS]; 2],
            volmax_db: 0.0,
            lb_enable: false,
            lb_max_db: 0.0,
            lb_ref_db: 0.0,
            lb_slope: 0.0,
            lb_weight: [0.0; NBANDS],
            lb_time_s: 2.0,
            ieq_enable: false,
            ieq_target: [0.0; NBANDS],
            ieq_amount: 0.0,
            ieq_max_db: 0.0,
            ieq_time_s: 3.0,
            mbc_enable: false,
            mbc: [MbcGroupCfg::default(); MAX_GROUPS],
            mbc_count: 0,
            reg_enable: false,
            reg_threshold: [0.0; NBANDS],
            reg_knee: 12.0,
            reg_timbre: 0.0,
            reg_isolated: [false; NBANDS],
            reg_attack_ms: 5.0,
            reg_release_ms: 150.0,
        }
    }
}

/// Regulator static curve: soft knee starting `knee` dB below `thr`, output
/// never exceeding `thr`. Returns gain change in dB (<= 0).
#[inline]
pub fn regulator_gain(level: f32, thr: f32, knee: f32) -> f32 {
    let lo = thr - knee;
    if level <= lo {
        0.0
    } else if knee > 0.0 && level < lo + 2.0 * knee {
        let d = level - lo;
        -(d * d) / (4.0 * knee)
    } else {
        thr - level
    }
}

/// Bands used to fit spectral tilt (141 Hz .. 11 kHz); the extremes are too
/// content-dependent to steer the fit.
const TILT_BANDS: std::ops::RangeInclusive<usize> = 1..=17;

/// Residual of `v` after removing its least-squares line over log-frequency.
fn detrend(v: &[f32; NBANDS], x: &[f32; NBANDS]) -> [f32; NBANDS] {
    let n = TILT_BANDS.count() as f32;
    let mx = TILT_BANDS.map(|b| x[b]).sum::<f32>() / n;
    let my = TILT_BANDS.map(|b| v[b]).sum::<f32>() / n;
    let sxy: f32 = TILT_BANDS.map(|b| (x[b] - mx) * (v[b] - my)).sum();
    let sxx: f32 = TILT_BANDS.map(|b| (x[b] - mx) * (x[b] - mx)).sum();
    let slope = sxy / sxx;
    std::array::from_fn(|b| v[b] - (my + slope * (x[b] - mx)))
}

/// IEQ: steer the *shape* of the long-term content spectrum toward the Dolby
/// target. Overall tilt is removed from both first: the targets are nearly flat
/// while raw music band energy falls ~4 dB/octave, so comparing them directly
/// would cut bass and boost treble on every song.
pub fn ieq_correction(
    content_db: &[f32; NBANDS],
    target_db: &[f32; NBANDS],
    log_f: &[f32; NBANDS],
    max_db: f32,
    amount: f32,
) -> [f32; NBANDS] {
    let c = detrend(content_db, log_f);
    let t = detrend(target_db, log_f);
    let loudest = content_db.iter().cloned().fold(f32::MIN, f32::max);
    std::array::from_fn(|b| {
        // Don't chase bands with essentially no content.
        if content_db[b] < loudest - 50.0 {
            0.0
        } else {
            (t[b] - c[b]).clamp(-max_db, max_db) * amount
        }
    })
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BandMeters {
    /// Dynamic gain reduction (mbc + regulator) per band, dB <= 0.
    pub gr_db: [f32; NBANDS],
    /// Current IEQ correction per band, dB.
    pub ieq_db: [f32; NBANDS],
    /// Detected input level per band, dBFS.
    pub level_db: [f32; NBANDS],
    /// Current bass-loudness boost before the per-band shelf weighting, dB.
    pub bass_db: f32,
}

pub struct BandProcessor {
    fs: f32,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    window: Vec<f32>,
    inbuf: [Vec<f32>; 2],
    outbuf: [Vec<f32>; 2],
    pos: usize,
    frame: Vec<f32>,
    spec: [Vec<Complex<f32>>; 2],
    scratch_f: Vec<Complex<f32>>,
    scratch_i: Vec<Complex<f32>>,
    bin_band: Vec<usize>,
    bin_interp: Vec<(usize, usize, f32)>,
    bin_gain: Vec<f32>,
    log_f: [f32; NBANDS],
    ref_power: f32,
    ieq_avg: [f64; NBANDS],
    ieq_primed: bool,
    /// Bands that measure the playback level for bass loudness.
    lb_detect: [bool; NBANDS],
    /// Smoothed playback level, dB; `None` until there has been signal.
    lb_level: Option<f32>,
    mbc_env: [f32; MAX_GROUPS],
    reg_env: [f32; NBANDS],
    cfg: BandConfig,
    pub meters: BandMeters,
}

impl BandProcessor {
    pub fn new(fs: f32, band_freqs: &[f32; NBANDS]) -> BandProcessor {
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(FFT_N);
        let inv = planner.plan_fft_inverse(FFT_N);
        // Periodic sqrt-Hann: analysis * synthesis = Hann, which sums to 2 at 75% overlap.
        let window: Vec<f32> = (0..FFT_N)
            .map(|i| (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT_N as f32).cos()).sqrt())
            .collect();
        let nbins = FFT_N / 2 + 1;
        let mut bin_band = vec![0; nbins];
        let mut bin_interp = vec![(0, 0, 0.0); nbins];
        let edges: Vec<f32> = (0..NBANDS - 1).map(|b| (band_freqs[b] * band_freqs[b + 1]).sqrt()).collect();
        for k in 0..nbins {
            let f = k as f32 * fs / FFT_N as f32;
            bin_band[k] = edges.iter().filter(|&&e| f >= e).count();
            bin_interp[k] = if f <= band_freqs[0] {
                (0, 0, 0.0)
            } else if f >= band_freqs[NBANDS - 1] {
                (NBANDS - 1, NBANDS - 1, 0.0)
            } else {
                let b = (0..NBANDS - 1).find(|&b| f < band_freqs[b + 1]).unwrap();
                let frac = (f / band_freqs[b]).ln() / (band_freqs[b + 1] / band_freqs[b]).ln();
                (b, b + 1, frac)
            };
        }
        let scratch_len = fwd.get_scratch_len().max(inv.get_scratch_len());
        let mut p = BandProcessor {
            fs,
            window,
            inbuf: [vec![0.0; FFT_N], vec![0.0; FFT_N]],
            outbuf: [vec![0.0; FFT_N], vec![0.0; FFT_N]],
            pos: 0,
            frame: vec![0.0; FFT_N],
            spec: [fwd.make_output_vec(), fwd.make_output_vec()],
            scratch_f: vec![Complex::default(); scratch_len],
            scratch_i: vec![Complex::default(); scratch_len],
            fwd,
            inv,
            bin_band,
            bin_interp,
            bin_gain: vec![1.0; nbins],
            log_f: band_freqs.map(|f| f.log2()),
            ref_power: 1.0,
            ieq_avg: [0.0; NBANDS],
            ieq_primed: false,
            lb_detect: band_freqs.map(|f| f >= 200.0),
            lb_level: None,
            mbc_env: [0.0; MAX_GROUPS],
            reg_env: [0.0; NBANDS],
            cfg: BandConfig::default(),
            meters: BandMeters::default(),
        };
        // Calibrate: a full-scale sine centred on a bin reads 0 dB.
        let k = 64.0;
        for i in 0..FFT_N {
            p.frame[i] = (2.0 * std::f32::consts::PI * k * i as f32 / FFT_N as f32).sin() * p.window[i];
        }
        p.fwd.process_with_scratch(&mut p.frame, &mut p.spec[0], &mut p.scratch_f).unwrap();
        p.ref_power = p.spec[0].iter().map(|c| c.norm_sqr()).sum();
        p.spec[0].iter_mut().for_each(|c| *c = Complex::default());
        p
    }

    pub fn set_config(&mut self, cfg: &BandConfig) {
        self.cfg = *cfg;
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let base = FFT_N - HOP;
        let mut i = 0;
        while i < l.len() {
            // Up to the next hop boundary at a time.
            let n = (HOP - self.pos).min(l.len() - i);
            for (c, x) in [&mut *l, &mut *r].into_iter().enumerate() {
                let x = &mut x[i..i + n];
                self.inbuf[c][base + self.pos..base + self.pos + n].copy_from_slice(x);
                x.copy_from_slice(&self.outbuf[c][self.pos..self.pos + n]);
            }
            self.pos += n;
            i += n;
            if self.pos == HOP {
                self.pos = 0;
                self.frame();
            }
        }
    }

    fn frame(&mut self) {
        let dt = HOP as f32 / self.fs;
        let cfg = self.cfg;

        // Analysis.
        let mut power = [[0f32; NBANDS]; 2];
        for c in 0..2 {
            for i in 0..FFT_N {
                self.frame[i] = self.inbuf[c][i] * self.window[i];
            }
            self.fwd.process_with_scratch(&mut self.frame, &mut self.spec[c], &mut self.scratch_f).unwrap();
            for (k, x) in self.spec[c].iter().enumerate() {
                power[c][self.bin_band[k]] += x.norm_sqr();
            }
        }
        let mut level = [0f32; NBANDS];
        for b in 0..NBANDS {
            level[b] = 10.0 * (power[0][b].max(power[1][b]) / self.ref_power + 1e-14).log10();
        }
        self.meters.level_db = level;

        // Adaptive IEQ: steer the long-term band spectrum toward the target curve.
        let mut ieq = [0f32; NBANDS];
        if cfg.ieq_enable && cfg.ieq_amount > 0.0 {
            let total: f32 = (0..NBANDS).map(|b| power[0][b] + power[1][b]).sum();
            if total / self.ref_power > 1e-7 {
                let a = (-(dt / cfg.ieq_time_s.max(0.05)) as f64).exp();
                for b in 0..NBANDS {
                    let p = (power[0][b] + power[1][b]) as f64;
                    self.ieq_avg[b] = if self.ieq_primed { a * self.ieq_avg[b] + (1.0 - a) * p } else { p };
                }
                self.ieq_primed = true;
            }
            if self.ieq_primed {
                let avg_db: [f32; NBANDS] =
                    std::array::from_fn(|b| 10.0 * ((self.ieq_avg[b] as f32 / self.ref_power) + 1e-14).log10());
                ieq = ieq_correction(&avg_db, &cfg.ieq_target, &self.log_f, cfg.ieq_max_db, cfg.ieq_amount);
            }
        }
        self.meters.ieq_db = ieq;

        // Bass loudness: follow the playback level, rising faster than falling so
        // a loud entrance isn't over-boosted for long. Pauses hold the last level.
        let mut lb = [0f32; NBANDS];
        self.meters.bass_db = 0.0;
        if cfg.lb_enable {
            let p: f32 = (0..NBANDS).filter(|&b| self.lb_detect[b]).map(|b| power[0][b].max(power[1][b])).sum();
            let now = 10.0 * (p / self.ref_power + 1e-14).log10();
            if now > -70.0 {
                self.lb_level = Some(match self.lb_level {
                    None => now,
                    Some(prev) => {
                        let tau = if now > prev { cfg.lb_time_s * 0.25 } else { cfg.lb_time_s };
                        let a = (-dt / tau.max(0.05)).exp();
                        now + a * (prev - now)
                    }
                });
            }
            if let Some(lvl) = self.lb_level {
                let boost = ((cfg.lb_ref_db - lvl) * cfg.lb_slope).clamp(0.0, cfg.lb_max_db);
                self.meters.bass_db = boost;
                for b in 0..NBANDS {
                    lb[b] = boost * cfg.lb_weight[b];
                }
            }
        }

        // Level each dynamics stage sees, after static EQ, IEQ and bass loudness.
        let mut det = [0f32; NBANDS];
        for b in 0..NBANDS {
            det[b] = level[b] + 0.5 * (cfg.static_db[0][b] + cfg.static_db[1][b]) + ieq[b] + lb[b];
        }

        let att = |ms: f32| (-dt / (ms.max(0.1) * 1e-3)).exp();
        let mut mbc_db = [0f32; NBANDS];
        let mut mbc_gr = [0f32; NBANDS];
        if cfg.mbc_enable {
            for g in 0..cfg.mbc_count {
                let grp = cfg.mbc[g];
                let (s, e) = (grp.start.min(NBANDS), grp.end.min(NBANDS));
                if s >= e {
                    continue;
                }
                let lin: f32 = (s..e).map(|b| 10f32.powf(det[b] / 10.0)).sum();
                let lg = 10.0 * (lin + 1e-14).log10();
                let target = if lg > grp.threshold_db { (lg - grp.threshold_db) * (grp.slope - 1.0) } else { 0.0 };
                let a = if target < self.mbc_env[g] { att(grp.attack_ms) } else { att(grp.release_ms) };
                self.mbc_env[g] = target + a * (self.mbc_env[g] - target);
                for b in s..e {
                    mbc_gr[b] = self.mbc_env[g];
                    mbc_db[b] = self.mbc_env[g] + grp.makeup_db;
                }
            }
        } else {
            self.mbc_env = [0.0; MAX_GROUPS];
        }

        let mut reg_db = [0f32; NBANDS];
        if cfg.reg_enable {
            let mut target = [0f32; NBANDS];
            for b in 0..NBANDS {
                target[b] = regulator_gain(det[b] + mbc_db[b], cfg.reg_threshold[b], cfg.reg_knee);
            }
            // Timbre preservation: spread reduction of non-isolated bands to their non-isolated neighbours.
            let raw = target;
            if cfg.reg_timbre > 0.0 {
                for b in 0..NBANDS {
                    if cfg.reg_isolated[b] {
                        continue;
                    }
                    for j in 0..NBANDS {
                        if j == b || cfg.reg_isolated[j] {
                            continue;
                        }
                        let d = (b as i32 - j as i32).unsigned_abs() as i32;
                        let shared = raw[j] * cfg.reg_timbre * 0.5f32.powi(d - 1);
                        target[b] = target[b].min(shared);
                    }
                }
            }
            for b in 0..NBANDS {
                let a = if target[b] < self.reg_env[b] { att(cfg.reg_attack_ms) } else { att(cfg.reg_release_ms) };
                self.reg_env[b] = target[b] + a * (self.reg_env[b] - target[b]);
                reg_db[b] = self.reg_env[b];
            }
        } else {
            self.reg_env = [0.0; NBANDS];
        }
        for b in 0..NBANDS {
            self.meters.gr_db[b] = mbc_gr[b] + reg_db[b];
        }

        // Synthesis.
        let scale = 1.0 / (FFT_N as f32 * 2.0);
        let totals: [[f32; NBANDS]; 2] = std::array::from_fn(|c| {
            std::array::from_fn(|b| cfg.static_db[c][b] + ieq[b] + lb[b] + mbc_db[b] + reg_db[b] + cfg.volmax_db)
        });
        for c in 0..2 {
            let total = &totals[c];
            // 0 dB everywhere is a gain of exactly 1.
            if total.iter().any(|&d| d != 0.0) {
                // The right channel usually gets the same gains as the left.
                if c == 0 || totals[1] != totals[0] {
                    for (k, &(b0, b1, fr)) in self.bin_interp.iter().enumerate() {
                        let db = total[b0] + (total[b1] - total[b0]) * fr;
                        self.bin_gain[k] = (db * DB_TO_LOG2).exp2();
                    }
                }
                for (x, g) in self.spec[c].iter_mut().zip(&self.bin_gain) {
                    *x *= *g;
                }
            }
            let last = self.spec[c].len() - 1;
            self.spec[c][0].im = 0.0;
            self.spec[c][last].im = 0.0;
            self.inv.process_with_scratch(&mut self.spec[c], &mut self.frame, &mut self.scratch_i).unwrap();
            let out = &mut self.outbuf[c];
            out.copy_within(HOP.., 0);
            out[FFT_N - HOP..].fill(0.0);
            for i in 0..FFT_N {
                out[i] += self.frame[i] * self.window[i] * scale;
            }
            self.inbuf[c].copy_within(HOP.., 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tuning::test_tuning;

    fn freqs() -> [f32; NBANDS] {
        test_tuning().band_freqs
    }

    #[test]
    fn unity_reconstruction() {
        let mut p = BandProcessor::new(48000.0, &freqs());
        let n = 48000;
        let input: Vec<f32> = (0..n).map(|i| (i as f32 * 0.37).sin() * 0.5 + (i as f32 * 0.011).sin() * 0.3).collect();
        let (mut l, mut r) = (input.clone(), input.clone());
        p.process(&mut l, &mut r);
        let mut err = 0f32;
        for i in LATENCY + FFT_N..n {
            err = err.max((l[i] - input[i - LATENCY]).abs());
        }
        assert!(20.0 * err.log10() < -90.0, "reconstruction error {err}");
    }

    fn sine_gain(p: &mut BandProcessor, f: f32, amp: f32) -> f32 {
        let n = 48000;
        let mut l: Vec<f32> = (0..n).map(|i| amp * (2.0 * std::f32::consts::PI * f * i as f32 / 48000.0).sin()).collect();
        let mut r = l.clone();
        p.process(&mut l, &mut r);
        let peak = l[n / 2..].iter().fold(0f32, |m, x| m.max(x.abs()));
        20.0 * (peak / amp).log10()
    }

    #[test]
    fn level_calibration() {
        let mut p = BandProcessor::new(48000.0, &freqs());
        sine_gain(&mut p, 1000.0, 0.5);
        let max = p.meters.level_db.iter().cloned().fold(f32::MIN, f32::max);
        assert!((max - (-6.02)).abs() < 0.5, "{max}");
    }

    #[test]
    fn regulator_limits_resonance_only() {
        let t = test_tuning();
        let prof = t.profile("music").unwrap();
        let mut cfg = BandConfig::default();
        cfg.reg_enable = true;
        cfg.reg_threshold = prof.threshold_high;
        cfg.reg_isolated = prof.isolated;
        cfg.reg_knee = 12.0;
        let mut p = BandProcessor::new(48000.0, &t.band_freqs);
        p.set_config(&cfg);
        // 515 Hz sits in the 469/656 Hz bands whose thresholds are ~ -36..-43 dB.
        let g = sine_gain(&mut p, 515.0, 0.5);
        assert!(g < -20.0, "515 Hz gain {g}");
        let mut p = BandProcessor::new(48000.0, &t.band_freqs);
        p.set_config(&cfg);
        let g = sine_gain(&mut p, 3000.0, 0.1);
        assert!(g.abs() < 0.5, "3 kHz gain {g}");
    }

    #[test]
    fn ieq_ignores_overall_tilt() {
        let f = freqs();
        let x = f.map(|v| v.log2());
        let target = test_tuning().ieq_curves["balanced"];
        // Same spectral shape at two different tilts must get the same correction.
        let bump: [f32; NBANDS] = std::array::from_fn(|b| if b == 3 { 6.0 } else { 0.0 });
        let a: [f32; NBANDS] = std::array::from_fn(|b| -20.0 - 4.0 * x[b] + bump[b]);
        let c: [f32; NBANDS] = std::array::from_fn(|b| -20.0 + 1.0 * x[b] + bump[b]);
        let ca = ieq_correction(&a, &target, &x, 8.0, 1.0);
        let cc = ieq_correction(&c, &target, &x, 8.0, 1.0);
        for b in 0..NBANDS {
            assert!((ca[b] - cc[b]).abs() < 1e-3, "band {b}: {} vs {}", ca[b], cc[b]);
        }
        // The bump is pulled down relative to its neighbours.
        assert!(ca[3] < ca[2] - 3.0 && ca[3] < ca[4] - 3.0);
    }

    #[test]
    fn volmax_is_applied_after_the_regulator() {
        // A loud 3 kHz tone in a band limited at -20 dB, with +6 dB volmax:
        // limit to the threshold first, then boost (as Dolby does on Windows).
        let mut cfg = BandConfig::default();
        cfg.reg_enable = true;
        cfg.reg_knee = 0.0;
        cfg.reg_threshold = [0.0; NBANDS];
        cfg.reg_threshold[11] = -20.0;
        cfg.volmax_db = 6.0;
        let mut p = BandProcessor::new(48000.0, &freqs());
        p.set_config(&cfg);
        let g = sine_gain(&mut p, 3000.0, 0.5); // -6 dBFS in
        assert!((g - (-20.0 + 6.0 - (-6.0))).abs() < 1.0, "net gain {g} dB, want about -8");
        // Below every threshold, volmax is a plain boost.
        let mut p = BandProcessor::new(48000.0, &freqs());
        p.set_config(&cfg);
        let g = sine_gain(&mut p, 3000.0, 0.01);
        assert!((g - 6.0).abs() < 0.5, "quiet tone gain {g}");
    }

    /// Gain at `probe` Hz (a quiet tone) while a 1 kHz tone of `amp` sets the playback level.
    fn probe_gain(p: &mut BandProcessor, amp: f32, probe: f32) -> f32 {
        let n = 3 * 48000;
        let w = |f: f32, i: usize| (2.0 * std::f32::consts::PI * f * i as f32 / 48000.0).sin();
        let input: Vec<f32> = (0..n).map(|i| amp * w(1000.0, i) + 0.01 * w(probe, i)).collect();
        let (mut l, mut r) = (input.clone(), input.clone());
        p.process(&mut l, &mut r);
        // Magnitude of the probe component over the last second; delay doesn't matter.
        let mag = |x: &[f32]| {
            let (mut re, mut im) = (0f64, 0f64);
            for i in n - 48000..n {
                let ph = 2.0 * std::f64::consts::PI * probe as f64 * i as f64 / 48000.0;
                re += x[i] as f64 * ph.cos();
                im += x[i] as f64 * ph.sin();
            }
            (re * re + im * im).sqrt()
        };
        (20.0 * (mag(&l) / mag(&input)).log10()) as f32
    }

    #[test]
    fn bass_loudness_follows_playback_level() {
        let mut cfg = BandConfig::default();
        cfg.lb_enable = true;
        cfg.lb_max_db = 9.0;
        cfg.lb_ref_db = -12.0;
        cfg.lb_slope = 0.5;
        cfg.lb_weight[0] = 1.0;
        cfg.lb_weight[1] = 1.0;
        cfg.lb_time_s = 0.2;
        let run = |amp: f32| {
            let mut p = BandProcessor::new(48000.0, &freqs());
            p.set_config(&cfg);
            (probe_gain(&mut p, amp, 50.0), p.meters.bass_db)
        };
        // Loud playback (-6 dB): no boost.
        let (g, m) = run(0.5);
        assert!(g.abs() < 0.5 && m < 0.1, "loud: {g} dB, meter {m}");
        // 14 dB quieter (-20 dB): half a dB per dB below -12.
        let (g, m) = run(0.1);
        assert!((g - 4.0).abs() < 0.7, "medium: {g} dB, meter {m}");
        // Very quiet: capped at the maximum.
        let (g, _) = run(0.01);
        assert!((g - 9.0).abs() < 0.7, "quiet: {g} dB");
        // Off: transparent.
        cfg.lb_enable = false;
        let mut p = BandProcessor::new(48000.0, &freqs());
        p.set_config(&cfg);
        assert!(probe_gain(&mut p, 0.01, 50.0).abs() < 0.1);
    }

    #[test]
    fn knee_curve() {
        assert_eq!(regulator_gain(-50.0, -20.0, 12.0), 0.0);
        assert!((regulator_gain(0.0, -20.0, 12.0) - (-20.0)).abs() < 1e-6);
        // continuous at the knee end
        assert!((regulator_gain(-32.0 + 24.0 - 1e-3, -20.0, 12.0) - regulator_gain(-8.0, -20.0, 12.0)).abs() < 1e-2);
    }
}
