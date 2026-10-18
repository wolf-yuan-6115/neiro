//! Stereo-linked lookahead brickwall limiter.

pub const LOOKAHEAD: usize = 48;

pub struct Limiter {
    delay: [[f32; LOOKAHEAD]; 2],
    req: [f32; LOOKAHEAD],
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
            req: [1.0; LOOKAHEAD],
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
            // Window spans LOOKAHEAD+1 samples: the outgoing entry still counts this sample.
            let m = self.req.iter().cloned().fold(need, f32::min);
            self.req[self.idx] = need;
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
}
