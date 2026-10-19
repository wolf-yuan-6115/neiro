//! User-facing parameters. A profile from the speaker tuning supplies the
//! factory defaults; edits are stored as full per-profile overrides.

use crate::tuning::{Profile, Tuning, NBANDS};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Widener {
    pub enable: bool,
    /// Extra side-signal gain above 300 Hz, 0..1 (+0..+6 dB).
    pub width: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeqFilter {
    pub enabled: bool,
    /// "peak", "notch", "lowshelf", "highshelf", "lowpass" or "highpass"
    /// (the pass filters are Butterworth of `order` 2, 4, 6 or 8).
    #[serde(default = "peak")]
    pub kind: String,
    #[serde(default = "two")]
    pub order: u32,
    pub f0: f32,
    pub gain_db: f32,
    pub q: f32,
}

fn peak() -> String {
    "peak".into()
}

fn two() -> u32 {
    2
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Peq {
    pub enable: bool,
    /// Left speaker's filters, also used for the right unless `right` is set.
    pub filters: Vec<PeqFilter>,
    /// Right speaker's filters when the tuning gives it different ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<Vec<PeqFilter>>,
}

/// DAX3 speaker-PEQ type id to neiro's filter kind.
fn peq_kind(kind: u32) -> &'static str {
    match kind {
        1 => "peak",
        2 => "notch",
        3 => "highshelf",
        4 => "lowshelf",
        6 | 8 => "lowpass",
        7 | 9 => "highpass",
        _ => "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VirtualBass {
    pub enable: bool,
    /// Overall harmonic gain in dB.
    pub gain_db: f32,
    pub src_lo: f32,
    pub src_hi: f32,
    pub mix_lo: f32,
    pub mix_hi: f32,
    /// Gains of 2nd, 3rd, 4th harmonic in dB (Dolby `virtual-bass-subgains`).
    pub h2_db: f32,
    pub h3_db: f32,
    pub h4_db: f32,
    /// High-pass the dry signal so the driver isn't asked for fundamentals it can't play.
    pub hp_enable: bool,
    pub hp_freq: f32,
}

/// Loudness compensation for the low end: the quieter the playback, the more
/// the low bands are lifted, the way DAX on Windows keeps bass at low volume.
/// Not in the tuning file; speaker-tuning-to-easyeffects measured DAX boosting
/// quiet sustained bass far beyond what the XML describes. The playback level is
/// read from the bands above 200 Hz after the app/sink volume, so bass-heavy
/// content doesn't lower its own boost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BassLoudness {
    pub enable: bool,
    /// Largest boost, in dB.
    pub max_db: f32,
    /// Playback level (dB re a full-scale sine) at and above which there is no boost.
    pub ref_db: f32,
    /// dB of boost per dB of level below `ref_db`.
    pub slope: f32,
    /// Shelf corner: a band at this frequency gets half the boost.
    pub corner_hz: f32,
    /// Level averaging time in seconds.
    pub time_s: f32,
}

impl Default for BassLoudness {
    fn default() -> Self {
        BassLoudness { enable: false, max_db: 9.0, ref_db: -12.0, slope: 0.5, corner_hz: 150.0, time_s: 2.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ieq {
    pub enable: bool,
    /// "balanced" | "warm" | "detailed"
    pub curve: String,
    /// Dolby scale 0..16.
    pub amount: f32,
    /// Max correction per band in dB.
    pub max_db: f32,
    /// Spectrum averaging time constant in seconds.
    pub time_s: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Optimizer {
    pub enable: bool,
    pub left: [f32; NBANDS],
    pub right: [f32; NBANDS],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Volmax {
    pub enable: bool,
    pub boost_db: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MbcGroup {
    /// Exclusive end band; the group starts where the previous one ended.
    pub end_band: usize,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mbc {
    pub enable: bool,
    pub groups: Vec<MbcGroup>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regulator {
    pub enable: bool,
    /// Offset added to every threshold (positive = louder before limiting).
    pub overdrive_db: f32,
    /// Fraction of a band's gain reduction shared with its non-isolated neighbours.
    pub timbre: f32,
    /// Soft-knee width below each threshold (Dolby uses threshold_high - threshold_low = 12 dB).
    pub knee_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub threshold: [f32; NBANDS],
    pub isolated: [bool; NBANDS],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limiter {
    pub enable: bool,
    pub ceiling_db: f32,
    pub release_ms: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Params {
    pub pregain_db: f32,
    pub widener: Widener,
    pub peq: Peq,
    pub vbass: VirtualBass,
    /// Added after the first release; older saved overrides lack it.
    #[serde(default)]
    pub loudness: BassLoudness,
    pub ieq: Ieq,
    pub optimizer: Optimizer,
    pub volmax: Volmax,
    pub mbc: Mbc,
    pub regulator: Regulator,
    pub limiter: Limiter,
    pub postgain_db: f32,
}

impl Params {
    pub fn from_profile(p: &Profile) -> Params {
        let off = p.name == "off";
        Params {
            pregain_db: p.pregain_db,
            widener: Widener { enable: p.virtualizer && !off, width: (p.surround_boost_db / 12.0).clamp(0.0, 1.0) },
            peq: {
                let conv = |fs: &[crate::tuning::PeqFilter]| -> Vec<PeqFilter> {
                    fs.iter()
                        .filter(|f| peq_kind(f.kind) != "unknown")
                        .map(|f| PeqFilter {
                            enabled: f.enabled,
                            kind: peq_kind(f.kind).into(),
                            order: f.order,
                            f0: f.f0,
                            gain_db: f.gain_db,
                            q: f.q,
                        })
                        .collect()
                };
                let (left, right) = (conv(&p.peq[0]), conv(&p.peq[1]));
                Peq { enable: p.peq_enable && !off, right: (right != left).then_some(right), filters: left }
            },
            vbass: VirtualBass {
                enable: false,
                gain_db: 0.0,
                src_lo: p.vb_src_freqs.0.max(30.0),
                src_hi: p.vb_src_freqs.1,
                mix_lo: p.vb_mix_freqs.0,
                mix_hi: p.vb_mix_freqs.1,
                h2_db: -2.0,
                h3_db: -9.0,
                h4_db: -12.0,
                hp_enable: false,
                hp_freq: 120.0,
            },
            loudness: BassLoudness::default(),
            ieq: Ieq {
                enable: p.ieq_enable && !off,
                curve: p.ieq_preset.clone(),
                amount: p.ieq_amount,
                max_db: 8.0,
                time_s: 3.0,
            },
            optimizer: Optimizer { enable: p.optimizer_enable && !off, left: p.optimizer[0], right: p.optimizer[1] },
            volmax: Volmax { enable: p.volmax_boost_db > 0.0 && !off, boost_db: p.volmax_boost_db },
            mbc: Mbc {
                enable: p.mbc_enable && !off,
                groups: p
                    .mbc_groups
                    .iter()
                    .map(|g| MbcGroup {
                        end_band: g.end_band,
                        threshold_db: g.threshold_db,
                        ratio: 1.0 / g.slope.max(1e-3),
                        attack_ms: g.attack_ms,
                        release_ms: g.release_ms,
                        makeup_db: g.makeup_db,
                    })
                    .collect(),
            },
            regulator: Regulator {
                enable: p.regulator_enable && p.regulator_dist_enable && !off,
                overdrive_db: p.regulator_overdrive_db,
                timbre: p.regulator_timbre,
                knee_db: {
                    let k = p.threshold_high[0] - p.threshold_low[0];
                    if k > 0.0 { k } else { 12.0 }
                },
                attack_ms: 5.0,
                release_ms: 150.0,
                threshold: p.threshold_high,
                isolated: p.isolated,
            },
            limiter: Limiter { enable: !off, ceiling_db: p.peak_level_db.min(-1.0), release_ms: 80.0 },
            postgain_db: p.postgain_db,
        }
    }

    /// Set a value by JSON pointer (e.g. `/regulator/threshold/4`).
    pub fn set(&mut self, path: &str, value: serde_json::Value) -> Result<()> {
        let mut v = serde_json::to_value(&*self)?;
        let slot = v.pointer_mut(path).ok_or_else(|| anyhow!("no such parameter {path}"))?;
        *slot = value;
        *self = serde_json::from_value(v).with_context(|| format!("invalid value for {path}"))?;
        Ok(())
    }

    pub fn get(&self, path: &str) -> Option<serde_json::Value> {
        serde_json::to_value(self).ok()?.pointer(path).cloned()
    }
}

// ---------------------------------------------------------------------------
// Personal EQ: the user's own taste, applied on top of every Dolby profile.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeKind {
    LowShelf,
    Peak,
    HighShelf,
}

pub const PE_BANDS: usize = 5;
pub const PE_KINDS: [PeKind; PE_BANDS] = [PeKind::LowShelf, PeKind::Peak, PeKind::Peak, PeKind::Peak, PeKind::HighShelf];
pub const PE_NAMES: [&str; PE_BANDS] = ["Bass shelf", "Low-mid", "Mid", "Presence", "Treble shelf"];
pub const PE_FREQ_RANGE: [(f32, f32); PE_BANDS] = [(30.0, 300.0), (150.0, 600.0), (500.0, 2500.0), (2000.0, 6000.0), (4000.0, 16000.0)];
pub const PE_DEFAULT_FREQ: [f32; PE_BANDS] = [120.0, 300.0, 1000.0, 3500.0, 8000.0];
pub const PE_MAX_GAIN: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PeBand {
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonalEq {
    pub enable: bool,
    pub pregain_db: f32,
    pub bands: [PeBand; PE_BANDS],
}

impl Default for PersonalEq {
    fn default() -> Self {
        PersonalEq {
            enable: true,
            pregain_db: 0.0,
            bands: std::array::from_fn(|i| PeBand {
                freq: PE_DEFAULT_FREQ[i],
                gain_db: 0.0,
                q: if PE_KINDS[i] == PeKind::Peak { 1.0 } else { std::f32::consts::FRAC_1_SQRT_2 },
            }),
        }
    }
}

impl PersonalEq {
    #[cfg(test)]
    pub fn is_flat(&self) -> bool {
        self.bands.iter().all(|b| b.gain_db.abs() < 1e-3) && self.pregain_db.abs() < 1e-3
    }

    pub fn coefs(&self, fs: f64) -> [crate::dsp::biquad::Coefs; PE_BANDS] {
        use crate::dsp::biquad::Coefs;
        std::array::from_fn(|i| {
            let b = &self.bands[i];
            let (f, q, g) = (b.freq as f64, b.q as f64, b.gain_db as f64);
            match PE_KINDS[i] {
                PeKind::LowShelf => Coefs::lowshelf(fs, f, q, g),
                PeKind::Peak => Coefs::peaking(fs, f, q, g),
                PeKind::HighShelf => Coefs::highshelf(fs, f, q, g),
            }
        })
    }

    /// Combined magnitude response in dB (bands only, without pre-gain).
    pub fn response_db(&self, f: f32) -> f32 {
        if !self.enable {
            return 0.0;
        }
        self.coefs(48000.0).iter().map(|c| c.response_db(48000.0, f as f64) as f32).sum()
    }
}

/// Persistent daemon state, stored in `~/.config/neiro/state.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Saved {
    pub profile: Option<String>,
    pub bypass: bool,
    /// Full parameter sets for profiles the user customised.
    #[serde(default)]
    pub overrides: BTreeMap<String, Params>,
    /// Optional path to a different tuning.xml.
    pub tuning_path: Option<String>,
    /// Hardware speaker volume to pin while the daemon runs (0..1).
    pub hw_volume: Option<f32>,
    /// The user's own EQ, applied on top of every profile.
    #[serde(default)]
    pub personal: PersonalEq,
    /// Tuning operating mode (convertible poses such as "tablet"); default "normal".
    pub mode: Option<String>,
    /// PipeWire node name of the built-in speaker sink, if auto-detection picks the wrong one.
    pub speaker_sink: Option<String>,
}

/// The project used to be called "dax": carry its saved state over once.
fn migrate_old_config() {
    let new = state_path();
    let Some(old) = dirs::config_dir().map(|d| d.join("dax").join("state.toml")) else { return };
    if !new.exists() && old.exists() {
        if let Some(dir) = new.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::copy(&old, &new).is_ok() {
            log::info!("copied settings from {}", old.display());
        }
    }
}

pub fn state_path() -> std::path::PathBuf {
    dirs::config_dir().unwrap_or_else(|| ".".into()).join("neiro").join("state.toml")
}

impl Saved {
    pub fn load() -> Saved {
        migrate_old_config();
        std::fs::read_to_string(state_path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn store(&self) -> Result<()> {
        let p = state_path();
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(&p, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// Factory parameters for a profile. "off" exists even when the tuning has no
/// such profile: it is then derived from the first one with every stage disabled.
pub fn factory(t: &Tuning, profile: &str) -> Result<Params> {
    if let Some(p) = t.profile(profile) {
        return Ok(Params::from_profile(p));
    }
    if profile == "off" {
        let mut p = t.profiles[0].clone();
        p.name = "off".into();
        return Ok(Params::from_profile(&p));
    }
    Err(anyhow!("this tuning has no profile named {profile}"))
}

// ---------------------------------------------------------------------------
// Parameter descriptors used by the TUI.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Kind {
    Bool,
    Float { min: f32, max: f32, step: f32, unit: String },
    Choice(Vec<String>),
    /// A button: the TUI runs the named action instead of editing a value.
    Action(String),
}

pub const ACTION_TUNER: &str = "tuner";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spec {
    pub path: String,
    pub label: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub enable: Option<String>,
    pub items: Vec<Spec>,
}

fn f(path: impl Into<String>, label: impl Into<String>, min: f32, max: f32, step: f32, unit: &str) -> Spec {
    Spec { path: path.into(), label: label.into(), kind: Kind::Float { min, max, step, unit: unit.into() } }
}

fn b(path: impl Into<String>, label: impl Into<String>) -> Spec {
    Spec { path: path.into(), label: label.into(), kind: Kind::Bool }
}

fn sec(name: &str, enable: Option<&str>, items: Vec<Spec>) -> Section {
    Section { name: name.into(), enable: enable.map(Into::into), items }
}

/// Descriptor paths under this prefix address `Saved::personal`, not the profile's `Params`.
pub const PERSONAL_PREFIX: &str = "/personal";

pub fn sections(p: &Params, band_freqs: &[f32; NBANDS]) -> Vec<Section> {
    let hz = |v: f32| if v >= 1000.0 { format!("{:.1}k", v / 1000.0) } else { format!("{v:.0}") };
    let mut peq = vec![];
    let sides: Vec<(&str, &str, &Vec<PeqFilter>)> = match &p.peq.right {
        Some(r) => vec![("filters", "L", &p.peq.filters), ("right", "R", r)],
        None => vec![("filters", "", &p.peq.filters)],
    };
    for (key, side, list) in sides {
        for (i, flt) in list.iter().enumerate() {
            let at = |field: &str| format!("/peq/{key}/{i}/{field}");
            let name = format!("{side}#{i} {}", flt.kind);
            peq.push(b(at("enabled"), format!("{name} on")));
            peq.push(f(at("f0"), format!("{side}#{i} freq"), 20.0, 20000.0, 5.0, "Hz"));
            match flt.kind.as_str() {
                "highpass" | "lowpass" => peq.push(f(at("order"), format!("{side}#{i} order"), 2.0, 8.0, 2.0, "")),
                "notch" => peq.push(f(at("q"), format!("{side}#{i} Q"), 0.1, 20.0, 0.1, "")),
                _ => {
                    peq.push(f(at("gain_db"), format!("{side}#{i} gain"), -24.0, 12.0, 0.5, "dB"));
                    peq.push(f(at("q"), format!("{side}#{i} Q"), 0.1, 20.0, 0.1, ""));
                }
            }
        }
    }
    let mut mbc = vec![];
    for (i, _) in p.mbc.groups.iter().enumerate() {
        mbc.push(f(format!("/mbc/groups/{i}/threshold_db"), format!("G{i} threshold"), -60.0, 0.0, 0.5, "dB"));
        mbc.push(f(format!("/mbc/groups/{i}/ratio"), format!("G{i} ratio"), 1.0, 20.0, 0.1, ":1"));
        mbc.push(f(format!("/mbc/groups/{i}/attack_ms"), format!("G{i} attack"), 1.0, 200.0, 1.0, "ms"));
        mbc.push(f(format!("/mbc/groups/{i}/release_ms"), format!("G{i} release"), 10.0, 1000.0, 10.0, "ms"));
        mbc.push(f(format!("/mbc/groups/{i}/makeup_db"), format!("G{i} makeup"), -12.0, 12.0, 0.5, "dB"));
    }
    let mut reg = vec![
        f("/regulator/overdrive_db", "Overdrive", -24.0, 24.0, 0.5, "dB"),
        f("/regulator/timbre", "Timbre preserve", 0.0, 1.0, 0.05, ""),
        f("/regulator/knee_db", "Knee", 0.0, 24.0, 0.5, "dB"),
        f("/regulator/attack_ms", "Attack", 1.0, 100.0, 1.0, "ms"),
        f("/regulator/release_ms", "Release", 10.0, 2000.0, 10.0, "ms"),
    ];
    for (i, fr) in band_freqs.iter().enumerate() {
        reg.push(f(format!("/regulator/threshold/{i}"), format!("{:>5}Hz thr", hz(*fr)), -60.0, 6.0, 0.5, "dB"));
        reg.push(b(format!("/regulator/isolated/{i}"), format!("{:>5}Hz isolated", hz(*fr))));
    }
    let mut opt = vec![];
    for (i, fr) in band_freqs.iter().enumerate() {
        opt.push(f(format!("/optimizer/left/{i}"), format!("{:>5}Hz L", hz(*fr)), -12.0, 12.0, 0.25, "dB"));
        opt.push(f(format!("/optimizer/right/{i}"), format!("{:>5}Hz R", hz(*fr)), -12.0, 12.0, 0.25, "dB"));
    }
    let mut personal = vec![
        Spec { path: String::new(), label: "Find my EQ: blind A/B test".into(), kind: Kind::Action(ACTION_TUNER.into()) },
        f("/personal/pregain_db", "Pre-gain", -12.0, 12.0, 0.5, "dB"),
    ];
    for i in 0..PE_BANDS {
        let (lo, hi) = PE_FREQ_RANGE[i];
        let name = PE_NAMES[i];
        personal.push(f(format!("/personal/bands/{i}/gain_db"), format!("{name} gain"), -PE_MAX_GAIN, PE_MAX_GAIN, 0.5, "dB"));
        personal.push(f(format!("/personal/bands/{i}/freq"), format!("{name} freq"), lo, hi, if hi > 3000.0 { 100.0 } else { 5.0 }, "Hz"));
        if PE_KINDS[i] == PeKind::Peak {
            personal.push(f(format!("/personal/bands/{i}/q"), format!("{name} Q"), 0.3, 4.0, 0.1, ""));
        }
    }
    vec![
        sec("Personal EQ", Some("/personal/enable"), personal),
        sec("Gain", None, vec![
            f("/pregain_db", "Pre-gain", -24.0, 12.0, 0.5, "dB"),
            f("/postgain_db", "Post-gain", -24.0, 12.0, 0.5, "dB"),
        ]),
        sec("Widener", Some("/widener/enable"), vec![f("/widener/width", "Width", 0.0, 1.0, 0.05, "")]),
        sec("Speaker PEQ", Some("/peq/enable"), peq),
        sec("Virtual bass", Some("/vbass/enable"), vec![
            f("/vbass/gain_db", "Harmonics gain", -24.0, 12.0, 0.5, "dB"),
            f("/vbass/src_lo", "Source low", 20.0, 200.0, 5.0, "Hz"),
            f("/vbass/src_hi", "Source high", 60.0, 400.0, 5.0, "Hz"),
            f("/vbass/mix_lo", "Mix low", 60.0, 600.0, 5.0, "Hz"),
            f("/vbass/mix_hi", "Mix high", 150.0, 2000.0, 10.0, "Hz"),
            f("/vbass/h2_db", "2nd harmonic", -40.0, 6.0, 0.5, "dB"),
            f("/vbass/h3_db", "3rd harmonic", -40.0, 6.0, 0.5, "dB"),
            f("/vbass/h4_db", "4th harmonic", -40.0, 6.0, 0.5, "dB"),
            b("/vbass/hp_enable", "Dry high-pass"),
            f("/vbass/hp_freq", "Dry HP freq", 40.0, 300.0, 5.0, "Hz"),
        ]),
        sec("Bass loudness", Some("/loudness/enable"), vec![
            f("/loudness/max_db", "Max boost", 0.0, 18.0, 0.5, "dB"),
            f("/loudness/ref_db", "Full-volume level", -40.0, 0.0, 1.0, "dB"),
            f("/loudness/slope", "Boost per dB quieter", 0.05, 1.5, 0.05, ""),
            f("/loudness/corner_hz", "Corner", 60.0, 400.0, 5.0, "Hz"),
            f("/loudness/time_s", "Averaging time", 0.2, 10.0, 0.1, "s"),
        ]),
        sec("Intelligent EQ", Some("/ieq/enable"), vec![
            Spec {
                path: "/ieq/curve".into(),
                label: "Curve".into(),
                kind: Kind::Choice(vec!["balanced".into(), "warm".into(), "detailed".into()]),
            },
            f("/ieq/amount", "Amount", 0.0, 16.0, 1.0, "/16"),
            f("/ieq/max_db", "Max correction", 0.0, 16.0, 0.5, "dB"),
            f("/ieq/time_s", "Averaging time", 0.2, 20.0, 0.2, "s"),
        ]),
        sec("Audio optimizer", Some("/optimizer/enable"), opt),
        sec("Volmax", Some("/volmax/enable"), vec![f("/volmax/boost_db", "Boost", 0.0, 12.0, 0.5, "dB")]),
        sec("MB compressor", Some("/mbc/enable"), mbc),
        sec("Regulator", Some("/regulator/enable"), reg),
        sec("Limiter", Some("/limiter/enable"), vec![
            f("/limiter/ceiling_db", "Ceiling", -12.0, 0.0, 0.1, "dB"),
            f("/limiter/release_ms", "Release", 10.0, 1000.0, 10.0, "ms"),
        ]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_saved_before_bass_loudness_still_load() {
        let t = crate::tuning::test_tuning();
        let mut v = serde_json::to_value(factory(&t, "music").unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("loudness");
        let saved = Saved { overrides: [("music".into(), serde_json::from_value(v).unwrap())].into(), ..Default::default() };
        let text = toml::to_string(&saved).unwrap();
        let back: Saved = toml::from_str(&text).unwrap();
        assert_eq!(back.overrides["music"].loudness, BassLoudness::default());
    }

    #[test]
    fn set_by_path() {
        let t = crate::tuning::test_tuning();
        let mut p = factory(&t, "music").unwrap();
        assert!(p.peq.right.is_some(), "fixture's speakers differ, so both sides are exposed");
        assert_eq!(p.peq.filters[1].kind, "notch");
        p.set("/regulator/threshold/4", serde_json::json!(-10.0)).unwrap();
        assert_eq!(p.regulator.threshold[4], -10.0);
        p.set("/vbass/enable", serde_json::json!(true)).unwrap();
        assert!(p.vbass.enable);
        assert!(p.set("/nope", serde_json::json!(1)).is_err());
        assert!(p.set("/vbass/enable", serde_json::json!("x")).is_err());
        assert_eq!(p.regulator.knee_db, 12.0);
        // every descriptor path resolves
        for s in sections(&p, &t.band_freqs) {
            let values = s.items.iter().filter(|i| !matches!(i.kind, Kind::Action(_)));
            for i in values.map(|i| &i.path).chain(s.enable.iter()) {
                if let Some(rest) = i.strip_prefix(PERSONAL_PREFIX) {
                    let pe = serde_json::to_value(PersonalEq::default()).unwrap();
                    assert!(pe.pointer(rest).is_some(), "{i}");
                } else {
                    assert!(p.get(i).is_some(), "{i}");
                }
            }
        }
        // "off" is synthesised when a tuning lacks it, with everything disabled.
        let tablet = crate::tuning::parse(crate::tuning::TEST_XML, Some("tablet")).unwrap();
        let off = factory(&tablet, "off").unwrap();
        assert!(!off.peq.enable && !off.mbc.enable && !off.limiter.enable);
        assert!(factory(&tablet, "movie").is_err());
    }
}
