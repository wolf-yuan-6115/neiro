//! PipeWire glue, modelled on module-loopback: a capture stream that shows up
//! as the "Neiro DSP" sink and a playback stream into the hardware
//! speaker. The capture side only triggers playback; all work happens in the
//! playback process callback so no extra buffering latency is added.

use crate::dsp::chain::{Config, Engine, SharedMeters};
use anyhow::Result;
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::stream::{StreamFlags, StreamRc};
use spa::pod::Pod;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const SINK_NAME: &str = "neiro_sink";
pub const SINK_DESC: &str = "Neiro DSP";
pub const RATE: u32 = 48000;
const MAX_FRAMES: usize = 8192;

pub struct RtState {
    pub engine: Engine,
    pub rx: rtrb::Consumer<Box<Config>>,
    pub back: rtrb::Producer<Box<Config>>,
    pub meters: Arc<SharedMeters>,
    l: Vec<f32>,
    r: Vec<f32>,
    ol: Vec<f32>,
    or: Vec<f32>,
    /// Set by the daemon on shutdown: ramp the output to silence.
    stopping: Arc<AtomicBool>,
    /// Set here once the output has reached silence.
    faded: Arc<AtomicBool>,
    gain: f32,
}

/// Shutdown fade length; long enough to avoid a click, short enough to feel instant.
const FADE_MS: f32 = 50.0;

impl RtState {
    pub fn new(
        engine: Engine,
        rx: rtrb::Consumer<Box<Config>>,
        back: rtrb::Producer<Box<Config>>,
        meters: Arc<SharedMeters>,
        stopping: Arc<AtomicBool>,
        faded: Arc<AtomicBool>,
    ) -> RtState {
        RtState {
            engine,
            rx,
            back,
            meters,
            l: vec![0.0; MAX_FRAMES],
            r: vec![0.0; MAX_FRAMES],
            ol: vec![0.0; MAX_FRAMES],
            or: vec![0.0; MAX_FRAMES],
            stopping,
            faded,
            gain: 1.0,
        }
    }
}

fn format_pod() -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(RATE);
    info.set_channels(2);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = pw::spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: pw::spa::sys::SPA_TYPE_OBJECT_Format,
            id: pw::spa::sys::SPA_PARAM_EnumFormat,
            properties: info.into(),
        }),
    )
    .expect("serialize format")
    .0
    .into_inner()
}

