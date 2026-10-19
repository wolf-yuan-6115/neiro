//! Parser for Dolby DAX3 speaker tunings (`*.xml`) shipped in Windows audio
//! driver packages.
//!
//! Most numeric values in the file are fixed point in 1/16 dB steps; they are
//! converted to plain dB here so the rest of the code never sees raw units.
//! The schema varies between devices and driver generations, so everything a
//! profile needs falls back to a neutral default when an element is missing.

use anyhow::{anyhow, bail, Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const NBANDS: usize = 20;

/// Dolby's 20-band layout at 48 kHz, used if a file lacks `band_20_freq`.
const DEFAULT_BANDS: [f32; NBANDS] = [
    47.0, 141.0, 234.0, 328.0, 469.0, 656.0, 844.0, 1031.0, 1313.0, 1688.0, 2250.0, 3000.0, 3750.0, 4688.0, 5813.0,
    7125.0, 9000.0, 11250.0, 13875.0, 19688.0,
];

/// Profiles shown first, in this order, when a tuning has them.
const CANONICAL: [&str; 6] = ["dynamic", "movie", "music", "game", "voice", "off"];

#[derive(Debug, Clone)]
struct Node {
    name: String,
    attrs: HashMap<String, String>,
    children: Vec<Node>,
}

impl Node {
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
    fn path(&self, path: &str) -> Option<&Node> {
        path.split('/').try_fold(self, |n, p| n.child(p))
    }
    fn attr(&self, a: &str) -> Option<&str> {
        self.attrs.get(a).map(|s| s.as_str())
    }
}

fn parse_tree(xml: &str) -> Result<Node> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Node> = vec![Node { name: "#root".into(), attrs: HashMap::new(), children: vec![] }];
    let mk = |e: &quick_xml::events::BytesStart| -> Result<Node> {
        let mut attrs = HashMap::new();
        for a in e.attributes() {
            let a = a?;
            let k: &str = a.key.as_ref();
            let k = k.to_string();
            #[allow(deprecated)]
            let v = a.unescape_value()?.into_owned();
            attrs.insert(k, v);
        }
        let qn = e.name();
        let name: &str = qn.as_ref();
        Ok(Node { name: name.to_string(), attrs, children: vec![] })
    };
    loop {
        match reader.read_event()? {
            Event::Start(e) => stack.push(mk(&e)?),
            Event::Empty(e) => {
                let n = mk(&e)?;
                stack.last_mut().unwrap().children.push(n);
            }
            Event::End(_) => {
                let n = stack.pop().ok_or_else(|| anyhow!("unbalanced xml"))?;
                stack.last_mut().ok_or_else(|| anyhow!("unbalanced xml"))?.children.push(n);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        bail!("unbalanced xml");
    }
    Ok(stack.pop().unwrap())
}

/// One speaker-PEQ filter. `kind` is the DAX3 type id:
/// 1 peaking, 2 notch, 3 high-shelf, 4 low-shelf, 6/8 low-pass, 7/9 high-pass.
/// Types 2, 3, 4, 6 and 8 are inferred from their parameters, not documented.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeqFilter {
    pub enabled: bool,
    pub kind: u32,
    pub f0: f32,
    pub gain_db: f32,
    /// Q; for shelves converted from the file's slope `s`.
    pub q: f32,
    pub order: u32,
}

/// One multiband-compressor group: `end_band, threshold, slope(Q15), attack ms, release ms, makeup`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MbcGroup {
    pub end_band: usize,
    pub threshold_db: f32,
    /// Output slope above threshold (1.0 = no compression). Ratio = 1 / slope.
    pub slope: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    // tuning-cp (content processing)
    pub ieq_enable: bool,
    pub ieq_amount: f32,
    pub ieq_preset: String,
    pub surround_boost_db: f32,
    pub virtualizer: bool,
    pub volmax_boost_db: f32,
    pub pregain_db: f32,
    pub postgain_db: f32,
    pub regulator_enable: bool,
    pub vb_src_freqs: (f32, f32),
    pub vb_mix_freqs: (f32, f32),
    // tuning-vlldp (device processing)
    pub optimizer_enable: bool,
    pub optimizer: [[f32; NBANDS]; 2],
    pub peq_enable: bool,
    /// Speaker-PEQ filters per output: [left (speaker 0), right (speaker 1)].
    pub peq: [Vec<PeqFilter>; 2],
    pub mbc_enable: bool,
    pub mbc_groups: Vec<MbcGroup>,
    pub peak_level_db: f32,
    pub regulator_overdrive_db: f32,
    pub regulator_timbre: f32,
    pub regulator_dist_enable: bool,
    pub threshold_high: [f32; NBANDS],
    pub threshold_low: [f32; NBANDS],
    pub isolated: [bool; NBANDS],
}

