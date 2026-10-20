//! A live view of the PipeWire graph for the daemon's monitor: nodes, links, the
//! "default" metadata and the volume of the speaker and the Neiro sink. Registry
//! events on the main loop keep it current, so nothing polls PipeWire's tools.

use crate::pw::SINK_NAME;
use crate::system::NodeInfo;
use anyhow::Result;
use pipewire as pw;
use pw::metadata::{Metadata, MetadataListener};
use pw::node::{Node, NodeListener};
use pw::registry::RegistryRc;
use pw::spa::param::ParamType;
use pw::spa::pod::deserialize::PodDeserializer;
use pw::spa::pod::{Pod, Value, ValueArray};
use pw::spa::utils::Id;
use pw::types::ObjectType;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Events often come in bursts (a stream appearing links several ports); gather
/// them. Short, because a stream reaching the pinned speaker plays at 100% until
/// the monitor unpins it.
const DEBOUNCE: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Default)]
pub struct GraphState {
    /// Bumped on every change.
    pub generation: u64,
    pub nodes: HashMap<u32, NodeInfo>,
    /// Link id -> (output node, input node).
    pub links: HashMap<u32, (u32, u32)>,
    /// Subject-0 entries of the "default" metadata, raw JSON values as
    /// `system::get_default` returns them.
    pub defaults: HashMap<String, String>,
    defaults_id: Option<u32>,
    /// (volume on wpctl's cubic scale, muted) of the watched nodes.
    pub volumes: HashMap<u32, (f32, bool)>,
}

impl GraphState {
    pub fn node_id(&self, name: &str) -> Option<u32> {
        self.nodes.values().find(|n| n.name == name).map(|n| n.id)
    }

    pub fn node(&self, name: &str) -> Option<&NodeInfo> {
        self.nodes.values().find(|n| n.name == name)
    }

    /// Raw JSON value of a "default" metadata key.
    pub fn default_raw(&self, key: &str) -> Option<String> {
        self.defaults.get(key).cloned()
    }

    /// Node name in a "default" metadata key, like `system::default_name`.
    pub fn default_name(&self, key: &str) -> Option<String> {
        let v: serde_json::Value = serde_json::from_str(self.defaults.get(key)?).ok()?;
        v.get("name")?.as_str().map(String::from)
    }

    pub fn volume(&self, id: u32) -> Option<(f32, bool)> {
        self.volumes.get(&id).copied()
    }
}

#[derive(Default)]
pub struct Graph {
    state: Mutex<GraphState>,
    changed: Condvar,
}

impl Graph {
    fn update(&self, f: impl FnOnce(&mut GraphState)) {
        let mut s = self.state.lock().unwrap();
        f(&mut s);
        s.generation += 1;
        self.changed.notify_all();
    }

    /// Wakes the monitor without a graph change, e.g. for a client's request.
    pub fn poke(&self) {
        self.update(|_| {});
    }

    pub fn snapshot(&self) -> GraphState {
        self.state.lock().unwrap().clone()
    }

    /// Waits until the graph has changed since `generation`, or `timeout` passes,
    /// and returns the state once a burst of changes has settled.
    pub fn wait_change(&self, generation: u64, timeout: Duration) -> GraphState {
        let s = self.state.lock().unwrap();
        let (s, res) = self.changed.wait_timeout_while(s, timeout, |s| s.generation == generation).unwrap();
        if res.timed_out() {
            return s.clone();
        }
        drop(s);
        std::thread::sleep(DEBOUNCE);
        self.snapshot()
    }

    /// Waits up to `timeout` for `f` to return `Some`; on timeout, the last state.
    pub fn wait_for<T>(&self, timeout: Duration, mut f: impl FnMut(&GraphState) -> Option<T>) -> Result<T, GraphState> {
        let deadline = Instant::now() + timeout;
        let mut s = self.state.lock().unwrap();
        loop {
            if let Some(v) = f(&s) {
                return Ok(v);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(s.clone());
            }
            s = self.changed.wait_timeout(s, left).unwrap().0;
        }
    }
}