/// Runs the PipeWire main loop until SIGINT/SIGTERM. `target` is the hardware sink's node.name.
pub fn run(
    mut rt: RtState,
    target: &str,
    on_started: impl FnOnce() + 'static,
    on_quit: impl FnOnce(),
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let playback = StreamRc::new(
        core.clone(),
        "neiro-output",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::NODE_NAME => "output.neiro",
            *pw::keys::NODE_DESCRIPTION => "Neiro DSP output",
            "node.group" => "neiro",
            "node.link-group" => "neiro",
            "node.passive" => "true",
            "node.dont-reconnect" => "true",
            // neiro manages volume and routing itself; WirePlumber's per-stream memory
            // would race the startup handoff and restore stale levels.
            "state.restore-props" => "false",
            "state.restore-target" => "false",
            "stream.dont-remix" => "true",
            "target.object" => target,
            *pw::keys::AUDIO_CHANNELS => "2",
            "audio.position" => "FL,FR",
        },
    )?;
    let capture = StreamRc::new(
        core.clone(),
        "neiro-input",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CLASS => "Audio/Sink",
            *pw::keys::NODE_NAME => SINK_NAME,
            *pw::keys::NODE_DESCRIPTION => SINK_DESC,
            "node.group" => "neiro",
            "node.link-group" => "neiro",
            "device.icon-name" => "audio-speakers",
            "state.restore-props" => "false",
            "state.restore-target" => "false",
            *pw::keys::AUDIO_CHANNELS => "2",
            "audio.position" => "FL,FR",
        },
    )?;

    let cap = capture.clone();
    let _pb_listener = playback
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            // Newest capture buffer wins; older ones are requeued as they drop.
            let mut inb = None;
            while let Some(b) = cap.dequeue_buffer() {
                inb = Some(b);
            }
            let Some(mut out) = stream.dequeue_buffer() else { return };
            let st = &mut rt;
            let mut n = 0;
            if let Some(mut ib) = inb {
                if let Some(d) = ib.datas_mut().first_mut() {
                    let (off, size) = (d.chunk().offset() as usize, d.chunk().size() as usize);
                    if let Some(bytes) = d.data() {
                        let end = (off + size).min(bytes.len());
                        let bytes = &bytes[off.min(end)..end];
                        n = (bytes.len() / 8).min(MAX_FRAMES);
                        for i in 0..n {
                            let s = &bytes[i * 8..i * 8 + 8];
                            st.l[i] = f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                            st.r[i] = f32::from_le_bytes([s[4], s[5], s[6], s[7]]);
                        }
                    }
                }
            }
            if n == 0 {
                n = (out.requested() as usize).clamp(1, MAX_FRAMES);
                st.l[..n].fill(0.0);
                st.r[..n].fill(0.0);
            }
            while let Ok(c) = st.rx.pop() {
                st.engine.set_config(&c);
                let _ = st.back.push(c);
            }
            let datas = out.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let Some(bytes) = d.data() else { return };
            n = n.min(bytes.len() / 8);
            st.engine.process(&st.l[..n], &st.r[..n], &mut st.ol[..n], &mut st.or[..n]);
            st.meters.store(&st.engine.meters, &st.engine.peaks);
            if st.stopping.load(Ordering::Relaxed) {
                // Ramp to silence so disconnecting doesn't cut the waveform mid-cycle.
                let step = 1.0 / (FADE_MS * 1e-3 * RATE as f32);
                for i in 0..n {
                    st.gain = (st.gain - step).max(0.0);
                    st.ol[i] *= st.gain;
                    st.or[i] *= st.gain;
                }
                if st.gain == 0.0 {
                    st.faded.store(true, Ordering::Release);
                }
            }
            for i in 0..n {
                bytes[i * 8..i * 8 + 4].copy_from_slice(&st.ol[i].to_le_bytes());
                bytes[i * 8 + 4..i * 8 + 8].copy_from_slice(&st.or[i].to_le_bytes());
            }
            let chunk = d.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = 8;
            *chunk.size_mut() = (n * 8) as u32;
        })
        .register()?;

    let pb = playback.clone();
    let _cap_listener = capture
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            if pb.trigger_process().is_err() {
                // Playback not ready: recycle our input so the graph keeps moving.
                while let Some(b) = stream.dequeue_buffer() {
                    drop(b);
                }
            }
        })
        .register()?;

    let fmt = format_pod();
    let mut params = [Pod::from_bytes(&fmt).unwrap()];
    let base = StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS;
    playback.connect(spa::utils::Direction::Output, None, base | StreamFlags::TRIGGER, &mut params)?;
    let mut params = [Pod::from_bytes(&fmt).unwrap()];
    let async_flag = StreamFlags::from_bits_retain(pw::sys::pw_stream_flags_PW_STREAM_FLAG_ASYNC);
    capture.connect(spa::utils::Direction::Input, None, base | async_flag, &mut params)?;

    let ml = mainloop.clone();
    let _sigint = mainloop.loop_().add_signal_local(pw::loop_::Signal::INT, move || ml.quit());
    let ml = mainloop.clone();
    let _sigterm = mainloop.loop_().add_signal_local(pw::loop_::Signal::TERM, move || ml.quit());

    on_started();
    mainloop.run();
    on_quit();
    let _ = capture.disconnect();
    let _ = playback.disconnect();
    Ok(())
}
