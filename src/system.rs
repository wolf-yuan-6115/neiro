//! Small wrappers around `pw-dump`, `wpctl` and `pw-metadata` for default-sink
//! and hardware-volume handling: writes, and reads before the main loop starts
//! or after it stops. While it runs, `graph` mirrors the graph instead.

use anyhow::{anyhow, Context, Result};
use std::process::{Command, Output};

/// Runs a PipeWire tool, logging the command at trace level and any failure with its stderr.
fn run(cmd: &mut Command) -> Result<Output> {
    let line = format!("{} {}", cmd.get_program().to_string_lossy(), cmd.get_args().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" "));
    log::trace!("running {line}");
    let out = cmd.output().with_context(|| format!("running {line}"))?;
    if !out.status.success() {
        log::debug!("{line} failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: u32,
    pub name: String,
    pub class: String,
    pub description: String,
}

pub fn nodes() -> Result<Vec<NodeInfo>> {
    let out = run(&mut Command::new("pw-dump"))?;
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
/// state.toml), else a built-in ALSA sink whose name or description says
/// "speaker", else the only built-in ALSA sink. USB, HDMI/DisplayPort and
/// headphone sinks never count as built-in, whatever the default output is.
pub fn find_speaker(preferred: Option<&str>) -> Result<String> {
    pick_speaker(&nodes()?, preferred)
}

fn pick_speaker(nodes: &[NodeInfo], preferred: Option<&str>) -> Result<String> {
    let sinks: Vec<&NodeInfo> =
        nodes.iter().filter(|n| n.class == "Audio/Sink" && n.name.starts_with("alsa_output")).collect();
    if let Some(p) = preferred {
        return sinks
            .iter()
            .find(|n| n.name == p)
            .map(|n| n.name.clone())
            .ok_or_else(|| anyhow!("configured speaker_sink {p} not found"));
    }
    let built_in = |n: &&&NodeInfo| {
        let s = format!("{} {}", n.name, n.description).to_lowercase();
        !["hdmi", "displayport", "headphone", "usb", "bluetooth", "bluez"].iter().any(|k| s.contains(k))
    };
    let candidates: Vec<&&NodeInfo> = sinks.iter().filter(built_in).collect();
    let is_speaker = |n: &&&NodeInfo| n.name.to_lowercase().contains("speaker") || n.description.to_lowercase().contains("speaker");
    if let Some(n) = candidates.iter().find(|n| is_speaker(n)) {
        return Ok(n.name.clone());
    }
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
    let out = run(Command::new("wpctl").args(["get-volume", &id.to_string()])).ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    Some((s.split_whitespace().nth(1)?.parse().ok()?, s.contains("[MUTED]")))
}

pub fn set_mute(id: u32, muted: bool) -> Result<()> {
    let out = run(Command::new("wpctl").args(["set-mute", &id.to_string(), if muted { "1" } else { "0" }]))?;
    if !out.status.success() {
        return Err(anyhow!("wpctl set-mute failed"));
    }
    Ok(())
}

pub fn set_volume(id: u32, v: f32) -> Result<()> {
    let out = run(Command::new("wpctl").args(["set-volume", &id.to_string(), &format!("{v:.4}")]))?;
    if !out.status.success() {
        return Err(anyhow!("wpctl set-volume failed"));
    }
    Ok(())
}

/// Raw JSON value of a key in the "default" metadata, e.g. `{"name":"alsa_output..."}`.
pub fn get_default(key: &str) -> Option<String> {
    let out = run(Command::new("pw-metadata").args(["-n", "default", "0", key])).ok()?;
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
    let st = run(&mut c)?;
    if !st.status.success() {
        return Err(anyhow!("pw-metadata failed"));
    }
    Ok(())
}

/// PipeWire's user-facing volume is cubic: dB = 60 * log10(v).
pub fn volume_db(v: f32) -> f32 {
    60.0 * v.max(1e-4).log10()
}

/// WirePlumber's stack of previously chosen default sinks, most recent first
/// (`node.restore-default-targets`). With the configured default absent,
/// WirePlumber picks the first available entry, so the order is the user's
/// device preference. Empty if the state file can't be read.
pub fn default_sink_history() -> Vec<String> {
    let Some(path) = dirs::state_dir().map(|d| d.join("wireplumber").join("default-nodes")) else { return vec![] };
    std::fs::read_to_string(path).map(|s| parse_sink_history(&s)).unwrap_or_default()
}

fn parse_sink_history(state: &str) -> Vec<String> {
    const KEY: &str = "default.configured.audio.sink";
    let mut entries: Vec<(i64, String)> = state
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let idx = match k.strip_prefix(KEY)? {
                "" => -1,
                rest => rest.strip_prefix('.')?.parse().ok()?,
            };
            Some((idx, v.trim().to_string()))
        })
        .collect();
    entries.sort();
    entries.into_iter().map(|(_, v)| v).collect()
}

