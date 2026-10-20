//! Stereo-linked lookahead brickwall limiter.

pub const LOOKAHEAD: usize = 48;
/// Ring capacity of the running-minimum deque: a power of two above LOOKAHEAD + 1.
const RING: usize = 64;

pub struct Limiter {
    delay: [[f32; LOOKAHEAD]; 2],
    /// Monotonic deque over the required gains of the last LOOKAHEAD + 1 samples:
    /// values rise from `head` to `tail`, so the front is the window minimum.
    min_val: [f32; RING],
    min_at: [u32; RING],
    head: usize,
    tail: usize,
    /// Sample counter for expiring deque entries (wraps).
    t: u32,
    idx: usize,
    env: f32,
    box_sum: f32,
    box_buf: [f32; LOOKAHEAD],
    pub gr_db: f32,
}

impl Limiter {
    pub fn new() -> Limiter {
        Limiter {
            delay: [[0.0; LOOKAHEAD]; 2],
            min_val: [1.0; RING],
            min_at: [0; RING],
            head: 0,
            tail: 0,
            t: 0,
            idx: 0,
            env: 1.0,
            box_sum: LOOKAHEAD as f32,
            box_buf: [1.0; LOOKAHEAD],
            gr_db: 0.0,
        }
    }

    /// `rel` is the per-sample release coefficient.
    pub fn process(&mut self, ceiling: f32, rel: f32, l: &mut [f32], r: &mut [f32]) {
        let mut min_g = 1.0f32;
        for i in 0..l.len() {
            let peak = l[i].abs().max(r[i].abs());
            let need = if peak > ceiling { ceiling / peak } else { 1.0 };
            // Minimum over a window of LOOKAHEAD+1 samples, this one included. Every
            // `need` is <= 1, so the empty start matches a window full of 1.0.
            while self.tail != self.head && self.min_val[(self.tail - 1) % RING] >= need {
                self.tail -= 1;
            }
            self.min_val[self.tail % RING] = need;
            self.min_at[self.tail % RING] = self.t;
            self.tail += 1;
            while self.t.wrapping_sub(self.min_at[self.head % RING]) > LOOKAHEAD as u32 {
                self.head += 1;
            }
            let m = self.min_val[self.head % RING];
            self.t = self.t.wrapping_add(1);
            self.env = if m < self.env { m } else { m + rel * (self.env - m) };
            // Box filter over the lookahead: gain reaches `m` exactly when the peak is output.
            self.box_sum += self.env - self.box_buf[self.idx];
            self.box_buf[self.idx] = self.env;
            let g = (self.box_sum / LOOKAHEAD as f32).min(1.0);
            min_g = min_g.min(g);
            let (dl, dr) = (self.delay[0][self.idx], self.delay[1][self.idx]);
            self.delay[0][self.idx] = l[i];
            self.delay[1][self.idx] = r[i];
            self.idx = (self.idx + 1) % LOOKAHEAD;
            l[i] = dl * g;
            r[i] = dr * g;
        }
        self.gr_db = 20.0 * min_g.max(1e-6).log10();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_exceeds_ceiling() {
        let mut lim = Limiter::new();
        let n = 9600;
        let mut l: Vec<f32> = (0..n).map(|i| if i % 997 < 3 { 4.0 } else { (i as f32 * 0.05).sin() * 1.5 }).collect();
        let mut r = l.clone();
        let ceil = 10f32.powf(-1.0 / 20.0);
        lim.process(ceil, 0.999, &mut l, &mut r);
        let peak = l.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!(peak <= ceil * 1.0001, "{peak}");
    }

    /// The previous limiter, scanning the whole window every sample.
    struct Scan {
        delay: [[f32; LOOKAHEAD]; 2],
        req: [f32; LOOKAHEAD],
        idx: usize,
        env: f32,
        box_sum: f32,
        box_buf: [f32; LOOKAHEAD],
    }

    impl Scan {
        fn process(&mut self, ceiling: f32, rel: f32, l: &mut [f32], r: &mut [f32]) {
            for i in 0..l.len() {
                let peak = l[i].abs().max(r[i].abs());
                let need = if peak > ceiling { ceiling / peak } else { 1.0 };
                let m = self.req.iter().cloned().fold(need, f32::min);
                self.req[self.idx] = need;
                self.env = if m < self.env { m } else { m + rel * (self.env - m) };
                self.box_sum += self.env - self.box_buf[self.idx];
                self.box_buf[self.idx] = self.env;
                let g = (self.box_sum / LOOKAHEAD as f32).min(1.0);
                let (dl, dr) = (self.delay[0][self.idx], self.delay[1][self.idx]);
                self.delay[0][self.idx] = l[i];
                self.delay[1][self.idx] = r[i];
                self.idx = (self.idx + 1) % LOOKAHEAD;
                l[i] = dl * g;
                r[i] = dr * g;
            }
        }
    }

    #[test]
    fn running_minimum_matches_full_scan() {
        let mut lim = Limiter::new();
        let mut scan = Scan {
            delay: [[0.0; LOOKAHEAD]; 2],
            req: [1.0; LOOKAHEAD],
            idx: 0,
            env: 1.0,
            box_sum: LOOKAHEAD as f32,
            box_buf: [1.0; LOOKAHEAD],
        };
        let mut seed = 7u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        // Bursts of loud noise and peaks over quiet passages, so the window minimum
        // keeps rising, falling and expiring.
        let n = 200_000;
        let l: Vec<f32> = (0..n).map(|i| noise() * if (i / 3000) % 3 == 0 { 4.0 } else { 0.6 } + if i % 911 == 0 { 3.0 } else { 0.0 }).collect();
        let r: Vec<f32> = (0..n).map(|_| noise() * 1.5).collect();
        let (mut al, mut ar, mut bl, mut br) = (l.clone(), r.clone(), l, r);
        let mut s = 0;
        for (k, blk) in [1usize, 333, 1024, 7, 48, 49].iter().cycle().enumerate() {
            if s >= n {
                break;
            }
            let e = (s + blk).min(n);
            // Change the ceiling now and then, and sometimes disable it, as a config push would.
            let ceil = match (k / 50) % 3 {
                0 => 10f32.powf(-1.0 / 20.0),
                1 => 0.5,
                _ => f32::MAX,
            };
            lim.process(ceil, 0.999, &mut al[s..e], &mut ar[s..e]);
            scan.process(ceil, 0.999, &mut bl[s..e], &mut br[s..e]);
            s = e;
        }
        assert!(al.iter().zip(&bl).chain(ar.iter().zip(&br)).all(|(a, b)| a.to_bits() == b.to_bits()));
    }
}
