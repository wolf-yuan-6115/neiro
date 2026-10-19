//! `neiro import`: find this machine's DAX3 speaker tuning in a Windows
//! partition, an unpacked driver package or a single file, and install it.
//!
//! Tuning files are named after the hardware they bind to:
//! - HD Audio: `[HDAUDIO_|INTELAUDIO_|PCI_|AUCD_]DEV_<codec>_SUBSYS_<subsystem>[_PCI_SUBSYS_…].xml`
//! - SoundWire: `SOUNDWIRE_[SDCAFUNCTION_NN_]MAN_<man>_FUNC_<func>_SUBSYS_<device><vendor>.xml`,
//!   where SUBSYS is the audio controller's PCI subsystem.
//! - Controller-bound: `SDW_<controller>_SUBSYS_<subsystem>…xml`, bound (per its security
//!   key, `INTELAUDIO\CTLR_DEV_<controller>`) to the audio controller's PCI device id.
//!
//! SUBSYS is usually hex, but some vendors use a text tag (Lenovo's `IDEA4002`).
//!
//! Matching follows the tiers worked out by the speaker-tuning-to-easyeffects
//! project: the exact device first, a subsystem-only match as a warned
//! fallback, and `tuning_version` as the tiebreak.

use crate::tuning;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Audio hardware identifiers of this machine.
#[derive(Debug, Default, Clone)]
pub struct MachineIds {
    /// SoundWire (manufacturer, part) pairs, lowercase hex.
    pub sdw: Vec<(String, String)>,
    /// PCI subsystems of audio controllers as `<device><vendor>`, uppercase hex.
    pub pci_subsys: Vec<String>,
    /// Audio controllers as (PCI device id, subsystem), uppercase hex.
    pub controllers: Vec<(String, String)>,
    /// HD-Audio codecs as (device, subsystem), uppercase hex.
    pub hda: Vec<(String, String)>,
}

impl MachineIds {
    pub fn detect() -> MachineIds {
        let mut ids = MachineIds::default();
        // e.g. sdw:0:2:01fa:3556:01:0 -> link, unique id, manufacturer, part, class[, function]
        if let Ok(rd) = std::fs::read_dir("/sys/bus/soundwire/devices") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let parts: Vec<&str> = name.split(':').collect();
                if parts.len() >= 5 && parts[0] == "sdw" {
                    let id = (parts[3].to_lowercase(), parts[4].to_lowercase());
                    if !ids.sdw.contains(&id) {
                        ids.sdw.push(id);
                    }
                }
            }
        }
        if let Ok(rd) = std::fs::read_dir("/sys/bus/pci/devices") {
            for e in rd.flatten() {
                let read = |f: &str| std::fs::read_to_string(e.path().join(f)).unwrap_or_default().trim().to_lowercase();
                // 0x0403xx: HD-Audio controller; 0x0401xx: audio DSP (SOF on newer Intel/AMD).
                let class = read("class");
                if !class.starts_with("0x0403") && !class.starts_with("0x0401") {
                    continue;
                }
                let (v, d) = (read("subsystem_vendor"), read("subsystem_device"));
                let key = format!("{}{}", d.trim_start_matches("0x"), v.trim_start_matches("0x")).to_uppercase();
                if key.len() == 8 && !ids.pci_subsys.contains(&key) {
                    ids.pci_subsys.push(key.clone());
                }
                let dev = read("device").trim_start_matches("0x").to_uppercase();
                if dev.len() == 4 && key.len() == 8 && !ids.controllers.contains(&(dev.clone(), key.clone())) {
                    ids.controllers.push((dev, key));
                }
            }
        }
        if let Ok(cards) = std::fs::read_dir("/proc/asound") {
            for card in cards.flatten().filter(|c| c.file_name().to_string_lossy().starts_with("card")) {
                for codec in std::fs::read_dir(card.path()).into_iter().flatten().flatten() {
                    if !codec.file_name().to_string_lossy().starts_with("codec#") {
                        continue;
                    }
                    let text = std::fs::read_to_string(codec.path()).unwrap_or_default();
                    let field = |k: &str| {
                        text.lines().find_map(|l| l.strip_prefix(k)).map(|v| v.trim().trim_start_matches("0x").to_uppercase())
                    };
                    if let (Some(vendor), Some(sub)) = (field("Vendor Id:"), field("Subsystem Id:")) {
                        if vendor.len() == 8 && sub.len() == 8 {
                            let id = (vendor[4..].to_string(), sub);
                            if !ids.hda.contains(&id) {
                                ids.hda.push(id);
                            }
                        }
                    }
                }
            }
        }
        ids
    }

    fn describe(&self) -> String {
        let mut parts = vec![];
        for (m, p) in &self.sdw {
            parts.push(format!("SoundWire {m}:{p}"));
        }
        for (d, s) in &self.hda {
            parts.push(format!("HDA codec {d} subsystem {s}"));
        }
        for (d, s) in &self.controllers {
            parts.push(format!("audio controller {d} subsystem {s}"));
        }
        if parts.is_empty() { "no audio hardware IDs found".into() } else { parts.join(", ") }
    }
}

