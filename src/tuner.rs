//! Blind A/B search for the personal EQ.
//!
//! Each trial pits the current EQ against a candidate with one change. The
//! candidate's side (A or B) is random. Searches run coarse to fine:
//!   1. each band's gain ±3 dB, pushing further while the listener keeps agreeing;
//!   2. the frequency of bands that moved by 3 dB or more, ×/÷ 1.5;
//!   3. every band's gain again in ±1.5 dB steps;
//!   4. two earlier decisions are re-asked as a consistency check.

use crate::params::{PersonalEq, PE_BANDS, PE_FREQ_RANGE};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const MAX_GAIN: f32 = 9.0;
const BAND_ORDER: [usize; PE_BANDS] = [0, 4, 3, 1, 2];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    A,
    B,
    Same,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Param {
    Gain(usize),
    Freq(usize),
}

impl Param {
    fn get(&self, eq: &PersonalEq) -> f32 {
        match *self {
            Param::Gain(b) => eq.bands[b].gain_db,
            Param::Freq(b) => eq.bands[b].freq,
        }
    }

    fn set(&self, eq: &mut PersonalEq, v: f32) {
        match *self {
            Param::Gain(b) => eq.bands[b].gain_db = v.clamp(-MAX_GAIN, MAX_GAIN),
            Param::Freq(b) => eq.bands[b].freq = v.clamp(PE_FREQ_RANGE[b].0, PE_FREQ_RANGE[b].1),
        }
    }
}

#[derive(Debug, Clone)]
struct Search {
    param: Param,
    /// dB for gain, multiplicative factor for frequency.
    step: f32,
    dir: f32,
    accepted_any: bool,
    pushes: u32,
    max_pushes: u32,
}

impl Search {
    fn new(param: Param, step: f32, max_pushes: u32) -> Search {
        Search { param, step, dir: 1.0, accepted_any: false, pushes: 0, max_pushes }
    }

    fn propose(&self, eq: &PersonalEq) -> Option<PersonalEq> {
        let cur = self.param.get(eq);
        let next = match self.param {
            Param::Gain(_) => cur + self.dir * self.step,
            Param::Freq(_) => cur * self.step.powf(self.dir),
        };
        let mut c = eq.clone();
        self.param.set(&mut c, next);
        let moved = self.param.get(&c);
        ((moved - cur).abs() > 0.01 * cur.abs().max(1.0)).then_some(c)
    }
}

#[derive(Debug, Clone)]
enum Task {
    Search(Search),
    /// Re-ask an earlier accepted change by offering to undo it.
    Check { param: Param, before: f32 },
}

pub struct Tuner {
    pub current: PersonalEq,
    start: PersonalEq,
    stage: u8,
    queue: VecDeque<Task>,
    active: Option<(Task, PersonalEq)>,
    /// Which side (0 = A, 1 = B) holds the candidate in the active trial.
    pub cand_side: usize,
    pub trials: usize,
    accepted: Vec<(Param, f32, f32)>,
    pub reversals: usize,
    pub checks: usize,
    rng: u64,
    pub done: bool,
}

impl Tuner {
    pub fn new(mut start: PersonalEq, seed: u64) -> Tuner {
        start.enable = true;
        let mut t = Tuner {
            current: start.clone(),
            start,
            stage: 0,
            queue: VecDeque::new(),
            active: None,
            cand_side: 0,
            trials: 0,
            accepted: vec![],
            reversals: 0,
            checks: 0,
            rng: seed | 1,
            done: false,
        };
        t.advance();
        t
    }

