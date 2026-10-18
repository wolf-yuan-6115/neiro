//! Parser for the Dolby DAX3 `tuning.xml` exported from the Windows driver package.
//!
//! Most numeric values in the file are fixed point in 1/16 dB steps; they are
//! converted to plain dB here so the rest of the code never sees raw units.

use anyhow::{anyhow, bail, Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;

pub const NBANDS: usize = 20;
pub const EMBEDDED_XML: &str = include_str!("../tuning.xml");

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeqFilter {
    pub enabled: bool,
    pub kind: u32,
    pub f0: f32,
    pub gain_db: f32,
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
    pub peq: Vec<PeqFilter>,
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
}

impl Tuning {
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }
}

fn ints(s: &str) -> Result<Vec<i64>> {
    s.split(',').map(|v| v.trim().parse::<i64>().with_context(|| format!("bad int {v:?}"))).collect()
}

fn arr20(v: &[f32]) -> Result<[f32; NBANDS]> {
    v.try_into().map_err(|_| anyhow!("expected {NBANDS} values, got {}", v.len()))
}

pub fn parse(xml: &str) -> Result<Tuning> {
    let root = parse_tree(xml)?;
    let dd = root.child("device_data").ok_or_else(|| anyhow!("no device_data"))?;
    let constant = dd.child("constant").ok_or_else(|| anyhow!("no constant"))?;

    let mut presets: HashMap<String, Vec<i64>> = HashMap::new();
    for c in &constant.children {
        if let Some(t) = c.attr("target") {
            presets.insert(c.name.clone(), ints(t)?);
        }
    }
    let band_freqs = constant
        .child("band_20_freq")
        .and_then(|n| n.attr("fs_48000"))
        .ok_or_else(|| anyhow!("no band_20_freq"))?;
    let band_freqs = arr20(&ints(band_freqs)?.iter().map(|&v| v as f32).collect::<Vec<_>>())?;

    let mut ieq_curves = HashMap::new();
    for (k, v) in &presets {
        if let Some(name) = k.strip_prefix("ieq_") {
            ieq_curves.insert(name.to_string(), arr20(&v.iter().map(|&x| x as f32 / 16.0).collect::<Vec<_>>())?);
        }
    }

    // Value lookup: either `value="..."` or `preset="..."` referencing a constant.
    let raw = |n: &Node| -> Result<Vec<i64>> {
        if let Some(v) = n.attr("value") {
            ints(v)
        } else if let Some(p) = n.attr("preset") {
            presets.get(p).cloned().ok_or_else(|| anyhow!("unknown preset {p}"))
        } else {
            bail!("node {} has no value", n.name)
        }
    };
    let endpoint = dd
        .children
        .iter()
        .find(|n| n.name == "endpoint" && n.attr("type") == Some("internal_speaker"))
        .ok_or_else(|| anyhow!("no internal_speaker endpoint"))?;

    let mut profiles = vec![];
    for p in endpoint.children.iter().filter(|n| n.name == "profile") {
        let name = p.attr("type").unwrap_or("?").to_string();
        let cp = p.child("tuning-cp").ok_or_else(|| anyhow!("{name}: no tuning-cp"))?;
        let dp = p.child("tuning-vlldp").ok_or_else(|| anyhow!("{name}: no tuning-vlldp"))?;
        let get = |n: &Node, path: &str| -> Result<Vec<i64>> {
            raw(n.path(path).ok_or_else(|| anyhow!("{name}: missing {path}"))?)
        };
        let one = |n: &Node, path: &str| -> Result<i64> { Ok(get(n, path)?[0]) };
        let db = |n: &Node, path: &str| -> Result<f32> { Ok(one(n, path)? as f32 / 16.0) };
        let db20 = |n: &Node, path: &str| -> Result<[f32; NBANDS]> {
            arr20(&get(n, path)?.iter().map(|&v| v as f32 / 16.0).collect::<Vec<_>>())
        };
        let pair = |n: &Node, path: &str| -> Result<(f32, f32)> {
            let v = get(n, path)?;
            Ok((v[0] as f32, v[1] as f32))
        };

        let peq = dp
            .child("speaker-peq-filters")
            .map(|f| {
                f.children
                    .iter()
                    .filter(|c| c.attr("speaker") == Some("0"))
                    .map(|c| -> Result<PeqFilter> {
                        let g = |a: &str| -> Result<f32> {
                            c.attr(a).ok_or_else(|| anyhow!("filter missing {a}"))?.parse::<f32>().map_err(Into::into)
                        };
                        // type 1 = peaking; type 7 = Butterworth high-pass with `order` and no gain/q.
                        let opt = |a: &str, d: f32| c.attr(a).and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
                        Ok(PeqFilter {
                            enabled: c.attr("enabled") == Some("1"),
                            kind: g("type")? as u32,
                            f0: g("f0")?,
                            gain_db: opt("gain", 0.0),
                            q: opt("q", 0.707),
                            order: opt("order", 2.0) as u32,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();

        let mbc_t = dp.child("mb-compressor-tuning").ok_or_else(|| anyhow!("{name}: no mbc"))?;
        let group_count = one(mbc_t, "group_count")? as usize;
        let mut mbc_groups = vec![];
        for g in 0..group_count.min(4) {
            let v = get(mbc_t, &format!("band_group_{g}"))?;
            mbc_groups.push(MbcGroup {
                end_band: v[0] as usize,
                threshold_db: v[1] as f32 / 16.0,
                slope: v[2] as f32 / 32768.0,
                attack_ms: v[3] as f32,
                release_ms: v[4] as f32,
                makeup_db: v[5] as f32 / 16.0,
            });
        }

        let isolated = get(dp, "regulator-tuning/isolated_band")?;
        let mut iso = [false; NBANDS];
        for (i, v) in isolated.iter().take(NBANDS).enumerate() {
            iso[i] = *v != 0;
        }
        let opt = dp.child("audio-optimizer-bands").ok_or_else(|| anyhow!("{name}: no optimizer"))?;
        let ch = |c: &str| -> Result<[f32; NBANDS]> {
            match opt.child(c) {
                Some(n) => arr20(&raw(n)?.iter().map(|&v| v as f32 / 16.0).collect::<Vec<_>>()),
                None => Ok([0.0; NBANDS]),
            }
        };

        profiles.push(Profile {
            ieq_enable: one(cp, "ieq-enable")? != 0,
            ieq_amount: one(cp, "ieq-amount")? as f32,
            ieq_preset: cp
                .child("ieq-bands-set")
                .and_then(|n| n.attr("preset"))
                .and_then(|s| s.strip_prefix("ieq_"))
                .unwrap_or("balanced")
                .to_string(),
            surround_boost_db: db(cp, "surround-boost")?,
            virtualizer: one(cp, "output-mode/processing_mode")? == 11,
            volmax_boost_db: db(cp, "volmax-boost")?,
            pregain_db: db(cp, "pregain")?,
            postgain_db: db(dp, "postgain")?,
            regulator_enable: one(cp, "regulator-enable")? != 0,
            vb_src_freqs: pair(cp, "virtual-bass-src-freqs")?,
            vb_mix_freqs: pair(cp, "virtual-bass-mix-freqs")?,
            optimizer_enable: one(dp, "audio-optimizer-enable")? != 0,
            optimizer: [ch("ch_00")?, ch("ch_01")?],
            peq_enable: one(dp, "speaker-peq-enable")? != 0,
            peq,
            mbc_enable: one(dp, "mb-compressor-enable")? != 0,
            mbc_groups,
            peak_level_db: db(dp, "peak-level")?,
            regulator_overdrive_db: db(dp, "regulator-overdrive")?,
            regulator_timbre: one(dp, "regulator-timbre-preservation")? as f32 / 16.0,
            regulator_dist_enable: one(dp, "regulator-speaker-dist-enable")? != 0,
            threshold_high: db20(dp, "regulator-tuning/threshold_high")?,
            threshold_low: db20(dp, "regulator-tuning/threshold_low")?,
            isolated: iso,
            name,
        });
    }
    Ok(Tuning { band_freqs, ieq_curves, profiles })
}

pub fn load(path: Option<&std::path::Path>) -> Result<Tuning> {
    match path {
        Some(p) => parse(&std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?),
        None => parse(EMBEDDED_XML),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_embedded() {
        let t = parse(EMBEDDED_XML).unwrap();
        assert_eq!(t.band_freqs[0], 47.0);
        assert_eq!(t.band_freqs[19], 19688.0);
        assert_eq!(t.profiles.len(), 10);
        let music = t.profile("music").unwrap();
        assert!(music.peq.iter().any(|f| f.enabled && f.f0 == 515.0 && f.gain_db == -9.0 && f.q == 9.0));
        assert_eq!(music.volmax_boost_db, 6.0);
        assert!(music.mbc_enable);
        assert_eq!(music.mbc_groups.len(), 4);
        assert!((music.threshold_high[4] - (-584.0 / 16.0)).abs() < 1e-6);
        assert!(music.isolated[0] && !music.isolated[10]);
        assert_eq!(music.optimizer[0][2], 2.0);
        let voice = t.profile("voice").unwrap();
        assert!(voice.peq.iter().any(|f| f.kind == 7 && f.f0 == 100.0 && f.order == 4));
        let off = t.profile("off").unwrap();
        assert!(off.peq.is_empty() && !off.mbc_enable);
        assert!((t.ieq_curves["balanced"][0] - 157.0 / 16.0).abs() < 1e-6);
    }
}
