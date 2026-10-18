//! Real-music excerpts for the A/B tuner, embedded in the binary.
//! Credits and licenses: samples/CREDITS.md.

pub struct Sample {
    pub id: &'static str,
    pub title: &'static str,
    pub artist: &'static str,
    pub style: &'static str,
    pub license: &'static str,
    /// Where the excerpt starts in the original track, seconds.
    pub start_s: f32,
    pub data: &'static [u8],
}

pub const SAMPLES: [Sample; 3] = [
    Sample {
        id: "night-owl",
        title: "Night Owl",
        artist: "Broke For Free",
        style: "electronic, punchy kick",
        license: "CC BY 3.0",
        start_s: 71.0,
        data: include_bytes!("../samples/night-owl.opus"),
    },
    Sample {
        id: "tetanus",
        title: "Like Tetanus In A Wound",
        artist: "Hussalonia",
        style: "indie pop-rock band with vocals",
        license: "CC BY 4.0",
        start_s: 108.0,
        data: include_bytes!("../samples/tetanus.opus"),
    },
    Sample {
        id: "aces-high",
        title: "Aces High",
        artist: "Kevin MacLeod",
        style: "funk band instrumental, live drums",
        license: "CC BY 3.0",
        start_s: 92.0,
        data: include_bytes!("../samples/aces-high.opus"),
    },
];

pub fn find(id: &str) -> Option<&'static Sample> {
    SAMPLES.iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::{octave_energy, Clip, FS};

    #[test]
    fn samples_decode_full_range_and_are_credited() {
        let credits = include_str!("../samples/CREDITS.md");
        for s in &SAMPLES {
            assert!(credits.contains(s.title) && credits.contains(s.artist) && credits.contains(s.license), "{} not credited", s.id);
            let c = Clip::from_sample(s).unwrap();
            let secs = c.l.len() as f32 / FS;
            assert!((9.9..10.2).contains(&secs), "{}: {secs} s", s.id);
            assert!(c.tail > 0 && c.tail <= 2400, "{}: tail {}", s.id, c.tail);
            let bands = octave_energy(&c.l, &c.r, FS);
            let loudest = bands.iter().cloned().fold(f32::MIN, f32::max);
            assert!(bands.iter().all(|b| *b > loudest - 30.0), "{}: {:?}", s.id, bands.map(|b| (b - loudest).round()));
        }
    }
}
