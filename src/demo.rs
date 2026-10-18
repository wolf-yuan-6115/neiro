//! Procedurally synthesised demo loops with energy across the whole spectrum,
//! for A/B tuning without shipping copyrighted music. Every loop is seamless:
//! note tails that run past the end wrap around to the start.

use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

pub const FS: usize = 48000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Demo {
    #[default]
    Electronic,
    Rock,
    Ballad,
}

impl Demo {
    pub const ALL: [Demo; 3] = [Demo::Electronic, Demo::Rock, Demo::Ballad];

    pub fn label(self) -> &'static str {
        match self {
            Demo::Electronic => "Electronic",
            Demo::Rock => "Rock band",
            Demo::Ballad => "Ballad",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Demo::Electronic => "four-on-the-floor kick, sub bass, synth chords · 120 bpm",
            Demo::Rock => "live drums, distorted guitars, picked bass · 130 bpm",
            Demo::Ballad => "piano, brushed drums, fingered bass, vocal-like lead · 76 bpm",
        }
    }
}

pub fn generate(demo: Demo) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = match demo {
        Demo::Electronic => electronic(),
        Demo::Rock => rock(),
        Demo::Ballad => ballad(),
    };
    normalize(&mut l, &mut r);
    (l, r)
}

// ---------------------------------------------------------------------------
// Shared building blocks.

struct Rng(u32);

impl Rng {
    fn noise(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.0 >> 8) as f32 / (1u32 << 23) as f32 - 1.0
    }
}

/// Adds `f(t)` (t in seconds since the hit) into a circular buffer.
fn place(buf: &mut [f32], start_s: f32, dur_s: f32, mut f: impl FnMut(f32) -> f32) {
    let n = buf.len();
    let s0 = (start_s * FS as f32) as usize;
    for i in 0..(dur_s * FS as f32) as usize {
        buf[(s0 + i) % n] += f(i as f32 / FS as f32);
    }
}

fn saw(phase: f32) -> f32 {
    2.0 * (phase - phase.floor()) - 1.0
}

fn midi(n: f32) -> f32 {
    440.0 * 2f32.powf((n - 69.0) / 12.0)
}

fn onepole(cutoff: f32) -> f32 {
    1.0 - (-2.0 * PI * cutoff / FS as f32).exp()
}

fn loop_len(bpm: f32, bars: usize) -> (f32, usize) {
    let beat = 60.0 / bpm;
    (beat, (bars as f32 * 4.0 * beat * FS as f32) as usize)
}

/// -16 dBFS RMS, peaks capped at -1 dBFS, so all demos play at a similar loudness.
fn normalize(l: &mut [f32], r: &mut [f32]) {
    let n = (l.len() * 2) as f32;
    let rms = (l.iter().chain(r.iter()).map(|v| v * v).sum::<f32>() / n).sqrt();
    let peak = l.iter().chain(r.iter()).fold(0f32, |m, v| m.max(v.abs()));
    let g = (10f32.powf(-16.0 / 20.0) / rms.max(1e-9)).min(0.89 / peak.max(1e-9));
    l.iter_mut().chain(r.iter_mut()).for_each(|v| *v *= g);
}

fn kick(buf: &mut [f32], at: f32, f_lo: f32, f_sweep: f32, decay: f32, level: f32) {
    let mut ph = 0.0f32;
    place(buf, at, decay * 2.0, |t| {
        ph += (f_lo + f_sweep * (-t / 0.03).exp()) / FS as f32;
        level * (2.0 * PI * ph).sin() * (-t / decay).exp()
    });
}

/// Doubly differentiated noise: a crude high-pass for cymbals and shakers.
fn metal(buf: &mut [f32], rng: &mut Rng, at: f32, attack: f32, decay: f32, level: f32) {
    let (mut p1, mut p2) = (0.0, 0.0);
    place(buf, at, decay * 5.0, |t| {
        let x = rng.noise();
        let d1 = x - p1;
        let d2 = d1 - p2;
        p1 = x;
        p2 = d1;
        level * d2 * (t / attack.max(1e-4)).min(1.0) * (-t / decay).exp()
    });
}

