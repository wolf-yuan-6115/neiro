//! RBJ-cookbook biquads in transposed direct form II (f64 state for low-frequency precision).

use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coefs {
    pub const IDENTITY: Coefs = Coefs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    /// Numerator equals denominator, as for a 0 dB peak or shelf: from a zero
    /// state the filter passes its input through unchanged.
    pub fn is_unity(&self) -> bool {
        self.b0 == 1.0 && self.b1 == self.a1 && self.b2 == self.a2
    }

    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Coefs {
        Coefs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    fn w0(fs: f64, f0: f64) -> (f64, f64) {
        let w0 = 2.0 * PI * (f0 / fs).clamp(1e-6, 0.4999);
        (w0.cos(), w0.sin())
    }

    pub fn peaking(fs: f64, f0: f64, q: f64, gain_db: f64) -> Coefs {
        let a = 10f64.powf(gain_db / 40.0);
        let (c, s) = Self::w0(fs, f0);
        let alpha = s / (2.0 * q.max(1e-3));
        Self::norm(1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a)
    }

    pub fn lowshelf(fs: f64, f0: f64, q: f64, gain_db: f64) -> Coefs {
        let a = 10f64.powf(gain_db / 40.0);
        let (c, s) = Self::w0(fs, f0);
        let alpha = s / (2.0 * q.max(1e-3));
        let sa = 2.0 * a.sqrt() * alpha;
        Self::norm(
            a * ((a + 1.0) - (a - 1.0) * c + sa),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - sa),
            (a + 1.0) + (a - 1.0) * c + sa,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - sa,
        )
    }

    pub fn highshelf(fs: f64, f0: f64, q: f64, gain_db: f64) -> Coefs {
        let a = 10f64.powf(gain_db / 40.0);
        let (c, s) = Self::w0(fs, f0);
        let alpha = s / (2.0 * q.max(1e-3));
        let sa = 2.0 * a.sqrt() * alpha;
        Self::norm(
            a * ((a + 1.0) + (a - 1.0) * c + sa),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - sa),
            (a + 1.0) - (a - 1.0) * c + sa,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - sa,
        )
    }

    pub fn notch(fs: f64, f0: f64, q: f64) -> Coefs {
        let (c, s) = Self::w0(fs, f0);
        let alpha = s / (2.0 * q.max(1e-3));
        Self::norm(1.0, -2.0 * c, 1.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    pub fn lowpass(fs: f64, f0: f64, q: f64) -> Coefs {
        let (c, s) = Self::w0(fs, f0);
        let alpha = s / (2.0 * q);
        Self::norm((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    pub fn highpass(fs: f64, f0: f64, q: f64) -> Coefs {
        let (c, s) = Self::w0(fs, f0);
        let alpha = s / (2.0 * q);
        Self::norm((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    /// Magnitude response in dB at frequency `f`.
    pub fn response_db(&self, fs: f64, f: f64) -> f64 {
        let w = 2.0 * PI * f / fs;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()
    }
}

/// Butterworth Q values for cascading two biquads into a 4th-order section.
pub const BUTTER4_Q: [f64; 2] = [0.541_196_1, 1.306_563];
pub const BUTTER2_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Section Qs of an even-order Butterworth filter built from biquads
/// (odd orders round up; order is capped at 8).
pub fn butterworth_qs(order: u32) -> impl Iterator<Item = f64> {
    let n = (order.clamp(1, 8).div_ceil(2) * 2) as f64;
    (1..=(n as u32 / 2)).map(move |k| 1.0 / (2.0 * (std::f64::consts::PI * (2.0 * k as f64 - 1.0) / (2.0 * n)).cos()))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    z1: f64,
    z2: f64,
}

impl State {
    #[inline]
    pub fn run(&mut self, c: &Coefs, x: f32) -> f32 {
        let x = x as f64;
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        // Flush denormals so silence never stalls the FPU.
        if self.z1.abs() < 1e-25 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-25 {
            self.z2 = 0.0;
        }
        y as f32
    }

    pub fn is_zero(&self) -> bool {
        self.z1 == 0.0 && self.z2 == 0.0
    }

    pub fn reset(&mut self) {
        *self = State::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaking_gain_at_f0() {
        for &(f0, q, g) in &[(515.0, 9.0, -9.0), (2000.0, 3.5, 1.5), (520.0, 1.5, -1.5), (60.0, 0.7, 6.0)] {
            let c = Coefs::peaking(48000.0, f0, q, g);
            assert!((c.response_db(48000.0, f0) - g).abs() < 0.1, "{f0} {g}");
            assert!(c.response_db(48000.0, f0 * 8.0).abs() < 0.5);
        }
    }

    #[test]
    fn shelves_reach_gain_at_extremes() {
        let fs = 48000.0;
        let ls = Coefs::lowshelf(fs, 120.0, BUTTER2_Q, 6.0);
        assert!((ls.response_db(fs, 10.0) - 6.0).abs() < 0.1);
        assert!(ls.response_db(fs, 10000.0).abs() < 0.1);
        let hs = Coefs::highshelf(fs, 8000.0, BUTTER2_Q, -4.0);
        assert!((hs.response_db(fs, 23000.0) + 4.0).abs() < 0.1);
        assert!(hs.response_db(fs, 100.0).abs() < 0.1);
    }

    #[test]
    fn notch_and_butterworth_orders() {
        let fs = 48000.0;
        let n = Coefs::notch(fs, 480.0, 6.0);
        assert!(n.response_db(fs, 480.0) < -60.0);
        assert!(n.response_db(fs, 2000.0).abs() < 0.1);
        let qs: Vec<f64> = butterworth_qs(4).collect();
        assert!((qs[0] - BUTTER4_Q[0]).abs() < 1e-4 && (qs[1] - BUTTER4_Q[1]).abs() < 1e-4);
        for order in [2, 4, 6, 8] {
            // Every Butterworth is -3 dB at the corner, regardless of order.
            let db: f64 = butterworth_qs(order).map(|q| Coefs::highpass(fs, 100.0, q).response_db(fs, 100.0)).sum();
            assert!((db + 3.01).abs() < 0.05, "order {order}: {db}");
        }
    }

    #[test]
    fn hp_lp_corner() {
        let hp = Coefs::highpass(48000.0, 120.0, BUTTER2_Q);
        assert!((hp.response_db(48000.0, 120.0) + 3.01).abs() < 0.05);
        let lp = Coefs::lowpass(48000.0, 160.0, BUTTER2_Q);
        assert!((lp.response_db(48000.0, 160.0) + 3.01).abs() < 0.05);
    }

    #[test]
    fn filter_matches_response() {
        let fs = 48000.0;
        let c = Coefs::peaking(fs, 1000.0, 2.0, 6.0);
        let mut s = State::default();
        let mut peak = 0.0f32;
        for n in 0..48000 {
            let x = (2.0 * PI * 1000.0 * n as f64 / fs).sin() as f32;
            let y = s.run(&c, x);
            if n > 24000 {
                peak = peak.max(y.abs());
            }
        }
        assert!((20.0 * (peak as f64).log10() - 6.0).abs() < 0.05);
    }
}
