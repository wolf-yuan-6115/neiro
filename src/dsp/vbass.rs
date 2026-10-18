//! Psychoacoustic bass: envelope-normalised Chebyshev harmonics of the 40–160 Hz
//! band so the ear infers fundamentals the micro-speakers cannot reproduce.

use super::biquad::{Coefs, State, BUTTER2_Q, BUTTER4_Q};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VbConfig {
    pub enable: bool,
    pub src_hp: Coefs,
    pub src_lp: [Coefs; 2],
    pub mix_hp: Coefs,
    pub mix_lp: Coefs,
    /// Linear weights of 2nd, 3rd, 4th harmonic (already including overall gain).
    pub weights: [f32; 3],
    pub dry_hp: Option<Coefs>,
}

impl VbConfig {
    pub fn disabled() -> VbConfig {
        VbConfig {
            enable: false,
            src_hp: Coefs::IDENTITY,
            src_lp: [Coefs::IDENTITY; 2],
            mix_hp: Coefs::IDENTITY,
            mix_lp: Coefs::IDENTITY,
            weights: [0.0; 3],
            dry_hp: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(fs: f64, src: (f32, f32), mix: (f32, f32), gain_db: f32, h_db: [f32; 3], dry_hp: Option<f32>) -> VbConfig {
        let g = 10f32.powf(gain_db / 20.0);
        VbConfig {
            enable: true,
            src_hp: Coefs::highpass(fs, src.0 as f64, BUTTER2_Q),
            src_lp: [
                Coefs::lowpass(fs, src.1 as f64, BUTTER4_Q[0]),
                Coefs::lowpass(fs, src.1 as f64, BUTTER4_Q[1]),
            ],
            mix_hp: Coefs::highpass(fs, mix.0 as f64, BUTTER2_Q),
            mix_lp: Coefs::lowpass(fs, mix.1 as f64, BUTTER2_Q),
            weights: h_db.map(|d| g * 10f32.powf(d / 20.0)),
            dry_hp: dry_hp.map(|f| Coefs::highpass(fs, f as f64, BUTTER2_Q)),
        }
    }
}

pub struct VirtualBass {
    src: [State; 3],
    mix: [State; 2],
    dry: [State; 2],
    env: f32,
    att: f32,
    rel: f32,
}

impl VirtualBass {
    pub fn new(fs: f32) -> VirtualBass {
        VirtualBass {
            src: Default::default(),
            mix: Default::default(),
            dry: Default::default(),
            env: 0.0,
            att: (-1.0 / (0.002 * fs)).exp(),
            rel: (-1.0 / (0.060 * fs)).exp(),
        }
    }

    pub fn process(&mut self, c: &VbConfig, l: &mut [f32], r: &mut [f32]) {
        for i in 0..l.len() {
            let m = 0.5 * (l[i] + r[i]);
            let x = self.src[0].run(&c.src_hp, m);
            let x = self.src[1].run(&c.src_lp[0], x);
            let x = self.src[2].run(&c.src_lp[1], x);
            let a = x.abs();
            let k = if a > self.env { self.att } else { self.rel };
            self.env = a + k * (self.env - a);
            let h = if self.env > 1e-6 {
                let u = (x / self.env).clamp(-1.0, 1.0);
                let u2 = u * u;
                let t2 = 2.0 * u2 - 1.0;
                let t3 = u * (4.0 * u2 - 3.0);
                let t4 = 8.0 * u2 * u2 - 8.0 * u2 + 1.0;
                self.env * (c.weights[0] * t2 + c.weights[1] * t3 + c.weights[2] * t4)
            } else {
                0.0
            };
            let h = self.mix[0].run(&c.mix_hp, h);
            let h = self.mix[1].run(&c.mix_lp, h);
            let (mut dl, mut dr) = (l[i], r[i]);
            if let Some(hp) = &c.dry_hp {
                dl = self.dry[0].run(hp, dl);
                dr = self.dry[1].run(hp, dr);
            }
            l[i] = dl + h;
            r[i] = dr + h;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_harmonics_of_bass_only() {
        let fs = 48000.0;
        let c = VbConfig::new(fs as f64, (35.0, 160.0), (94.0, 469.0), 0.0, [-2.0, -9.0, -12.0], None);
        let run = |f: f32| {
            let mut vb = VirtualBass::new(fs);
            let n = 48000;
            let mut l: Vec<f32> = (0..n).map(|i| 0.3 * (2.0 * std::f32::consts::PI * f * i as f32 / fs).sin()).collect();
            let orig = l.clone();
            let mut r = l.clone();
            vb.process(&c, &mut l, &mut r);
            // energy of the added signal
            let e: f32 = l[n / 2..].iter().zip(&orig[n / 2..]).map(|(a, b)| (a - b) * (a - b)).sum();
            assert!(l.iter().all(|v| v.is_finite()));
            e / (n / 2) as f32
        };
        assert!(run(70.0) > 1e-3);
        assert!(run(2000.0) < 1e-6);
    }
}