#[derive(Debug, Clone)]
pub struct Tuning {
    pub band_freqs: [f32; NBANDS],
    pub ieq_curves: HashMap<String, [f32; NBANDS]>,
    pub profiles: Vec<Profile>,
    /// Selected `internal_speaker` operating mode, and all modes the file offers.
    pub mode: String,
    pub modes: Vec<String>,
    /// Speakers the endpoint declares; neiro processes the first two.
    pub channels: u32,
    pub default_profile: Option<String>,
    pub tuning_version: Option<i64>,
}

impl Tuning {
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Profiles to offer: the canonical ones first, then any extras in file
    /// order. "off" is always included, even if the file has none.
    pub fn profile_names(&self) -> Vec<String> {
        let mut out: Vec<String> =
            CANONICAL.iter().filter(|c| **c == "off" || self.profile(c).is_some()).map(|s| s.to_string()).collect();
        for p in &self.profiles {
            if !out.contains(&p.name) {
                out.push(p.name.clone());
            }
        }
        out
    }

    /// Profile to start on: the file's declared default, else music, else the first.
    pub fn initial_profile(&self) -> String {
        let names = self.profile_names();
        self.default_profile
            .clone()
            .filter(|d| names.contains(d))
            .or_else(|| names.iter().find(|n| *n == "music").cloned())
            .unwrap_or_else(|| names[0].clone())
    }
}

fn ints(s: &str) -> Result<Vec<i64>> {
    s.split(',').filter(|v| !v.trim().is_empty()).map(|v| v.trim().parse::<i64>().with_context(|| format!("bad int {v:?}"))).collect()
}

fn arr20(v: &[f32]) -> Result<[f32; NBANDS]> {
    v.try_into().map_err(|_| anyhow!("expected {NBANDS} values, got {}", v.len()))
}

/// Q for an RBJ shelf given its slope `s` and gain.
fn shelf_q(s: f32, gain_db: f32) -> f32 {
    let a = 10f32.powf(gain_db / 40.0);
    let inv = (a + 1.0 / a) * (1.0 / s.max(1e-3) - 1.0) + 2.0;
    if inv > 1e-6 { 1.0 / inv.sqrt() } else { std::f32::consts::FRAC_1_SQRT_2 }
}