/// The sinks to re-select, in order, after selecting `ours`, so that `ours`
/// lands just above `speaker` in the history and everything the user ranked
/// above the speaker stays above it. Each selection moves a name to the top.
pub fn history_pushes(history: &[String], speaker: &str, ours: &str) -> Vec<String> {
    const MAX: usize = 16;
    let above: Vec<&String> = history.iter().take_while(|n| n.as_str() != speaker).filter(|n| n.as_str() != ours).collect();
    if above.len() > MAX || !history.iter().any(|n| n == speaker) {
        // Speaker not ranked (or buried): keep just the user's own choice on top.
        return history.iter().filter(|n| n.as_str() != ours).take(1).cloned().collect();
    }
    above.into_iter().rev().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_is_never_an_external_device() {
        let sink = |id, name: &str, description: &str| NodeInfo {
            id,
            name: name.into(),
            class: "Audio/Sink".into(),
            description: description.into(),
        };
        // An external USB speaker listed before the built-in one, while it's the default.
        let nodes = [
            sink(30, "alsa_output.usb-Acer_ASK130_USB_Speaker_12345678custom-00.analog-stereo", "ASK130 USB Speaker Analog Stereo"),
            sink(40, "alsa_output.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Headphones__sink", "Headphones"),
            sink(47, "alsa_output.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Speaker__sink", "Speaker"),
            sink(60, "bluez_output.B8_7B_D4_12_0F_95.1", "Bluetooth Speaker"),
        ];
        assert_eq!(pick_speaker(&nodes, None).unwrap(), nodes[2].name);
        // HDA: one analog sink for speaker and jack, next to a USB DAC.
        let hda = [
            sink(30, "alsa_output.usb-LightWolf_Pumper_USB_DAC-00.analog-stereo", "Pumper USB DAC Analog Stereo"),
            sink(50, "alsa_output.pci-0000_00_1f.3.analog-stereo", "Built-in Audio Analog Stereo"),
            sink(51, "alsa_output.pci-0000_00_1f.3.hdmi-stereo", "Built-in Audio Digital Stereo (HDMI)"),
        ];
        assert_eq!(pick_speaker(&hda, None).unwrap(), hda[1].name);
        // Nothing built-in: ask instead of guessing an external device.
        assert!(pick_speaker(&nodes[..1], None).is_err());
    }

    #[test]
    fn sink_history_order() {
        let state = "[default-nodes]\n\
            default.configured.audio.sink.1=bt\n\
            default.configured.audio.sink=dac\n\
            default.configured.audio.source=mic\n\
            default.configured.audio.sink.0=neiro_sink\n\
            default.configured.audio.sink.2=speaker\n\
            default.configured.audio.sink.3=headphones\n";
        let h = parse_sink_history(state);
        assert_eq!(h, ["dac", "neiro_sink", "bt", "speaker", "headphones"]);
        // Select neiro, then bt, then dac: history becomes dac, bt, neiro, speaker.
        assert_eq!(history_pushes(&h, "speaker", "neiro_sink"), ["bt", "dac"]);
        let none: Vec<String> = vec![];
        assert_eq!(history_pushes(&["speaker".into()], "speaker", "neiro_sink"), none);
        assert_eq!(history_pushes(&["dac".into(), "x".into()], "speaker", "neiro_sink"), ["dac"]);
    }
}