    fn coin(&mut self) -> usize {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 33) as usize & 1
    }

    fn fill_stage(&mut self) -> bool {
        self.stage += 1;
        match self.stage {
            1 => BAND_ORDER.iter().for_each(|&b| self.queue.push_back(Task::Search(Search::new(Param::Gain(b), 3.0, 3)))),
            2 => {
                for &b in &BAND_ORDER {
                    if (self.current.bands[b].gain_db - self.start.bands[b].gain_db).abs() >= 3.0 || self.current.bands[b].gain_db.abs() >= 3.0 {
                        self.queue.push_back(Task::Search(Search::new(Param::Freq(b), 1.5, 2)));
                    }
                }
            }
            3 => BAND_ORDER.iter().for_each(|&b| self.queue.push_back(Task::Search(Search::new(Param::Gain(b), 1.5, 2)))),
            4 => {
                // The two largest accepted moves get asked again.
                let mut acc = self.accepted.clone();
                acc.sort_by(|a, b| {
                    let d = |x: &(Param, f32, f32)| match x.0 {
                        Param::Gain(_) => (x.2 - x.1).abs(),
                        Param::Freq(_) => 3.0 * (x.2 / x.1).ln().abs(),
                    };
                    d(b).total_cmp(&d(a))
                });
                for (param, before, _) in acc.into_iter().take(2) {
                    self.queue.push_back(Task::Check { param, before });
                }
            }
            _ => return false,
        }
        true
    }

    /// Move to the next trial that actually changes something, or finish.
    fn advance(&mut self) {
        loop {
            let Some(task) = self.queue.pop_front() else {
                if self.fill_stage() {
                    continue;
                }
                self.active = None;
                self.done = true;
                return;
            };
            let cand = match &task {
                Task::Search(s) => s.propose(&self.current),
                Task::Check { param, before } => {
                    let mut c = self.current.clone();
                    param.set(&mut c, *before);
                    (c != self.current).then_some(c)
                }
            };
            if let Some(c) = cand {
                self.cand_side = self.coin();
                self.active = Some((task, c));
                return;
            }
        }
    }

    /// EQs for sides A and B. When finished both are the result.
    pub fn sides(&self) -> [PersonalEq; 2] {
        match &self.active {
            Some((_, cand)) if self.cand_side == 0 => [cand.clone(), self.current.clone()],
            Some((_, cand)) => [self.current.clone(), cand.clone()],
            None => [self.current.clone(), self.current.clone()],
        }
    }

    pub fn answer(&mut self, choice: Choice) {
        let Some((task, cand)) = self.active.take() else { return };
        self.trials += 1;
        let took = match choice {
            Choice::A => self.cand_side == 0,
            Choice::B => self.cand_side == 1,
            Choice::Same => false,
        };
        match task {
            Task::Search(mut s) => {
                if took {
                    self.accepted.push((s.param, s.param.get(&self.current), s.param.get(&cand)));
                    self.current = cand;
                    s.accepted_any = true;
                    s.pushes += 1;
                    if s.pushes < s.max_pushes {
                        self.queue.push_front(Task::Search(s));
                    }
                } else if !s.accepted_any && s.dir > 0.0 {
                    s.dir = -1.0;
                    self.queue.push_front(Task::Search(s));
                }
            }
            Task::Check { .. } => {
                self.checks += 1;
                if took {
                    // Preferred the undo: the earlier decision didn't hold up.
                    self.reversals += 1;
                    self.current = cand;
                }
            }
        }
        self.advance();
    }

    /// Stop early: keep whatever has been accepted so far.
    pub fn finish(&mut self) {
        self.active = None;
        self.queue.clear();
        self.stage = 99;
        self.done = true;
    }

    /// (trials answered, rough expected total).
    pub fn progress(&self) -> (usize, usize) {
        let remaining_stages: usize = match self.stage {
            0 | 1 => 3 + 5 + 2,
            2 => 5 + 2,
            3 => 2,
            _ => 0,
        };
        let pending = self.queue.len() * 2 + self.active.is_some() as usize + remaining_stages;
        (self.trials, self.trials + pending)
    }

    pub fn low_confidence(&self) -> bool {
        self.checks > 0 && self.reversals * 2 >= self.checks.max(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: &PersonalEq, b: &PersonalEq) -> f32 {
        let n = 40;
        let s: f32 = (0..n)
            .map(|i| {
                let f = 30.0 * (16000.0f32 / 30.0).powf(i as f32 / (n - 1) as f32);
                (a.response_db(f) - b.response_db(f)).powi(2)
            })
            .sum();
        (s / n as f32).sqrt()
    }

    /// A listener who prefers whichever side is closer to a hidden target, makes
    /// mistakes `err` of the time, and can't hear differences under 0.3 dB.
    fn session(target: &PersonalEq, seed: u64, err: f32) -> (Tuner, f32) {
        let mut t = Tuner::new(PersonalEq::default(), seed);
        let mut rng = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
        while !t.done {
            let [a, b] = t.sides();
            let (da, db) = (dist(&a, target), dist(&b, target));
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let wrong = (rng >> 40) as f32 / (1u64 << 24) as f32 <= err;
            let mut c = if (da - db).abs() < 0.3 {
                Choice::Same
            } else if da < db {
                Choice::A
            } else {
                Choice::B
            };
            if wrong {
                c = match c {
                    Choice::A => Choice::B,
                    _ => Choice::A,
                };
            }
            t.answer(c);
            assert!(t.trials < 80, "runaway session");
        }
        let d = dist(&t.current, target);
        (t, d)
    }

    fn target() -> PersonalEq {
        let mut eq = PersonalEq::default();
        eq.bands[0].gain_db = 6.0;
        eq.bands[0].freq = 100.0;
        eq.bands[3].gain_db = -3.0;
        eq.bands[4].gain_db = 3.0;
        eq
    }

    #[test]
    fn perfect_listener_converges() {
        let (t, d) = session(&target(), 42, 0.0);
        assert!(d < 1.5, "distance {d} after {} trials: {:?}", t.trials, t.current);
        assert!(t.trials <= 35, "{} trials", t.trials);
        assert!(!t.low_confidence());
    }

    #[test]
    fn noisy_listener_mostly_converges() {
        let t = target();
        let start = dist(&PersonalEq::default(), &t);
        let good = (1..=20).filter(|&seed| session(&t, seed, 0.1).1 < 2.0).count();
        assert!(good >= 14, "only {good}/20 noisy sessions got within 2 dB (start {start:.1} dB)");
    }

    #[test]
    fn sides_are_blind_and_finish_keeps_progress() {
        let mut t = Tuner::new(PersonalEq::default(), 7);
        let mut seen = [0; 2];
        for _ in 0..6 {
            seen[t.cand_side] += 1;
            let pick = if t.cand_side == 0 { Choice::A } else { Choice::B };
            t.answer(pick);
        }
        assert!(seen[0] > 0 && seen[1] > 0, "candidate side should vary: {seen:?}");
        let cur = t.current.clone();
        assert!(!cur.is_flat());
        t.finish();
        assert!(t.done && t.current == cur);
        let [a, b] = t.sides();
        assert_eq!(a, b);
    }
}