/// Parse a tuning, picking the `internal_speaker` endpoint for `mode`
/// (default "normal", else the first one).
pub fn parse(xml: &str, mode: Option<&str>) -> Result<Tuning> {
    let root = parse_tree(xml)?;
    let dd = root.child("device_data").ok_or_else(|| anyhow!("not a DAX3 tuning (no <device_data>)"))?;
    let empty = Node { name: String::new(), attrs: HashMap::new(), children: vec![] };
    let constant = dd.child("constant").unwrap_or(&empty);

    let mut presets: HashMap<String, Vec<i64>> = HashMap::new();
    for c in &constant.children {
        if let Some(t) = c.attr("target") {
            presets.insert(c.name.clone(), ints(t)?);
        }
    }
    let band_freqs = match constant.child("band_20_freq").and_then(|n| n.attr("fs_48000")) {
        Some(b) => arr20(&ints(b)?.iter().map(|&v| v as f32).collect::<Vec<_>>())?,
        None => DEFAULT_BANDS,
    };
    let mut ieq_curves = HashMap::new();
    for (k, v) in &presets {
        if let Some(name) = k.strip_prefix("ieq_") {
            if let Ok(c) = arr20(&v.iter().map(|&x| x as f32 / 16.0).collect::<Vec<_>>()) {
                ieq_curves.insert(name.to_string(), c);
            }
        }
    }

    let speakers: Vec<&Node> =
        dd.children.iter().filter(|n| n.name == "endpoint" && n.attr("type") == Some("internal_speaker")).collect();
    if speakers.is_empty() {
        bail!("no internal_speaker endpoint in this tuning");
    }
    let mode_of = |n: &Node| n.attr("operating_mode").unwrap_or("normal").to_string();
    let mut modes: Vec<String> = vec![];
    for e in &speakers {
        if !modes.contains(&mode_of(e)) {
            modes.push(mode_of(e));
        }
    }
    let endpoint = match mode {
        Some(m) => *speakers.iter().find(|e| mode_of(e) == m).ok_or_else(|| {
            anyhow!("this tuning has no \"{m}\" operating mode (available: {})", modes.join(", "))
        })?,
        None => *speakers.iter().find(|e| mode_of(e) == "normal").unwrap_or(&speakers[0]),
    };
    let channels = endpoint
        .attr("total_count")
        .and_then(|v| v.parse().ok())
        .or_else(|| endpoint.path("speaker-config/output-count").and_then(|n| n.attr("value")).and_then(|v| v.parse().ok()))
        .unwrap_or(2);

    // Value lookup: `value="..."`, `preset="..."` naming a constant, or a
    // per-channel `<ch_00 .../>` sub-element (some SoundWire tunings).
    let raw = |n: &Node| -> Option<Vec<i64>> {
        if let Some(v) = n.attr("value") {
            ints(v).ok()
        } else if let Some(p) = n.attr("preset") {
            presets.get(p).cloned()
        } else {
            n.child("ch_00").and_then(|c| {
                c.attr("value").and_then(|v| ints(v).ok()).or_else(|| c.attr("preset").and_then(|p| presets.get(p).cloned()))
            })
        }
    };
    let get = |n: Option<&Node>, path: &str| -> Option<Vec<i64>> { raw(n?.path(path)?).filter(|v| !v.is_empty()) };
    let one = |n: Option<&Node>, path: &str, d: i64| get(n, path).map_or(d, |v| v[0]);
    let db = |n: Option<&Node>, path: &str| one(n, path, 0) as f32 / 16.0;
    let db20 = |n: Option<&Node>, path: &str, d: f32| -> [f32; NBANDS] {
        get(n, path)
            .and_then(|v| arr20(&v.iter().map(|&x| x as f32 / 16.0).collect::<Vec<_>>()).ok())
            .unwrap_or([d; NBANDS])
    };
    let pair = |n: Option<&Node>, path: &str, d: (f32, f32)| {
        get(n, path).filter(|v| v.len() >= 2).map_or(d, |v| (v[0] as f32, v[1] as f32))
    };

    let mut profiles = vec![];
    for p in endpoint.children.iter().filter(|n| n.name == "profile") {
        let name = p.attr("type").unwrap_or("unnamed").to_string();
        let cp = p.child("tuning-cp");
        let dp = p.child("tuning-vlldp");

        let mut peq: [Vec<PeqFilter>; 2] = [vec![], vec![]];
        let mut speaker_seen = [false; 2];
        if let Some(f) = dp.and_then(|d| d.child("speaker-peq-filters")) {
            for c in &f.children {
                let spk: usize = c.attr("speaker").and_then(|v| v.parse().ok()).unwrap_or(0);
                if spk > 1 {
                    continue;
                }
                let num = |a: &str| c.attr(a).and_then(|v| v.trim().parse::<f32>().ok());
                let (Some(kind), Some(f0)) = (num("type"), num("f0")) else { continue };
                let gain_db = num("gain").unwrap_or(0.0);
                let q = match (num("q"), num("s")) {
                    (Some(q), _) => q,
                    (None, Some(s)) => shelf_q(s, gain_db),
                    _ => std::f32::consts::FRAC_1_SQRT_2,
                };
                speaker_seen[spk] = true;
                peq[spk].push(PeqFilter {
                    enabled: c.attr("enabled") != Some("0"),
                    kind: kind as u32,
                    f0,
                    gain_db,
                    q,
                    order: num("order").map_or(2, |o| o as u32),
                });
            }
        }
        if !speaker_seen[1] {
            // Mono-described tunings apply speaker 0's filters to both outputs.
            peq[1] = peq[0].clone();
        }

        let mbc_t = dp.and_then(|d| d.child("mb-compressor-tuning"));
        let group_count = one(mbc_t, "group_count", 0).clamp(0, 4) as usize;
        let mut mbc_groups = vec![];
        for g in 0..group_count {
            let Some(v) = get(mbc_t, &format!("band_group_{g}")).filter(|v| v.len() >= 6) else { continue };
            mbc_groups.push(MbcGroup {
                end_band: (v[0].max(0) as usize).min(NBANDS),
                threshold_db: v[1] as f32 / 16.0,
                slope: v[2] as f32 / 32768.0,
                attack_ms: v[3] as f32,
                release_ms: v[4] as f32,
                makeup_db: v[5] as f32 / 16.0,
            });
        }

        let mut iso = [false; NBANDS];
        for (i, v) in get(dp, "regulator-tuning/isolated_band").unwrap_or_default().iter().take(NBANDS).enumerate() {
            iso[i] = *v != 0;
        }
        let opt = dp.and_then(|d| d.child("audio-optimizer-bands"));
        let ch = |c: &str| db20(opt, c, 0.0);
        let threshold_high = db20(dp, "regulator-tuning/threshold_high", 0.0);
        let threshold_low = match get(dp, "regulator-tuning/threshold_low") {
            Some(_) => db20(dp, "regulator-tuning/threshold_low", -12.0),
            None => threshold_high.map(|v| v - 12.0),
        };

        profiles.push(Profile {
            ieq_enable: one(cp, "ieq-enable", 0) != 0,
            ieq_amount: one(cp, "ieq-amount", 10) as f32,
            ieq_preset: cp
                .and_then(|c| c.child("ieq-bands-set"))
                .and_then(|n| n.attr("preset"))
                .and_then(|s| s.strip_prefix("ieq_"))
                .unwrap_or("balanced")
                .to_string(),
            surround_boost_db: db(cp, "surround-boost"),
            virtualizer: one(cp, "output-mode/processing_mode", 1) == 11,
            volmax_boost_db: db(cp, "volmax-boost"),
            pregain_db: db(cp, "pregain"),
            postgain_db: db(dp, "postgain"),
            regulator_enable: one(cp, "regulator-enable", 1) != 0,
            vb_src_freqs: pair(cp, "virtual-bass-src-freqs", (35.0, 160.0)),
            vb_mix_freqs: pair(cp, "virtual-bass-mix-freqs", (94.0, 469.0)),
            optimizer_enable: one(dp, "audio-optimizer-enable", 0) != 0,
            optimizer: [ch("ch_00"), if opt.and_then(|o| o.child("ch_01")).is_some() { ch("ch_01") } else { ch("ch_00") }],
            peq_enable: one(dp, "speaker-peq-enable", 1) != 0,
            peq,
            mbc_enable: one(dp, "mb-compressor-enable", 0) != 0 && !mbc_groups.is_empty(),
            mbc_groups,
            peak_level_db: db(dp, "peak-level"),
            regulator_overdrive_db: db(dp, "regulator-overdrive"),
            regulator_timbre: one(dp, "regulator-timbre-preservation", 0) as f32 / 16.0,
            regulator_dist_enable: one(dp, "regulator-speaker-dist-enable", 0) != 0,
            threshold_high,
            threshold_low,
            isolated: iso,
            name,
        });
    }
    if profiles.is_empty() {
        bail!("the \"{}\" endpoint has no profiles", mode_of(endpoint));
    }
    let setting = |k: &str| dd.path(&format!("setting/{k}")).and_then(|n| n.attr("value")).map(String::from);
    Ok(Tuning {
        band_freqs,
        ieq_curves,
        profiles,
        mode: mode_of(endpoint),
        modes,
        channels,
        default_profile: setting("default_profile"),
        tuning_version: dd.child("tuning_version").and_then(|n| n.attr("value")).and_then(|v| v.parse().ok()),
    })
}

