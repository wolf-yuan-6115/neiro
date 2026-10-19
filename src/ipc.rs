//! Newline-delimited JSON over a Unix socket between `neiro daemon` and its clients.

use crate::dsp::chain::MeterSnapshot;
use crate::params::{Params, PersonalEq, Section};
use crate::tuner::Choice;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("neiro.sock")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    GetState,
    Set { path: String, value: serde_json::Value },
    SetProfile { profile: String },
    /// `None` toggles.
    Bypass { on: Option<bool> },
    Save,
    Reset,
    Meters,
    /// Start a blind A/B session for the personal EQ.
    TunerStart { source: TunerSource },
    /// Which side to hear: 0 = A, 1 = B.
    TunerListen { side: usize },
    TunerAnswer { choice: Choice },
    /// Alternate A/B automatically every `secs` seconds (None = manual).
    TunerAutoSwitch { secs: Option<f32> },
    /// End the search early and show the result.
    TunerFinish,
    /// Leave the session, keeping the result as the personal EQ or discarding it.
    TunerStop { keep: bool },
    /// Make the Neiro DSP sink the default output (when the speaker is).
    MakeDefault,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TunerSource {
    Demo {
        #[serde(default)]
        demo: crate::demo::Demo,
    },
    File { path: String },
    /// One of the embedded real-music excerpts, by id.
    Sample { id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunerView {
    pub clip: String,
    pub segment: String,
    pub listening: usize,
    pub auto_secs: Option<f32>,
    pub trial: usize,
    pub total: usize,
    pub done: bool,
    pub low_confidence: bool,
    /// Only revealed once the session is done, so the test stays blind.
    pub result: Option<PersonalEq>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateView {
    pub profile: String,
    pub profiles: Vec<String>,
    pub bypass: bool,
    pub dirty: bool,
    pub customised: bool,
    pub params: Params,
    pub sections: Vec<Section>,
    pub band_freqs: Vec<f32>,
    pub latency_ms: f32,
    pub target: String,
    /// Regulator threshold offset compensating for hardware volume below 100%.
    pub hw_comp_db: f32,
    pub pinned: bool,
    #[serde(default)]
    pub personal: PersonalEq,
    #[serde(default)]
    pub tuner: Option<TunerView>,
    /// The current default output (node name, description), if any.
    #[serde(default)]
    pub default_output: Option<(String, String)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Response {
    pub error: Option<String>,
    pub state: Option<StateView>,
    pub meters: Option<MeterSnapshot>,
}

pub trait Handler: Send + Sync + 'static {
    fn handle(&self, req: Request) -> Response;
}

/// Fails if a daemon already answers on the control socket.
pub fn ensure_no_daemon() -> Result<()> {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        return Err(anyhow!("another neiro daemon is already listening on {}", path.display()));
    }
    Ok(())
}

pub fn serve<H: Handler>(handler: std::sync::Arc<H>) -> Result<()> {
    ensure_no_daemon()?;
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            log::trace!("control connection");
            let h = handler.clone();
            std::thread::spawn(move || {
                let mut w = match conn.try_clone() {
                    Ok(w) => w,
                    Err(_) => return,
                };
                for line in BufReader::new(conn).lines() {
                    let Ok(line) = line else { break };
                    let resp = match serde_json::from_str::<Request>(&line) {
                        Ok(req) => h.handle(req),
                        Err(e) => {
                            log::debug!("bad request {line:?}: {e}");
                            Response { error: Some(format!("bad request: {e}")), ..Default::default() }
                        }
                    };
                    let mut out = serde_json::to_string(&resp).unwrap_or_default();
                    out.push('\n');
                    if w.write_all(out.as_bytes()).is_err() {
                        break;
                    }
                }
            });
        }
    });
    Ok(())
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    pub fn connect() -> Result<Client> {
        let s = UnixStream::connect(socket_path()).context("neiro daemon is not running")?;
        Ok(Client { reader: BufReader::new(s.try_clone()?), writer: s })
    }

    pub fn call(&mut self, req: &Request) -> Result<Response> {
        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        let mut buf = String::new();
        if self.reader.read_line(&mut buf)? == 0 {
            return Err(anyhow!("daemon closed the connection"));
        }
        let resp: Response = serde_json::from_str(&buf)?;
        if let Some(e) = &resp.error {
            return Err(anyhow!("{e}"));
        }
        Ok(resp)
    }
}
