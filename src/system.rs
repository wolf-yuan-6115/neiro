//! Small wrappers around `pw-dump`, `wpctl` and `pw-metadata` for default-sink
//! and hardware-volume handling. These run outside the audio path only.

use anyhow::{anyhow, Context, Result};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: u32,
    pub name: String,
    pub class: String,
    pub description: String,
}

pub fn nodes() -> Result<Vec<NodeInfo>> {
    let out = Command::new("pw-dump").output().context("running pw-dump")?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|o| {
                    let props = o.pointer("/info/props")?;
                    Some(NodeInfo {
                        id: o.get("id")?.as_u64()? as u32,
                        name: props.get("node.name")?.as_str()?.to_string(),
                        class: props.get("media.class").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                        description: props.get("node.description").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn node_id(name: &str) -> Option<u32> {
    nodes().ok()?.into_iter().find(|n| n.name == name).map(|n| n.id)
}

/// The built-in speaker sink: `preferred` if given (`speaker_sink` in
/// state.toml), else an ALSA sink whose name or description says "speaker",
/// else the only ALSA sink that isn't HDMI/DisplayPort, headphones or USB.
pub fn find_speaker(preferred: Option<&str>) -> Result<String> {
    let sinks: Vec<NodeInfo> =
        nodes()?.into_iter().filter(|n| n.class == "Audio/Sink" && n.name.starts_with("alsa_output")).collect();
    if let Some(p) = preferred {
        return sinks
            .iter()
            .find(|n| n.name == p)
            .map(|n| n.name.clone())
            .ok_or_else(|| anyhow!("configured speaker_sink {p} not found"));
    }
    let is_speaker = |n: &NodeInfo| n.name.to_lowercase().contains("speaker") || n.description.to_lowercase().contains("speaker");
    if let Some(n) = sinks.iter().find(|n| is_speaker(n)) {
        return Ok(n.name.clone());
    }
    let other = |n: &&NodeInfo| {
        let s = format!("{} {}", n.name, n.description).to_lowercase();
        !["hdmi", "displayport", "headphone", "usb", "bluetooth", "bluez"].iter().any(|k| s.contains(k))
    };
    let candidates: Vec<&NodeInfo> = sinks.iter().filter(other).collect();
    match candidates.as_slice() {
        [one] => Ok(one.name.clone()),
        _ => {
            let list: Vec<String> = sinks.iter().map(|n| format!("  {}  ({})", n.name, n.description)).collect();
            Err(anyhow!(
                "couldn't tell which sink is the built-in speaker. Set speaker_sink in {} to one of:\n{}",
                crate::params::state_path().display(),
                list.join("\n")
            ))
        }
    }
}

/// (volume, muted)
pub fn get_volume(id: u32) -> Option<(f32, bool)> {
    let out = Command::new("wpctl").args(["get-volume", &id.to_string()]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    Some((s.split_whitespace().nth(1)?.parse().ok()?, s.contains("[MUTED]")))
}

pub fn set_mute(id: u32, muted: bool) -> Result<()> {
    let st = Command::new("wpctl").args(["set-mute", &id.to_string(), if muted { "1" } else { "0" }]).status()?;
    if !st.success() {
        return Err(anyhow!("wpctl set-mute failed"));
    }
    Ok(())
}

pub fn set_volume(id: u32, v: f32) -> Result<()> {
    let st = Command::new("wpctl").args(["set-volume", &id.to_string(), &format!("{v:.4}")]).status()?;
    if !st.success() {
        return Err(anyhow!("wpctl set-volume failed"));
    }
    Ok(())
}

/// Raw JSON value of a key in the "default" metadata, e.g. `{"name":"alsa_output..."}`.
pub fn get_default(key: &str) -> Option<String> {
    let out = Command::new("pw-metadata").args(["-n", "default", "0", key]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().find(|l| l.contains(&format!("key:'{key}'")))?;
    let start = line.find("value:'")? + 7;
    let end = line[start..].find("' type:")? + start;
    Some(line[start..end].to_string())
}

pub fn default_name(key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&get_default(key)?).ok()?;
    v.get("name")?.as_str().map(String::from)
}

pub fn set_default(key: &str, json: Option<&str>) -> Result<()> {
    let mut c = Command::new("pw-metadata");
    c.args(["-n", "default"]);
    match json {
        Some(v) => c.args(["0", key, v, "Spa:String:JSON"]),
        None => c.args(["-d", "0", key]),
    };
    let st = c.output()?;
    if !st.status.success() {
        return Err(anyhow!("pw-metadata failed"));
    }
    Ok(())
}

/// All links as (output node id, input node id).
pub fn links() -> Result<Vec<(u32, u32)>> {
    let out = Command::new("pw-dump").output().context("running pw-dump")?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Link"))
                .filter_map(|o| {
                    let i = o.get("info")?;
                    Some((i.get("output-node-id")?.as_u64()? as u32, i.get("input-node-id")?.as_u64()? as u32))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// PipeWire's user-facing volume is cubic: dB = 60 * log10(v).
pub fn volume_db(v: f32) -> f32 {
    60.0 * v.max(1e-4).log10()
}