/// Where `neiro import` installs the tuning.
pub fn default_path() -> PathBuf {
    crate::params::state_path().with_file_name("tuning.xml")
}

/// The tuning to use: an explicit path, else the configured one, else the default location.
pub fn resolve(explicit: Option<&Path>, configured: Option<&str>) -> Result<PathBuf> {
    let p = explicit.map(Path::to_path_buf).or_else(|| configured.map(PathBuf::from)).unwrap_or_else(default_path);
    if !p.exists() {
        bail!(
            "no speaker tuning at {}.\n\
             neiro needs your laptop's Dolby (DAX3) tuning XML from its Windows audio driver.\n\
             Run `neiro import <path>` with a mounted Windows partition, an unpacked driver\n\
             package, or the XML itself; it picks the file that matches this machine.",
            p.display()
        );
    }
    Ok(p)
}

pub fn load(path: &Path, mode: Option<&str>) -> Result<Tuning> {
    let t = parse(&std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?, mode)
        .with_context(|| format!("parsing {}", path.display()))?;
    if t.channels > 2 {
        eprintln!(
            "neiro: this tuning describes {} speakers; neiro processes the first two (left/right) for now",
            t.channels
        );
    }
    Ok(t)
}

#[cfg(test)]
pub const TEST_XML: &str = include_str!("../tests/fixtures/synthetic_dax3.xml");