/// (volume, muted) from a node's Props param, if this Props object carries them.
/// The volume is on the cubic scale `wpctl` shows: the cube root of the linear
/// channel volumes' average.
fn parse_props(pod: &Pod) -> Option<(f32, bool)> {
    let obj = pod.as_object().ok()?;
    let vols = obj.find_prop(Id(pw::spa::sys::SPA_PROP_channelVolumes))?;
    let Ok((_, Value::ValueArray(ValueArray::Float(v)))) = PodDeserializer::deserialize_any_from(vols.value().as_bytes()) else {
        return None;
    };
    if v.is_empty() {
        return None;
    }
    let mute = obj.find_prop(Id(pw::spa::sys::SPA_PROP_mute)).and_then(|p| p.value().get_bool().ok()).unwrap_or(false);
    Some(((v.iter().sum::<f32>() / v.len() as f32).cbrt(), mute))
}

/// Proxies kept bound for their events.
enum Bound {
    Node(#[allow(dead_code)] Node, #[allow(dead_code)] NodeListener),
    Metadata(#[allow(dead_code)] Metadata, #[allow(dead_code)] MetadataListener),
}

/// Keeps the registry subscription alive; events stop when it is dropped.
pub struct Watcher {
    _listener: pw::registry::Listener,
    _bound: Rc<RefCell<HashMap<u32, Bound>>>,
    _registry: RegistryRc,
}

/// Starts mirroring the graph into `graph`. Volumes are tracked for the
/// speaker sink `target` and the Neiro sink only. Must run on the main loop's thread.
pub fn watch(core: &pw::core::CoreRc, target: &str, graph: Arc<Graph>) -> Result<Watcher> {
    let registry = core.get_registry_rc()?;
    let bound: Rc<RefCell<HashMap<u32, Bound>>> = Rc::default();
    let weak = registry.downgrade();
    let (g, b, target) = (graph.clone(), bound.clone(), target.to_string());
    let (g2, b2) = (graph, bound.clone());
    let listener = registry
        .add_listener_local()
        .global(move |obj| {
            let Some(props) = obj.props else { return };
            match obj.type_ {
                ObjectType::Node => {
                    let info = NodeInfo {
                        id: obj.id,
                        name: props.get("node.name").unwrap_or_default().to_string(),
                        class: props.get("media.class").unwrap_or_default().to_string(),
                        description: props.get("node.description").unwrap_or_default().to_string(),
                    };
                    let watched = info.name == target || info.name == SINK_NAME;
                    g.update(|s| {
                        s.nodes.insert(obj.id, info);
                    });
                    if !watched {
                        return;
                    }
                    let Some(node) = weak.upgrade().and_then(|r| r.bind::<Node, _>(obj).ok()) else {
                        log::warn!("couldn't bind node {} to follow its volume", obj.id);
                        return;
                    };
                    let (g, id) = (g.clone(), obj.id);
                    let l = node
                        .add_listener_local()
                        .param(move |_, _, _, _, pod| {
                            if let Some(v) = pod.and_then(parse_props) {
                                log::trace!("node {id} volume {:.3}, muted {}", v.0, v.1);
                                g.update(|s| {
                                    s.volumes.insert(id, v);
                                });
                            }
                        })
                        .register();
                    node.subscribe_params(&[ParamType::Props]);
                    b.borrow_mut().insert(obj.id, Bound::Node(node, l));
                }
                ObjectType::Link => {
                    let get = |k| props.get(k).and_then(|v: &str| v.parse::<u32>().ok());
                    if let (Some(o), Some(i)) = (get("link.output.node"), get("link.input.node")) {
                        g.update(|s| {
                            s.links.insert(obj.id, (o, i));
                        });
                    }
                }
                ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
                    let Some(meta) = weak.upgrade().and_then(|r| r.bind::<Metadata, _>(obj).ok()) else {
                        log::warn!("couldn't bind the default metadata");
                        return;
                    };
                    let (g, id) = (g.clone(), obj.id);
                    g.update(|s| {
                        s.defaults.clear();
                        s.defaults_id = Some(id);
                    });
                    let l = meta
                        .add_listener_local()
                        .property(move |subject, key, _, value| {
                            // Subject 0 is the core: the global defaults.
                            if subject == 0 {
                                g.update(|s| match (key, value) {
                                    (None, _) => s.defaults.clear(),
                                    (Some(k), None) => {
                                        s.defaults.remove(k);
                                    }
                                    (Some(k), Some(v)) => {
                                        s.defaults.insert(k.to_string(), v.to_string());
                                    }
                                });
                            }
                            0
                        })
                        .register();
                    b.borrow_mut().insert(obj.id, Bound::Metadata(meta, l));
                }
                _ => {}
            }
        })
        .global_remove(move |id| {
            b2.borrow_mut().remove(&id);
            g2.update(|s| {
                s.nodes.remove(&id);
                s.links.remove(&id);
                s.volumes.remove(&id);
                if s.defaults_id == Some(id) {
                    s.defaults.clear();
                    s.defaults_id = None;
                }
            });
        })
        .register();
    Ok(Watcher { _listener: listener, _bound: bound, _registry: registry })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pw::spa::pod::serialize::PodSerializer;
    use pw::spa::pod::{Object, Property};

    fn props_pod(vols: &[f32], mute: Option<bool>) -> Vec<u8> {
        let mut properties = vec![Property::new(pw::spa::sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(vols.to_vec())))];
        if let Some(m) = mute {
            properties.push(Property::new(pw::spa::sys::SPA_PROP_mute, Value::Bool(m)));
        }
        let obj = Value::Object(Object {
            type_: pw::spa::sys::SPA_TYPE_OBJECT_Props,
            id: pw::spa::sys::SPA_PARAM_Props,
            properties,
        });
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &obj).unwrap().0.into_inner()
    }

    #[test]
    fn props_volume_is_on_the_cubic_scale() {
        let b = props_pod(&[0.03278, 0.03278], Some(true));
        let (v, m) = parse_props(Pod::from_bytes(&b).unwrap()).unwrap();
        // wpctl shows this node's volume as 0.32.
        assert!((v - 0.32).abs() < 1e-3 && m, "{v} {m}");
        let b = props_pod(&[1.0, 1.0], None);
        assert_eq!(parse_props(Pod::from_bytes(&b).unwrap()), Some((1.0, false)));
        // A Props object without volumes (ALSA nodes send one) is ignored.
        let b = props_pod(&[], Some(false));
        assert_eq!(parse_props(Pod::from_bytes(&b).unwrap()), None);
    }

    /// Read-only check against the running PipeWire server:
    /// `cargo test --release live_graph -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a running PipeWire"]
    fn live_graph() {
        pw::init();
        let ml = pw::main_loop::MainLoopRc::new(None).unwrap();
        let ctx = pw::context::ContextRc::new(&ml, None).unwrap();
        let core = ctx.connect_rc(None).unwrap();
        let target = crate::system::find_speaker(None).unwrap();
        let g = Arc::new(Graph::default());
        let _w = watch(&core, &target, g.clone()).unwrap();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(500) {
            ml.loop_().iterate(pw::loop_::Timeout::Finite(Duration::from_millis(50)));
        }
        let s = g.snapshot();
        let spk = s.node_id(&target).expect("speaker node");
        println!("{} nodes, {} links, generation {}", s.nodes.len(), s.links.len(), s.generation);
        println!("defaults: {:?}", s.defaults);
        println!("speaker {target} ({spk}): {:?}", s.volume(spk));
        if let Some(sink) = s.node_id(SINK_NAME) {
            println!("{SINK_NAME} ({sink}): {:?}", s.volume(sink));
            println!("links into the speaker: {:?}", s.links.values().filter(|l| l.1 == spk).collect::<Vec<_>>());
        }
        assert!(s.volume(spk).is_some() && s.default_raw("default.audio.sink").is_some());
        // Same values the tools report.
        let (v, m) = crate::system::get_volume(spk).unwrap();
        let (gv, gm) = s.volume(spk).unwrap();
        assert!((v - gv).abs() < 0.006 && m == gm, "wpctl {v} {m}, graph {gv} {gm}");
        assert_eq!(s.default_raw("default.audio.sink"), crate::system::get_default("default.audio.sink"));
    }

    #[test]
    fn waits_for_changes_and_conditions() {
        let g = Arc::new(Graph::default());
        let g0 = g.snapshot().generation;
        let g2 = g.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            g2.update(|s| {
                s.defaults.insert("default.audio.sink".into(), r#"{"name":"neiro_sink"}"#.into());
            });
        });
        let s = g.wait_change(g0, Duration::from_secs(5));
        assert!(s.generation > g0);
        assert_eq!(s.default_name("default.audio.sink").as_deref(), Some("neiro_sink"));
        t.join().unwrap();
        let r = g.wait_for(Duration::from_millis(30), |s| s.node_id("nope"));
        assert!(r.is_err());
        assert_eq!(g.wait_for(Duration::from_millis(30), |s| s.default_raw("default.audio.sink")).unwrap(), r#"{"name":"neiro_sink"}"#);
    }
}