/// Two-pole resonator, used for vocal formants.
#[derive(Clone, Copy)]
struct Reson {
    a1: f32,
    a2: f32,
    g: f32,
    y1: f32,
    y2: f32,
}

impl Reson {
    fn new(freq: f32, bw: f32, gain: f32) -> Reson {
        let r = (-PI * bw / FS as f32).exp();
        let w = 2.0 * PI * freq / FS as f32;
        Reson { a1: 2.0 * r * w.cos(), a2: -r * r, g: gain * (1.0 - r * r), y1: 0.0, y2: 0.0 }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.g * x + self.a1 * self.y1 + self.a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Small Schroeder reverb run over two passes of the loop so its tail wraps
/// seamlessly. `mix` is the wet level.
fn reverb(l: &mut [f32], r: &mut [f32], mix: f32) {
    let n = l.len();
    let make = |src: &[f32], spread: usize| -> Vec<f32> {
        let combs = [1116 + spread, 1188 + spread, 1277 + spread, 1356 + spread].map(|d| d * 2);
        let mut bufs: Vec<Vec<f32>> = combs.iter().map(|&d| vec![0.0; d]).collect();
        let mut idx = vec![0usize; combs.len()];
        let aps = [556 + spread, 441 + spread].map(|d| d * 2);
        let mut apb: Vec<Vec<f32>> = aps.iter().map(|&d| vec![0.0; d]).collect();
        let mut api = vec![0usize; aps.len()];
        let mut out = vec![0.0; n];
        for pass in 0..2 {
            for i in 0..n {
                let x = src[i];
                let mut y = 0.0;
                for c in 0..combs.len() {
                    let d = bufs[c][idx[c]];
                    bufs[c][idx[c]] = x + d * 0.8;
                    idx[c] = (idx[c] + 1) % combs[c];
                    y += d * 0.25;
                }
                for a in 0..aps.len() {
                    let d = apb[a][api[a]];
                    apb[a][api[a]] = y + d * 0.5;
                    api[a] = (api[a] + 1) % aps[a];
                    y = d - y * 0.5;
                }
                if pass == 1 {
                    out[i] = y;
                }
            }
        }
        out
    };
    let (wl, wr) = (make(l, 0), make(r, 23));
    for i in 0..n {
        l[i] += mix * wl[i];
        r[i] += mix * wr[i];
    }
}

// ---------------------------------------------------------------------------
// The loops.

/// Four-on-the-floor kick, sub bass, synth pad and lead.
fn electronic() -> (Vec<f32>, Vec<f32>) {
    let (beat, n) = loop_len(120.0, 4);
    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
    let mut mono = vec![0f32; n];
    let mut rng = Rng(0x5eed);

    // Chords: Am, F, C, G (root midi, chord tones).
    let roots = [33.0, 29.0, 36.0, 31.0];
    let chords = [[57.0, 60.0, 64.0], [53.0, 57.0, 60.0], [60.0, 64.0, 67.0], [55.0, 59.0, 62.0]];
    let melody = [76.0, 74.0, 72.0, 74.0, 76.0, 76.0, 79.0, 76.0, 72.0, 71.0, 72.0, 74.0, 76.0, 74.0, 71.0, 67.0];

    for bar in 0..4 {
        let t0 = bar as f32 * 4.0 * beat;
        for b in 0..4 {
            kick(&mut mono, t0 + b as f32 * beat, 45.0, 75.0, 0.22, 0.9);
            // Snare on 2 and 4: noise + 190 Hz body.
            if b % 2 == 1 {
                let mut prev = 0.0;
                place(&mut mono, t0 + b as f32 * beat, 0.3, |t| {
                    let x = rng.noise();
                    let hp = x - prev;
                    prev = x;
                    0.6 * hp * (-t / 0.12).exp() + 0.3 * (2.0 * PI * 190.0 * t).sin() * (-t / 0.06).exp()
                });
            }
        }
        // Hats on eighths, panned slightly right.
        for e in 0..8 {
            let decay = if e % 4 == 3 { 0.12 } else { 0.025 };
            metal(&mut r, &mut rng, t0 + e as f32 * beat / 2.0, 0.0, decay, 0.09);
        }
        // Bass: eighth notes on the root, saw + sub, one-pole low-passed.
        let root = midi(roots[bar]);
        for e in 0..8 {
            let f = if e == 7 { root * 1.5 } else { root };
            let mut lp = 0.0;
            let a = onepole(400.0);
            place(&mut mono, t0 + e as f32 * beat / 2.0, beat / 2.0, |t| {
                lp += a * (saw(f * t) - lp);
                let env = (t / 0.005).min(1.0) * (-t / 0.35).exp();
                env * (0.28 * lp + 0.3 * (2.0 * PI * f * t).sin())
            });
        }
        // Pad: detuned saws, wide stereo.
        for (k, note) in chords[bar].iter().enumerate() {
            let f = midi(*note);
            for (buf, det) in [(&mut l, 1.003f32), (&mut r, 0.997f32)] {
                let mut lp = 0.0;
                let a = onepole(4000.0);
                place(buf, t0, 4.0 * beat, |t| {
                    lp += a * (saw(f * det * t + k as f32 * 0.31) - lp);
                    let env = (t / 0.25).min(1.0) * (1.0 - (t / (4.0 * beat)).powi(4));
                    0.11 * lp * env
                });
            }
        }
        // Lead: bright saw with vibrato, panned slightly left.
        for q in 0..4 {
            let f = midi(melody[bar * 4 + q]);
            place(&mut l, t0 + q as f32 * beat, beat * 0.9, |t| {
                let vib = 1.0 + 0.004 * (2.0 * PI * 5.5 * t).sin();
                let ph = f * vib * t;
                let x: f32 = (1..=8).map(|h| (2.0 * PI * h as f32 * ph).sin() / h as f32).sum();
                0.16 * x * (t / 0.01).min(1.0) * (-t / 0.6).exp()
            });
        }
    }
    for i in 0..n {
        l[i] += mono[i];
        r[i] += mono[i];
    }
    (l, r)
}

/// Live-style drums with a crash, double-tracked distorted power chords, picked bass.
fn rock() -> (Vec<f32>, Vec<f32>) {
    let (beat, n) = loop_len(130.0, 4);
    let eighth = beat / 2.0;
    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
    let mut mono = vec![0f32; n];
    let mut rng = Rng(0xb00);

    // E5, C5, G5, D5 power chords; bass an octave below the guitar root.
    let roots = [40.0, 48.0, 43.0, 50.0];

    // Crash on the downbeat of the loop, different noise per side for width.
    metal(&mut l, &mut rng, 0.0, 0.002, 1.1, 0.16);
    metal(&mut r, &mut rng, 0.0, 0.002, 1.2, 0.16);

    for bar in 0..4 {
        let t0 = bar as f32 * 4.0 * beat;
        for e in 0..8 {
            let at = t0 + e as f32 * eighth;
            // Kick on 1, the "and" of 2, and 3; snare on 2 and 4.
            if [0, 3, 4].contains(&e) {
                kick(&mut mono, at, 52.0, 95.0, 0.16, 0.95);
                place(&mut mono, at, 0.01, |t| 0.25 * rng.noise() * (-t / 0.002).exp());
            }
            if e == 2 || e == 6 {
                let mut prev = 0.0;
                place(&mut mono, at, 0.45, |t| {
                    let x = rng.noise();
                    let hp = x - prev;
                    prev = x;
                    0.75 * hp * (-t / 0.16).exp()
                        + 0.4 * (2.0 * PI * 200.0 * t).sin() * (-t / 0.05).exp()
                        + 0.2 * (2.0 * PI * 330.0 * t).sin() * (-t / 0.04).exp()
                });
            }
            if !(bar == 0 && e == 0) {
                metal(&mut r, &mut rng, at, 0.0, 0.03, 0.08);
            }

            // Guitars: palm-muted chugs, the last two eighths of each bar ring out.
            let f = midi(roots[bar]);
            let ring = e >= 6;
            let dur = if ring { 2.0 * eighth } else { eighth };
            for (side, det, delay) in [(0usize, 1.002f32, 0.0f32), (1, 0.998, 0.009)] {
                let (mut lp1, mut lp2) = (0.0, 0.0);
                let a = onepole(if ring { 3400.0 } else { 1800.0 });
                let buf = if side == 0 { &mut l } else { &mut r };
                place(buf, at + delay, dur, |t| {
                    let x = saw(f * det * t) + saw(f * 1.4983 * det * t + 0.2) + 0.7 * saw(2.0 * f * det * t + 0.5);
                    let d = (3.5 * x).tanh();
                    lp1 += a * (d - lp1);
                    lp2 += a * (lp1 - lp2);
                    let env = (t / 0.003).min(1.0) * (-t / if ring { 0.8 } else { 0.12 }).exp() * (1.0 - (t / dur).powi(8));
                    0.17 * lp2 * env
                });
            }

            // Bass follows the guitar root an octave down.
            let fb = f / 2.0;
            let mut lp = 0.0;
            let a = onepole(900.0);
            place(&mut mono, at, eighth, |t| {
                lp += a * (saw(fb * t) - lp);
                let env = (t / 0.002).min(1.0) * (-t / 0.4).exp() * (1.0 - (t / eighth).powi(8));
                env * (0.35 * lp + 0.25 * (2.0 * PI * fb * t).sin())
            });
        }
    }
    for i in 0..n {
        l[i] += mono[i];
        r[i] += mono[i];
    }
    (l, r)
}

/// Piano chords, brushed drums and shaker, fingered bass, formant-filtered "aah" lead.
fn ballad() -> (Vec<f32>, Vec<f32>) {
    let (beat, n) = loop_len(76.0, 4);
    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
    let mut mono = vec![0f32; n];
    let mut rng = Rng(0xba11);

    // Cmaj7, Am7, Fmaj7, G: (bass midi, piano voicing).
    let chords: [(f32, [f32; 4]); 4] = [
        (36.0, [60.0, 64.0, 67.0, 71.0]),
        (33.0, [57.0, 60.0, 64.0, 67.0]),
        (29.0, [57.0, 60.0, 64.0, 65.0]),
        (31.0, [55.0, 59.0, 62.0, 67.0]),
    ];
    let melody = [64.0, 67.0, 69.0, 67.0, 64.0, 62.0, 60.0, 62.0, 65.0, 64.0, 62.0, 60.0, 62.0, 59.0, 62.0, 64.0];

    for bar in 0..4 {
        let t0 = bar as f32 * 4.0 * beat;
        let (bass, voicing) = chords[bar];
        for b in 0..4 {
            let at = t0 + b as f32 * beat;
            if b % 2 == 0 {
                kick(&mut mono, at, 48.0, 60.0, 0.28, 0.9);
            } else {
                // Brushed snare: soft-attack filtered noise.
                let mut lp = 0.0;
                let a = onepole(9000.0);
                place(&mut mono, at, 0.6, |t| {
                    lp += a * (rng.noise() - lp);
                    0.35 * lp * (t / 0.02).min(1.0) * (-t / 0.22).exp()
                });
            }
            // Shaker on sixteenths, accented on eighths.
            for s in 0..4 {
                let level = if s % 2 == 0 { 0.16 } else { 0.1 };
                metal(&mut r, &mut rng, at + s as f32 * beat / 4.0, 0.008, 0.04, level);
            }
        }
        // Piano: chord on beat 1, softer re-hit on beat 3. Slightly inharmonic partials.
        for (hit, vel) in [(0.0, 1.0f32), (2.0, 0.6)] {
            for note in voicing {
                let f = midi(note);
                let pan = ((note - 55.0) / 20.0).clamp(0.0, 1.0);
                let tone = move |t: f32| -> f32 {
                    let mut x = 0.0;
                    for k in 1..=8 {
                        let kf = k as f32;
                        let fk = kf * f * (1.0 + 0.0003 * kf * kf).sqrt();
                        x += (2.0 * PI * fk * t).sin() / kf.powf(1.3) * (-t * kf.powf(0.6) / 2.5).exp();
                    }
                    0.07 * vel * x * (t / 0.002).min(1.0)
                };
                place(&mut l, t0 + hit * beat, 2.0 * beat, |t| tone(t) * (1.0 - 0.5 * pan));
                place(&mut r, t0 + hit * beat, 2.0 * beat, |t| tone(t) * (0.5 + 0.5 * pan));
            }
        }
        // Fingered bass: half notes, root then fifth.
        for (h, interval) in [(0.0f32, 0.0f32), (2.0, 7.0)] {
            let f = midi(bass + interval);
            place(&mut mono, t0 + h * beat, 2.0 * beat, |t| {
                let x = (2.0 * PI * f * t).sin() + 0.35 * (4.0 * PI * f * t).sin() + 0.12 * (6.0 * PI * f * t).sin();
                0.55 * x * (t / 0.01).min(1.0) * (-t / 1.2).exp() * (1.0 - (t / (2.0 * beat)).powi(6))
            });
        }
        // "Aah" lead: buzzy source through three vowel formants, with delayed vibrato.
        for q in 0..4 {
            let f = midi(melody[bar * 4 + q]);
            let dur = beat * 1.05;
            let mut formants = [Reson::new(800.0, 80.0, 0.7), Reson::new(1150.0, 90.0, 0.5), Reson::new(2900.0, 120.0, 0.9), Reson::new(3400.0, 200.0, 0.5)];
            let mut ph = 0.0f32;
            let mut nrng = Rng(0x7a + (bar * 4 + q) as u32);
            place(&mut mono, t0 + q as f32 * beat, dur, |t| {
                let vib = 1.0 + 0.006 * (t / 0.3).min(1.0) * (2.0 * PI * 5.2 * t).sin();
                ph += f * vib / FS as f32;
                let src = saw(ph) + 0.05 * nrng.noise();
                let y: f32 = formants.iter_mut().map(|fm| fm.run(src)).sum();
                let env = (t / 0.06).min(1.0) * ((dur - t) / 0.08).clamp(0.0, 1.0);
                0.45 * y * env
            });
        }
    }
    for i in 0..n {
        l[i] += mono[i];
        r[i] += mono[i];
    }
    reverb(&mut l, &mut r, 0.18);
    (l, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demos_are_full_range_and_not_clipping() {
        let mut lens = vec![];
        for d in Demo::ALL {
            let (l, r) = generate(d);
            lens.push(l.len());
            assert!(l.len() > 6 * FS && l.len() < 14 * FS, "{d:?} length {}", l.len());
            assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 0.9), "{d:?} clips");
            let bands = crate::clip::octave_energy(&l, &r, FS as f32);
            let loudest = bands.iter().cloned().fold(f32::MIN, f32::max);
            println!("{d:?} octaves 63..8k: {:?}", bands.map(|b| (b - loudest).round()));
            for (i, b) in bands.iter().enumerate() {
                assert!(*b > loudest - 30.0, "{d:?} octave {i} at {:.1} dB below the loudest", loudest - b);
            }
            // Loop point: the end flows into the start without a jump.
            let jump = (l[0] - l[l.len() - 1]).abs();
            let typical = l.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
            assert!(jump <= typical, "{d:?} loop jump {jump} vs max step {typical}");
        }
        assert!(lens[0] != lens[2], "demos should differ");
    }
}