/// Hardware key parsed from a tuning's file name.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    SoundWire { man: String, func: String, subsys: String },
    Hda { dev: String, subsys: String },
    Controller { dev: String, subsys: String },
}

/// `len` characters right after `tag` in `s`, all hex digits (or, with
/// `alnum`, any ASCII letters and digits).
fn field_after(s: &str, tag: &str, len: usize, alnum: bool) -> Option<String> {
    let i = s.find(tag)? + tag.len();
    let h: String = s[i..].chars().take(len).collect();
    let ok = |c: char| if alnum { c.is_ascii_alphanumeric() } else { c.is_ascii_hexdigit() };
    (h.len() == len && h.chars().all(ok)).then_some(h)
}

fn hex_after(s: &str, tag: &str, len: usize) -> Option<String> {
    field_after(s, tag, len, false)
}

/// SUBSYS is usually hex, but some vendors use a text vendor tag (`IDEA4002`).
fn subsys_after(s: &str) -> Option<String> {
    field_after(s, "SUBSYS_", 8, true)
}

/// Parse a speaker-tuning file name; `None` for anything else, including the
/// `_settings`, `_dmic` and `_amic` companions that share the naming.
pub fn parse_name(file: &str) -> Option<Key> {
    let u = file.to_uppercase();
    let stem = u.strip_suffix(".XML")?;
    if ["_SETTINGS", "_DMIC", "_AMIC"].iter().any(|c| stem.ends_with(c)) {
        return None;
    }
    if stem.contains("SOUNDWIRE") || (stem.starts_with("SDW_") && stem.contains("MAN_")) {
        return Some(Key::SoundWire {
            man: hex_after(stem, "MAN_", 4)?.to_lowercase(),
            func: hex_after(stem, "FUNC_", 4)?.to_lowercase(),
            subsys: subsys_after(stem)?,
        });
    }
    if stem.starts_with("SDW_") {
        return Some(Key::Controller { dev: hex_after(stem, "SDW_", 4)?, subsys: subsys_after(stem)? });
    }
    let dev = hex_after(stem, "DEV_", 4)?;
    let subsys = subsys_after(stem)?;
    Some(Key::Hda { dev, subsys })
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub enum Fit {
    /// Hardware this machine doesn't have.
    None,
    /// Same subsystem but a different codec / SoundWire function: plausible, unverified.
    Partial,
    Exact,
}

pub fn score(key: &Key, ids: &MachineIds) -> Fit {
    match key {
        Key::SoundWire { man, func, subsys } => {
            if !ids.pci_subsys.contains(subsys) || !ids.sdw.iter().any(|(m, _)| m == man) {
                Fit::None
            } else if ids.sdw.iter().any(|(m, p)| m == man && p == func) {
                Fit::Exact
            } else {
                Fit::Partial
            }
        }
        Key::Controller { dev, subsys } => {
            if ids.controllers.iter().any(|(d, s)| d == dev && s == subsys) {
                Fit::Exact
            } else if ids.pci_subsys.contains(subsys) {
                Fit::Partial
            } else {
                Fit::None
            }
        }
        Key::Hda { dev, subsys } => {
            if ids.hda.iter().any(|(d, s)| d == dev && s == subsys) {
                Fit::Exact
            } else if ids.hda.iter().any(|(_, s)| s == subsys) {
                Fit::Partial
            } else {
                Fit::None
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub fit: Fit,
    pub version: i64,
}

fn tuning_version(path: &Path) -> i64 {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.split("<tuning_version value=\"").nth(1).and_then(|r| r.split('"').next()).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// Every speaker-tuning file under `root`, without following symlinks.
pub fn find_tunings(root: &Path) -> Vec<(PathBuf, Key)> {
    // On a Windows partition only the driver store matters; skip the rest of the disk.
    let store = ["Windows/System32/DriverStore/FileRepository", "System32/DriverStore/FileRepository"]
        .iter()
        .map(|p| root.join(p))
        .find(|p| p.is_dir())
        .unwrap_or_else(|| root.to_path_buf());
    let mut out = vec![];
    let mut stack = vec![(store, 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && depth < 16 {
                stack.push((e.path(), depth + 1));
            } else if ft.is_file() {
                if let Some(key) = parse_name(&e.file_name().to_string_lossy()) {
                    out.push((e.path(), key));
                }
            }
        }
    }
    out
}

/// Candidates that could belong to this machine, best first.
pub fn rank(found: Vec<(PathBuf, Key)>, ids: &MachineIds) -> Vec<Candidate> {
    let mut c: Vec<Candidate> = found
        .into_iter()
        .map(|(path, key)| Candidate { fit: score(&key, ids), version: tuning_version(&path), path })
        .filter(|c| c.fit != Fit::None)
        .collect();
    c.sort_by(|a, b| b.fit.partial_cmp(&a.fit).unwrap().then(b.version.cmp(&a.version)));
    c
}

fn name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn import(source: &Path, list_only: bool) -> Result<()> {
    if !source.exists() {
        bail!("{} doesn't exist", source.display());
    }
    let ids = MachineIds::detect();
    println!("This machine: {}", ids.describe());

    let chosen: PathBuf = if source.is_file() {
        // A single file is the user's explicit choice; just sanity-check the name.
        match parse_name(&name(source)) {
            Some(key) if score(&key, &ids) == Fit::None => {
                println!("warning: {} names hardware this machine doesn't report; importing it anyway", name(source))
            }
            None => println!("note: {} doesn't follow the DAX3 naming; importing it anyway", name(source)),
            _ => {}
        }
        source.to_path_buf()
    } else {
        let found = find_tunings(source);
        let total = found.len();
        let ranked = rank(found, &ids);
        println!("Found {total} speaker tunings under {}; {} could belong to this machine.", source.display(), ranked.len());
        for c in ranked.iter().take(8) {
            let fit = if c.fit == Fit::Exact { "exact" } else { "subsystem only" };
            println!("  {:<15} v{:<4} {}", fit, c.version, c.path.display());
        }
        let Some(best) = ranked.first() else {
            bail!(
                "none of the {total} tunings matches this machine.\n\
                 If you know which file is right, pass it directly: neiro import <file.xml>"
            );
        };
        if best.fit == Fit::Partial {
            println!("warning: no exact match; the best one only shares the subsystem id, so it may be for a sibling model");
        }
        best.path.clone()
    };
    if list_only {
        return Ok(());
    }

    let text = std::fs::read_to_string(&chosen).with_context(|| format!("reading {}", chosen.display()))?;
    let t = tuning::parse(&text, None).with_context(|| format!("{} isn't a usable DAX3 tuning", chosen.display()))?;
    let dest = tuning::default_path();
    std::fs::create_dir_all(dest.parent().unwrap())?;
    if dest.exists() && std::fs::read_to_string(&dest).ok().as_deref() != Some(text.as_str()) {
        let backup = dest.with_extension("xml.bak");
        std::fs::rename(&dest, &backup)?;
        println!("Previous tuning kept as {}", backup.display());
    }
    std::fs::write(&dest, &text)?;
    println!("Installed {} → {}", name(&chosen), dest.display());
    println!(
        "  modes: {} · profiles: {} · speakers: {}",
        t.modes.join(", "),
        t.profile_names().join(", "),
        t.channels
    );
    if let Some(p) = crate::params::Saved::load().tuning_path {
        println!("note: state.toml sets tuning_path = {p}, which takes precedence over the imported file");
    }
    println!("Restart the neiro daemon to use it.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soundwire_machine() -> MachineIds {
        MachineIds {
            sdw: vec![("01fa".into(), "3556".into()), ("01fa".into(), "4243".into())],
            pci_subsys: vec!["1E131043".into()],
            controllers: vec![("A828".into(), "1E131043".into())],
            hda: vec![],
        }
    }

    #[test]
    fn parses_tuning_names() {
        assert_eq!(
            parse_name("SOUNDWIRE_MAN_01FA_FUNC_3556_SUBSYS_1E131043.xml"),
            Some(Key::SoundWire { man: "01fa".into(), func: "3556".into(), subsys: "1E131043".into() })
        );
        assert_eq!(
            parse_name("SOUNDWIRE_SDCAFUNCTION_10_MAN_01FA_FUNC_3556_SUBSYS_CA0A144D.xml"),
            Some(Key::SoundWire { man: "01fa".into(), func: "3556".into(), subsys: "CA0A144D".into() })
        );
        assert_eq!(
            parse_name("DEV_0287_SUBSYS_17AA38C5_PCI_SUBSYS_38C517AA.xml"),
            Some(Key::Hda { dev: "0287".into(), subsys: "17AA38C5".into() })
        );
        assert_eq!(
            parse_name("HDAUDIO_DEV_0257_SUBSYS_17AA3852.xml"),
            Some(Key::Hda { dev: "0257".into(), subsys: "17AA3852".into() })
        );
        // Lenovo IdeaPad installers use a text vendor tag in SUBSYS.
        assert_eq!(
            parse_name("AUCD_DEV_0C29_SUBSYS_IDEA4002.xml"),
            Some(Key::Hda { dev: "0C29".into(), subsys: "IDEA4002".into() })
        );
        // Short SDW_ names bind to the audio controller's PCI device id.
        assert_eq!(
            parse_name("SDW_51CC_SUBSYS_230117AA_PCI_SUBSYS_230117AA.xml"),
            Some(Key::Controller { dev: "51CC".into(), subsys: "230117AA".into() })
        );
        for skip in ["DEV_0287_SUBSYS_17AA38C5_settings.xml", "DEV_0287_SUBSYS_17AA38C5_dmic.xml", "readme.xml", "DEV_0287.inf"] {
            assert_eq!(parse_name(skip), None, "{skip}");
        }
    }

    #[test]
    fn scoring_tiers() {
        let ids = soundwire_machine();
        let sdw = |func: &str, sub: &str| Key::SoundWire { man: "01fa".into(), func: func.into(), subsys: sub.into() };
        assert_eq!(score(&sdw("3556", "1E131043"), &ids), Fit::Exact);
        assert_eq!(score(&sdw("9999", "1E131043"), &ids), Fit::Partial);
        assert_eq!(score(&sdw("3556", "CA0A144D"), &ids), Fit::None, "same amp on another laptop");
        // The HDA tuning keyed to the same PCI subsystem needs that codec present.
        assert_eq!(score(&Key::Hda { dev: "0294".into(), subsys: "10431E13".into() }, &ids), Fit::None);
        let ctl = MachineIds {
            pci_subsys: vec!["230117AA".into()],
            controllers: vec![("51CC".into(), "230117AA".into())],
            ..Default::default()
        };
        assert_eq!(score(&Key::Controller { dev: "51CC".into(), subsys: "230117AA".into() }, &ctl), Fit::Exact);
        assert_eq!(score(&Key::Controller { dev: "A828".into(), subsys: "230117AA".into() }, &ctl), Fit::Partial);
        assert_eq!(score(&Key::Controller { dev: "51CC".into(), subsys: "1E131043".into() }, &ctl), Fit::None);
        let hda = MachineIds { hda: vec![("0287".into(), "17AA38C5".into())], ..Default::default() };
        assert_eq!(score(&Key::Hda { dev: "0287".into(), subsys: "17AA38C5".into() }, &hda), Fit::Exact);
        // Lenovo reuses subsystems across codecs: that's only a partial match.
        assert_eq!(score(&Key::Hda { dev: "0257".into(), subsys: "17AA38C5".into() }, &hda), Fit::Partial);
    }

    #[test]
    fn finds_and_ranks_in_a_tree() {
        let dir = std::env::temp_dir().join(format!("neiro-discover-{}", std::process::id()));
        let a = dir.join("Windows/System32/DriverStore/FileRepository/pkg_a");
        let b = dir.join("Windows/System32/DriverStore/FileRepository/pkg_b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let body = |v: u32| format!("<device_data><tuning_version value=\"{v}\"/></device_data>");
        std::fs::write(a.join("SOUNDWIRE_MAN_01FA_FUNC_3556_SUBSYS_1E131043.xml"), body(30)).unwrap();
        std::fs::write(b.join("SOUNDWIRE_MAN_01FA_FUNC_3556_SUBSYS_1E131043.xml"), body(32)).unwrap();
        std::fs::write(b.join("SOUNDWIRE_MAN_01FA_FUNC_3556_SUBSYS_1E131043_settings.xml"), body(99)).unwrap();
        std::fs::write(b.join("SOUNDWIRE_MAN_01FA_FUNC_3556_SUBSYS_CA0A144D.xml"), body(40)).unwrap();
        let found = find_tunings(&dir);
        assert_eq!(found.len(), 3, "companion skipped");
        let ranked = rank(found, &soundwire_machine());
        assert_eq!(ranked.len(), 2, "other laptop's tuning dropped");
        assert_eq!(ranked[0].version, 32, "newest tuning wins the tie");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Runs every tuning in a local corpus through discovery, the parser and the
    /// DSP. Not run by default; the corpus is third-party data and never committed.
    /// To build one: download a vendor's audio driver package (e.g. an ASUS
    /// "Dolby Atmos driver"), unpack it (`7z x -tPE pkg.exe .rsrc/ZIP/103`, then
    /// `7z x` the result) and point NEIRO_CORPUS at the folder:
    ///   NEIRO_CORPUS=/path/to/xmls cargo test --release corpus -- --ignored --nocapture
    #[test]
    #[ignore]
    fn corpus() {
        use crate::dsp::chain::{Config, Engine};
        let Ok(root) = std::env::var("NEIRO_CORPUS") else { panic!("set NEIRO_CORPUS to a folder of tuning XMLs") };
        let found = find_tunings(Path::new(&root));
        assert!(!found.is_empty(), "no tuning files under {root}");
        let (mut ok, mut rows) = (0, 0);
        let mut failures = vec![];
        let noise: Vec<f32> = (0..24000u32).map(|i| ((i.wrapping_mul(2654435761) >> 8) as f32 / 8388608.0 - 1.0) * 0.5).collect();
        for (path, _) in &found {
            let text = std::fs::read_to_string(path).unwrap();
            let base = match tuning::parse(&text, None) {
                Ok(t) => t,
                Err(e) => {
                    failures.push(format!("{}: {e:#}", path.display()));
                    continue;
                }
            };
            for mode in &base.modes {
                let t = tuning::parse(&text, Some(mode)).unwrap();
                for prof in t.profile_names() {
                    let mut p = crate::params::factory(&t, &prof).unwrap();
                    p.vbass.enable = true;
                    let mut e = Engine::new(48000.0, &t, Config::build(&p, &t, 48000.0, false, 0.0));
                    let (mut l, mut r) = (vec![0.0; noise.len()], vec![0.0; noise.len()]);
                    e.process(&noise, &noise, &mut l, &mut r);
                    let peak = l.iter().chain(&r).fold(0f32, |m, v| m.max(v.abs()));
                    if !l.iter().chain(&r).all(|v| v.is_finite()) || peak > 1.0 {
                        failures.push(format!("{} [{mode}/{prof}]: peak {peak}", path.display()));
                    }
                    rows += 1;
                }
            }
            ok += 1;
        }
        println!("corpus: {ok}/{} files parsed, {rows} mode/profile combinations rendered", found.len());
        assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    }
}