#[cfg(test)]
pub fn test_tuning() -> Tuning {
    parse(TEST_XML, None).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture() {
        let t = test_tuning();
        assert_eq!(t.band_freqs[0], 47.0);
        assert_eq!(t.band_freqs[19], 19688.0);
        assert_eq!(t.mode, "normal");
        assert_eq!(t.modes, ["normal", "tablet"]);
        assert_eq!(t.channels, 2);
        assert_eq!(t.tuning_version, Some(7));
        assert_eq!(t.profiles.len(), 6);
        let music = t.profile("music").unwrap();
        assert_eq!(music.volmax_boost_db, 6.0);
        assert!(music.mbc_enable && music.mbc_groups.len() == 4);
        assert!((music.threshold_high[4] - (-560.0 / 16.0)).abs() < 1e-6);
        assert!(music.isolated[0] && !music.isolated[10]);
        assert_eq!(music.optimizer[0][2], 1.5);
        assert!((t.ieq_curves["balanced"][0] - 9.0).abs() < 0.1);
    }

    #[test]
    fn filters_per_speaker_and_all_types() {
        let t = test_tuning();
        let music = t.profile("music").unwrap();
        let kinds: Vec<u32> = music.peq[0].iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [1, 2, 3, 4, 9, 8]);
        assert_eq!(music.peq[1].len(), music.peq[0].len() + 1, "right speaker has an extra filter");
        let notch = music.peq[0][1];
        assert_eq!((notch.f0, notch.q), (480.0, 6.0));
        let shelf = music.peq[0][2];
        assert!((shelf.q - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3, "s=1 shelf is Q=0.707");
        assert!(!music.peq[0][5].enabled && music.peq[0][5].order == 8);
        let voice = t.profile("voice").unwrap();
        assert!(voice.peq[0].iter().any(|f| f.kind == 7 && f.f0 == 110.0 && f.order == 4));
        assert!(voice.peq[0].iter().any(|f| f.kind == 6 && f.f0 == 9000.0));
    }

    #[test]
    fn threshold_forms_and_single_group_mbc() {
        let t = test_tuning();
        // dynamic uses presets, voice the per-channel ch_00 sub-schema.
        assert_eq!(t.profile("dynamic").unwrap().threshold_high, [0.0; NBANDS]);
        let voice = t.profile("voice").unwrap();
        assert!((voice.threshold_high[4] - (-520.0 / 16.0)).abs() < 1e-6);
        assert!((voice.threshold_high[4] - voice.threshold_low[4] - 12.0).abs() < 1e-6);
        assert_eq!(voice.mbc_groups.len(), 1);
        assert_eq!(voice.mbc_groups[0].end_band, 20);
    }

    #[test]
    fn modes_profiles_and_defaults() {
        let t = test_tuning();
        assert_eq!(t.profile_names(), ["dynamic", "movie", "music", "voice", "off", "personalize_user1"]);
        assert_eq!(t.initial_profile(), "music");
        let tablet = parse(TEST_XML, Some("tablet")).unwrap();
        assert_eq!(tablet.mode, "tablet");
        assert_eq!(tablet.profile("music").unwrap().volmax_boost_db, 3.0);
        assert_eq!(tablet.profile_names(), ["dynamic", "music", "off"], "off is always offered");
        assert!(parse(TEST_XML, Some("tent")).unwrap_err().to_string().contains("normal, tablet"));
    }

    #[test]
    fn minimal_tuning_uses_defaults() {
        let xml = r#"<device_data><endpoint type="internal_speaker"><profile type="dynamic">
            <tuning-vlldp><speaker-peq-filters><filter speaker="0" enabled="1" type="1" f0="1000" gain="3" q="1"/></speaker-peq-filters></tuning-vlldp>
            </profile></endpoint></device_data>"#;
        let t = parse(xml, None).unwrap();
        assert_eq!(t.band_freqs, DEFAULT_BANDS);
        let p = &t.profiles[0];
        assert_eq!(p.peq[1], p.peq[0], "speaker 0 filters apply to both sides");
        assert!(!p.mbc_enable && p.threshold_high == [0.0; NBANDS]);
        assert!(parse("<foo/>", None).is_err());
    }
}
