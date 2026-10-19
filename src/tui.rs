//! `neiro tui`: ratatui front-end for the daemon.

use crate::dsp::chain::MeterSnapshot;
use crate::ipc::{Client, Request, StateView, TunerSource, TunerView};
use crate::params::{Kind, PersonalEq, Spec, ACTION_TUNER, PERSONAL_PREFIX, PE_BANDS, PE_NAMES};
use crate::demo::Demo;
use crate::samples::SAMPLES;
use crate::tuner::Choice;
use std::path::PathBuf;
use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs};
use ratatui::{DefaultTerminal, Frame};
use std::time::{Duration, Instant};

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Main,
    /// Choosing the A/B source.
    Pick,
    Tuner,
}

const AUTO_SWITCH_SECS: f32 = 4.0;
const AUDIO_EXT: [&str; 8] = ["flac", "mp3", "m4a", "opus", "ogg", "wav", "aac", "wma"];

/// "Neiro" in the FIGlet font Larry 3D.
const BANNER: [&str; 7] = [
    r" __  __",
    r"/\ \/\ \          __",
    r"\ \ `\\ \     __ /\_\  _ __   ___",
    r" \ \ , ` \  /'__`\/\ \/\`'__\/ __`\",
    r"  \ \ \`\ \/\  __/\ \ \ \ \//\ \L\ \",
    r"   \ \_\ \_\ \____\\ \_\ \_\\ \____/",
    r"    \/_/\/_/\/____/ \/_/\/_/ \/___/",
];

/// The banner with a left-to-right cyan → magenta gradient. Lines are padded to
/// one width so they stay aligned when centred.
fn banner_lines() -> Vec<Line<'static>> {
    let w = BANNER.iter().map(|l| l.chars().count()).max().unwrap_or(1);
    let (from, to) = ((0u8, 200u8, 255u8), (230u8, 70u8, 255u8));
    let mix = |a: u8, b: u8, t: f32| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    BANNER
        .iter()
        .map(|l| {
            let spans: Vec<Span> = format!("{l:<w$}")
                .chars()
                .enumerate()
                .map(|(x, c)| {
                    let t = x as f32 / (w - 1).max(1) as f32;
                    let color = Color::Rgb(mix(from.0, to.0, t), mix(from.1, to.1, t), mix(from.2, to.2, t));
                    Span::styled(c.to_string(), Style::new().fg(color).bold())
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// Look up a descriptor path in the profile params or the global personal EQ.
fn value(st: &StateView, path: &str) -> Option<serde_json::Value> {
    match path.strip_prefix(PERSONAL_PREFIX) {
        Some(rest) => serde_json::to_value(&st.personal).ok()?.pointer(rest).cloned(),
        None => st.params.get(path),
    }
}

fn music_dir() -> PathBuf {
    dirs::audio_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Music"))
}

fn list_audio(dir: &std::path::Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if p.is_dir() && depth > 0 {
            list_audio(&p, depth - 1, out);
        } else if p.extension().is_some_and(|x| AUDIO_EXT.contains(&x.to_string_lossy().to_lowercase().as_str())) {
            out.push(p);
        }
    }
}

/// ASCII plot of an EQ's magnitude response, ±12 dB, 20 Hz – 20 kHz.
fn curve_lines(eq: &PersonalEq, w: usize, h: usize) -> Vec<Line<'static>> {
    let h = h.max(3);
    let w = w.max(10);
    let range = 12.0;
    let col_db: Vec<f32> = (0..w)
        .map(|x| eq.response_db(20.0 * 1000f32.powf(x as f32 / (w - 1) as f32)) + if eq.enable { eq.pregain_db } else { 0.0 })
        .collect();
    // Half-block characters give two vertical steps per text row.
    let sub_of = |db: f32| (((range - db.clamp(-range, range)) / (2.0 * range)) * (2 * h - 1) as f32).round() as usize;
    let zero = sub_of(0.0) / 2;
    let mut lines = vec![];
    for row in 0..h {
        let spans: Vec<Span> = col_db
            .iter()
            .map(|&db| {
                let sub = sub_of(db);
                if sub / 2 == row {
                    let ch = if sub % 2 == 0 { "▀" } else { "▄" };
                    Span::styled(ch, Style::new().fg(if db >= 0.0 { Color::Cyan } else { Color::Magenta }))
                } else if row == zero {
                    Span::raw("─").dim()
                } else {
                    Span::raw(" ")
                }
            })
            .collect();
        lines.push(Line::from(spans));
    }
    let mut axis = vec![' '; w];
    for (f, label) in [(20.0f32, "20"), (100.0, "100"), (1000.0, "1k"), (10000.0, "10k")] {
        let x = ((f / 20.0).log10() / 3.0 * (w - 1) as f32).round() as usize;
        for (i, ch) in label.chars().enumerate() {
            if x + i < w {
                axis[x + i] = ch;
            }
        }
    }
    lines.push(Line::from(axis.into_iter().collect::<String>()).dim());
    lines
}

const HOVER_BG: Color = Color::Rgb(44, 48, 68);

/// Which part of a parameter row the pointer is on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Part {
    Label,
    Slider,
    Toggle,
    Choice(usize),
    Action,
}

/// A clickable or hoverable element, recorded with its screen area while drawing.
#[derive(Debug, Clone, PartialEq)]
enum Hit {
    StartDaemon,
    Power,
    Tab(usize),
    Stage { idx: usize, checkbox: bool },
    Param { idx: usize, part: Part },
    Pick(usize),
    Listen(usize),
    Answer(Choice),
    AutoSwitch,
    Keep(bool),
    Band(usize),
    Level(usize),
}

impl Hit {
    /// Footer hint while hovering.
    fn hint(&self) -> Option<&'static str> {
        Some(match self {
            Hit::StartDaemon => " click: start the neiro daemon",
            Hit::Power => " click: turn processing on/off",
            Hit::Tab(_) => " click: switch to this profile · wheel: next/previous profile",
            Hit::Stage { checkbox: true, .. } => " click: enable/disable this stage",
            Hit::Stage { .. } => " click: open this stage · wheel: move through stages",
            Hit::Param { part: Part::Slider, .. } => " click or drag: set value · wheel: fine adjust",
            Hit::Param { part: Part::Toggle, .. } => " click: toggle",
            Hit::Param { part: Part::Choice(_), .. } => " click: choose this option",
            Hit::Param { part: Part::Action, .. } => " click: start the blind A/B tuner",
            Hit::Param { part: Part::Label, .. } => " click: select · wheel: scroll",
            Hit::Pick(_) => " click: select · click again: start the tuner with it",
            Hit::Listen(_) => " click: listen to this side",
            Hit::Answer(Choice::Same) => " click: you can't hear a difference",
            Hit::Answer(_) => " click: this side sounds better",
            Hit::AutoSwitch => " click: toggle automatic A/B switching",
            Hit::Keep(true) => " click: keep this as your personal EQ",
            Hit::Keep(false) => " click: discard this result",
            Hit::Band(_) | Hit::Level(_) => return None,
        })
    }
}

fn contains(m: Option<(u16, u16)>, r: Rect) -> bool {
    m.is_some_and(|(x, y)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
}

fn hover(style: Style) -> Style {
    style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

/// Builds one line span by span, recording where its buttons land.
struct RowBuilder {
    spans: Vec<Span<'static>>,
    x: u16,
    y: u16,
    mouse: Option<(u16, u16)>,
    hits: Vec<(Rect, Hit)>,
}

impl RowBuilder {
    fn new(x: u16, y: u16, mouse: Option<(u16, u16)>) -> RowBuilder {
        RowBuilder { spans: vec![], x, y, mouse, hits: vec![] }
    }

    fn text(&mut self, s: impl Into<String>, style: Style) -> &mut Self {
        let span = Span::styled(s.into(), style);
        self.x += span.width() as u16;
        self.spans.push(span);
        self
    }

    fn button(&mut self, s: impl Into<String>, style: Style, hit: Hit) -> &mut Self {
        let s = s.into();
        let w = Span::raw(s.as_str()).width() as u16;
        let r = Rect::new(self.x, self.y, w, 1);
        let style = if contains(self.mouse, r) { hover(style) } else { style };
        self.hits.push((r, hit));
        self.text(s, style)
    }

    fn finish(self, hits: &mut Vec<(Rect, Hit)>) -> Line<'static> {
        hits.extend(self.hits);
        Line::from(self.spans)
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Focus {
    Sections,
    Params,
}

struct App {
    client: Option<Client>,
    state: Option<StateView>,
    meters: MeterSnapshot,
    focus: Focus,
    sec: usize,
    item: usize,
    items_state: ListState,
    msg: Option<(String, Instant)>,
    last_connect: Option<Instant>,
    confirm_reset: bool,
    /// Peak-hold per meter: (dBFS, when it was set).
    hold: [(f32, Instant); 4],
    screen: Screen,
    files: Vec<PathBuf>,
    filter: String,
    pick: usize,
    /// Whether ffmpeg is installed; without it only the synthesised loops can play.
    ffmpeg_ok: bool,
    /// Clickable areas from the last frame; later entries win.
    hits: Vec<(Rect, Hit)>,
    mouse: Option<(u16, u16)>,
    /// Slider being dragged: (parameter index, slider area).
    drag: Option<(usize, Rect)>,
    pick_offset: usize,
    /// Requests issued, recorded so tests can check what a click did.
    #[cfg(test)]
    sent: Vec<Request>,
}

const VU_FLOOR: f32 = -60.0;
const HOLD_SECS: f32 = 1.0;
const HOLD_DECAY_DB_S: f32 = 20.0;

fn held(h: (f32, Instant)) -> f32 {
    let t = h.1.elapsed().as_secs_f32();
    if t < HOLD_SECS { h.0 } else { h.0 - (t - HOLD_SECS) * HOLD_DECAY_DB_S }
}

pub fn run() -> Result<()> {
    let mut terminal = ratatui::init();
    // Report clicks, drags, wheel and plain pointer movement (for hover).
    let _ = ratatui::crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let mut app = App::new();
    let res = app.main_loop(&mut terminal);
    // Don't leave the A/B clip looping after the UI is gone.
    if app.state.as_ref().is_some_and(|s| s.tuner.is_some()) {
        app.request(Request::TunerStop { keep: false });
    }
    let _ = ratatui::crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

impl App {
    fn new() -> App {
        App {
            client: None,
            state: None,
            meters: MeterSnapshot::default(),
            focus: Focus::Sections,
            sec: 0,
            item: 0,
            items_state: ListState::default(),
            msg: None,
            last_connect: None,
            confirm_reset: false,
            hold: [(VU_FLOOR, Instant::now()); 4],
            screen: Screen::Main,
            files: vec![],
            filter: String::new(),
            pick: 0,
            ffmpeg_ok: true,
            hits: vec![],
            mouse: None,
            drag: None,
            pick_offset: 0,
            #[cfg(test)]
            sent: vec![],
        }
    }

    fn flash(&mut self, m: impl Into<String>) {
        self.msg = Some((m.into(), Instant::now()));
    }

    fn connect(&mut self) {
        if self.client.is_some() || self.last_connect.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.last_connect = Some(Instant::now());
        if let Ok(c) = Client::connect() {
            self.client = Some(c);
            self.request(Request::GetState);
        }
    }

    fn request(&mut self, req: Request) {
        #[cfg(test)]
        self.sent.push(req.clone());
        let Some(c) = self.client.as_mut() else { return };
        match c.call(&req) {
            Ok(r) => {
                if let Some(s) = r.state {
                    // Follow the daemon: a running session always shows the tuner.
                    match (&s.tuner, self.screen) {
                        (Some(_), Screen::Main | Screen::Pick) => self.screen = Screen::Tuner,
                        (None, Screen::Tuner) => self.screen = Screen::Main,
                        _ => {}
                    }
                    self.state = Some(s);
                }
                if let Some(m) = r.meters {
                    for k in 0..4 {
                        if m.peak_db[k] >= held(self.hold[k]) {
                            self.hold[k] = (m.peak_db[k], Instant::now());
                        }
                    }
                    self.meters = m;
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("closed") || msg.contains("Broken pipe") || msg.contains("reset") {
                    self.client = None;
                    self.state = None;
                }
                self.flash(msg);
            }
        }
    }

    fn main_loop(&mut self, term: &mut DefaultTerminal) -> Result<()> {
        loop {
            self.connect();
            if self.client.is_some() {
                self.request(Request::Meters);
            }
            term.draw(|f| self.draw(f))?;
            // Wait briefly for input, then drain whatever queued up (mouse motion is chatty).
            let mut timeout = Duration::from_millis(50);
            while event::poll(timeout)? {
                timeout = Duration::ZERO;
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => {
                        if !self.key(k.code, k.modifiers) {
                            return Ok(());
                        }
                    }
                    Event::Mouse(m) => self.mouse(m),
                    _ => {}
                }
            }
        }
    }

    fn items(&self) -> Vec<Spec> {
        self.state.as_ref().and_then(|s| s.sections.get(self.sec)).map(|s| s.items.clone()).unwrap_or_default()
    }

    fn filtered(&self) -> Vec<(String, TunerSource)> {
        let f = self.filter.to_lowercase();
        let root = music_dir();
        let mut out = vec![];
        for s in &SAMPLES {
            let label = format!("♫ {:<24} {:<15} {} · {}", s.title, s.artist, s.style, s.license);
            if f.is_empty() || label.to_lowercase().contains(&f) {
                out.push((label, TunerSource::Sample { id: s.id.into() }));
            }
        }
        // Synthesised loops need no decoder, so they're the fallback when ffmpeg is missing.
        if !self.ffmpeg_ok {
            for d in Demo::ALL {
                let label = format!("★ Synth loop: {:<11} {}", d.label(), d.blurb());
                if f.is_empty() || label.to_lowercase().contains(&f) {
                    out.push((label, TunerSource::Demo { demo: d }));
                }
            }
        }
        for p in &self.files {
            let label = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().into_owned();
            if f.is_empty() || label.to_lowercase().contains(&f) {
                out.push((label, TunerSource::File { path: p.to_string_lossy().into_owned() }));
            }
        }
        out
    }

    fn open_picker(&mut self) {
        self.ffmpeg_ok = std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if self.files.is_empty() {
            list_audio(&music_dir(), 6, &mut self.files);
            self.files.sort();
        }
        self.filter.clear();
        self.pick = 0;
        self.screen = Screen::Pick;
    }

    fn key_pick(&mut self, code: KeyCode) {
        let n = self.filtered().len();
        match code {
            KeyCode::Esc => self.screen = Screen::Main,
            KeyCode::Up => self.pick = self.pick.saturating_sub(1),
            KeyCode::Down => self.pick = (self.pick + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.pick = self.pick.saturating_sub(10),
            KeyCode::PageDown => self.pick = (self.pick + 10).min(n.saturating_sub(1)),
            KeyCode::Backspace => {
                self.filter.pop();
                self.pick = 0;
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.pick = 0;
            }
            KeyCode::Enter => self.start_pick(),
            _ => {}
        }
    }

    fn start_pick(&mut self) {
        let Some((label, source)) = self.filtered().into_iter().nth(self.pick) else { return };
        self.flash(format!("preparing {label}…"));
        self.request(Request::TunerStart { source });
    }

    fn toggle_auto(&mut self) {
        let on = self.state.as_ref().and_then(|s| s.tuner.as_ref()).is_some_and(|t| t.auto_secs.is_some());
        self.request(Request::TunerAutoSwitch { secs: if on { None } else { Some(AUTO_SWITCH_SECS) } });
    }

    fn key_tuner(&mut self, code: KeyCode) {
        let Some(t) = self.state.as_ref().and_then(|s| s.tuner.clone()) else { return };
        if t.done {
            match code {
                KeyCode::Char('k') | KeyCode::Enter => {
                    self.request(Request::TunerStop { keep: true });
                    self.flash("personal EQ saved — it applies on every profile");
                }
                KeyCode::Char('d') | KeyCode::Esc => {
                    self.request(Request::TunerStop { keep: false });
                    self.flash("result discarded");
                }
                _ => {}
            }
            return;
        }
        match code {
            KeyCode::Char('a') | KeyCode::Left => self.request(Request::TunerListen { side: 0 }),
            KeyCode::Char('b') | KeyCode::Right => self.request(Request::TunerListen { side: 1 }),
            KeyCode::Char(' ') | KeyCode::Tab => self.request(Request::TunerListen { side: 1 - t.listening }),
            KeyCode::Char('1') => self.request(Request::TunerAnswer { choice: Choice::A }),
            KeyCode::Char('2') => self.request(Request::TunerAnswer { choice: Choice::B }),
            KeyCode::Char('0') | KeyCode::Char('=') => self.request(Request::TunerAnswer { choice: Choice::Same }),
            KeyCode::Char('x') => self.toggle_auto(),
            KeyCode::Esc => self.request(Request::TunerFinish),
            _ => {}
        }
    }

    /// Returns false to quit.
    fn key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        let reset_armed = std::mem::take(&mut self.confirm_reset);
        if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
            return false;
        }
        if self.state.is_none() {
            match code {
                KeyCode::Char('q') | KeyCode::Esc => return false,
                KeyCode::Char('d') => {
                    start_daemon();
                    self.flash("starting daemon…");
                    self.last_connect = None;
                }
                _ => {}
            }
            return true;
        }
        match self.screen {
            Screen::Pick => {
                self.key_pick(code);
                return true;
            }
            Screen::Tuner => {
                if code == KeyCode::Char('q') {
                    return false;
                }
                self.key_tuner(code);
                return true;
            }
            Screen::Main => {}
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            _ => {}
        }
        let nsec = self.state.as_ref().map_or(0, |s| s.sections.len());
        let coarse = mods.contains(KeyModifiers::SHIFT);
        match code {
            KeyCode::Char(' ') => self.request(Request::Bypass { on: None }),
            KeyCode::Char(c @ '1'..='6') => {
                let i = c as usize - '1' as usize;
                if let Some(p) = self.state.as_ref().and_then(|s| s.profiles.get(i)).cloned() {
                    self.request(Request::SetProfile { profile: p });
                    self.clamp_item();
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Sections { Focus::Params } else { Focus::Sections };
            }
            KeyCode::Char('s') => {
                self.request(Request::Save);
                self.flash("saved profile");
            }
            KeyCode::Char('R') => {
                if reset_armed {
                    self.request(Request::Reset);
                    self.clamp_item();
                    self.flash("profile reset to factory tuning");
                } else {
                    self.confirm_reset = true;
                    self.flash("press R again to reset this profile to factory tuning");
                }
            }
            KeyCode::Char('e') => self.toggle_section(),
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Sections => {
                    self.sec = (self.sec + nsec - 1) % nsec.max(1);
                    self.item = 0;
                }
                Focus::Params => self.item = self.item.saturating_sub(if coarse { 10 } else { 1 }),
            },
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Sections => {
                    self.sec = (self.sec + 1) % nsec.max(1);
                    self.item = 0;
                }
                Focus::Params => {
                    self.item += if coarse { 10 } else { 1 };
                    self.clamp_item();
                }
            },
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('L') => {
                if self.focus == Focus::Sections {
                    self.focus = Focus::Params;
                } else {
                    self.adjust(1.0, coarse || code == KeyCode::Char('L'));
                }
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('H') => {
                if self.focus == Focus::Params {
                    self.adjust(-1.0, coarse || code == KeyCode::Char('H'));
                }
            }
            KeyCode::Enter => match self.focus {
                Focus::Sections => self.toggle_section(),
                Focus::Params => match self.items().get(self.item).map(|i| i.kind.clone()) {
                    Some(Kind::Action(a)) => self.run_action(&a),
                    _ => self.adjust(1.0, false),
                },
            },
            _ => {}
        }
        true
    }

    fn clamp_item(&mut self) {
        let n = self.items().len();
        self.item = self.item.min(n.saturating_sub(1));
    }

    fn toggle_section(&mut self) {
        let Some(st) = &self.state else { return };
        let Some(path) = st.sections.get(self.sec).and_then(|s| s.enable.clone()) else { return };
        let cur = value(st, &path).and_then(|v| v.as_bool()).unwrap_or(false);
        self.request(Request::Set { path, value: serde_json::json!(!cur) });
    }

    fn run_action(&mut self, action: &str) {
        if action == ACTION_TUNER {
            self.open_picker();
        }
    }

    fn adjust(&mut self, dir: f32, coarse: bool) {
        let Some(st) = &self.state else { return };
        let Some(spec) = self.items().get(self.item).cloned() else { return };
        let cur = value(st, &spec.path).unwrap_or(serde_json::Value::Null);
        let value = match &spec.kind {
            Kind::Action(_) => return, // buttons only respond to Enter
            Kind::Bool => serde_json::json!(!cur.as_bool().unwrap_or(false)),
            Kind::Choice(opts) => {
                let i = opts.iter().position(|o| Some(o.as_str()) == cur.as_str()).unwrap_or(0) as i32;
                let n = opts.len() as i32;
                serde_json::json!(opts[((i + dir as i32) % n + n) as usize % opts.len()])
            }
            Kind::Float { step, .. } => {
                let v = cur.as_f64().unwrap_or(0.0) as f32;
                let mult = if coarse { 10.0 } else { 1.0 };
                self.send_float(&spec, v + dir * step * mult);
                return;
            }
        };
        self.request(Request::Set { path: spec.path, value });
    }

    /// Set a float parameter, snapped to its step and range; no-op if unchanged.
    fn send_float(&mut self, spec: &Spec, v: f32) {
        let Kind::Float { min, max, step, .. } = &spec.kind else { return };
        let Some(st) = &self.state else { return };
        let cur = value(st, &spec.path).unwrap_or(serde_json::Value::Null);
        let nv = ((v / step).round() * step).clamp(*min, *max);
        if cur.as_f64().is_some_and(|c| (c as f32 - nv).abs() < step * 0.01) {
            return;
        }
        let value = if cur.is_u64() { serde_json::json!(nv.round() as u64) } else { serde_json::json!((nv * 1000.0).round() / 1000.0) };
        self.request(Request::Set { path: spec.path.clone(), value });
    }

    fn hit_at(&self, x: u16, y: u16) -> Option<(Rect, Hit)> {
        self.hits.iter().rev().find(|(r, _)| contains(Some((x, y)), *r)).cloned()
    }

    fn hovered(&self) -> Option<Hit> {
        let (x, y) = self.mouse?;
        self.hit_at(x, y).map(|(_, h)| h)
    }

    fn mouse(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
        self.mouse = Some((x, y));
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((r, hit)) = self.hit_at(x, y) {
                    self.click(hit, r, x);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((idx, r)) = self.drag {
                    self.set_slider(idx, r, x);
                }
            }
            MouseEventKind::Up(_) => self.drag = None,
            MouseEventKind::ScrollUp => self.wheel(x, y, 1),
            MouseEventKind::ScrollDown => self.wheel(x, y, -1),
            _ => {}
        }
    }

    fn click(&mut self, hit: Hit, r: Rect, x: u16) {
        match hit {
            Hit::StartDaemon => {
                start_daemon();
                self.flash("starting daemon…");
                self.last_connect = None;
            }
            Hit::Power => self.request(Request::Bypass { on: None }),
            Hit::Tab(i) => {
                if let Some(p) = self.state.as_ref().and_then(|s| s.profiles.get(i)).cloned() {
                    self.request(Request::SetProfile { profile: p });
                    self.clamp_item();
                }
            }
            Hit::Stage { idx, checkbox } => {
                if self.sec != idx {
                    self.sec = idx;
                    self.item = 0;
                }
                self.focus = Focus::Sections;
                if checkbox {
                    self.toggle_section();
                }
            }
            Hit::Param { idx, part } => {
                self.item = idx;
                self.focus = Focus::Params;
                match part {
                    Part::Slider => {
                        self.drag = Some((idx, r));
                        self.set_slider(idx, r, x);
                    }
                    Part::Toggle => self.adjust(1.0, false),
                    Part::Choice(i) => {
                        if let Some(Spec { path, kind: Kind::Choice(opts), .. }) = self.items().get(idx).cloned() {
                            if let Some(o) = opts.get(i) {
                                self.request(Request::Set { path, value: serde_json::json!(o) });
                            }
                        }
                    }
                    Part::Action => {
                        if let Some(Kind::Action(a)) = self.items().get(idx).map(|s| s.kind.clone()) {
                            self.run_action(&a);
                        }
                    }
                    Part::Label => {}
                }
            }
            Hit::Pick(i) => {
                if self.pick == i {
                    self.start_pick();
                } else {
                    self.pick = i;
                }
            }
            Hit::Listen(side) => self.request(Request::TunerListen { side }),
            Hit::Answer(choice) => self.request(Request::TunerAnswer { choice }),
            Hit::AutoSwitch => self.toggle_auto(),
            Hit::Keep(keep) => {
                self.request(Request::TunerStop { keep });
                self.flash(if keep { "personal EQ saved — it applies on every profile" } else { "result discarded" });
            }
            Hit::Band(_) | Hit::Level(_) => {}
        }
    }

    /// Map a pointer column inside a slider to a value.
    fn set_slider(&mut self, idx: usize, r: Rect, x: u16) {
        let Some(spec) = self.items().get(idx).cloned() else { return };
        let Kind::Float { min, max, .. } = spec.kind else { return };
        let t = (x.saturating_sub(r.x) as f32 / r.width.saturating_sub(1).max(1) as f32).clamp(0.0, 1.0);
        self.send_float(&spec, min + t * (max - min));
    }

    /// Wheel up = +1: raises a hovered slider, otherwise moves list selections up.
    fn wheel(&mut self, x: u16, y: u16, dir: i32) {
        let step = |cur: usize, n: usize| (cur as i32 - dir).clamp(0, n.saturating_sub(1) as i32) as usize;
        match self.hit_at(x, y).map(|(_, h)| h) {
            Some(Hit::Param { idx, part: Part::Slider }) => {
                self.item = idx;
                self.focus = Focus::Params;
                self.adjust(dir as f32, false);
            }
            Some(Hit::Param { .. }) => {
                self.focus = Focus::Params;
                self.item = step(self.item, self.items().len());
            }
            Some(Hit::Stage { .. }) => {
                let n = self.state.as_ref().map_or(0, |s| s.sections.len());
                let next = step(self.sec, n);
                if next != self.sec {
                    self.sec = next;
                    self.item = 0;
                }
            }
            Some(Hit::Pick(_)) => self.pick = step(self.pick, self.filtered().len()),
            Some(Hit::Tab(_)) => {
                let Some(st) = &self.state else { return };
                let cur = st.profiles.iter().position(|p| *p == st.profile).unwrap_or(0);
                if let Some(p) = st.profiles.get(step(cur, st.profiles.len())).cloned() {
                    self.request(Request::SetProfile { profile: p });
                    self.clamp_item();
                }
            }
            _ => {}
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        self.hits.clear();
        let [header, body, meters, footer] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(8), Constraint::Length(9), Constraint::Length(1)])
                .areas(f.area());
        let Some(st) = self.state.clone() else {
            self.draw_not_running(f);
            return;
        };

        // Header: power badge (clickable) + profile tabs (clickable).
        let mut rb = RowBuilder::new(header.x + 1, header.y, self.mouse);
        rb.text(" neiro ", Style::new());
        if st.bypass {
            rb.button(" BYPASS ", Style::new().bg(Color::DarkGray).fg(Color::White).bold(), Hit::Power);
        } else {
            rb.button(" ● ON ", Style::new().bg(Color::Green).fg(Color::Black).bold(), Hit::Power);
        }
        if st.dirty {
            rb.text(" unsaved ", Style::new().fg(Color::Yellow));
        } else if st.customised {
            rb.text(" custom ", Style::new().fg(Color::Cyan));
        }
        let title = rb.finish(&mut self.hits);
        // Tabs render as: pad, title, pad, divider; mirror that to find each tab.
        let mut x = header.x + 1;
        let mut titles = vec![];
        for (i, p) in st.profiles.iter().enumerate() {
            let t = format!("{} {}", i + 1, p);
            let w = Span::raw(t.as_str()).width() as u16;
            let r = Rect::new(x, header.y + 1, w + 2, 1);
            self.hits.push((r, Hit::Tab(i)));
            titles.push(if contains(self.mouse, r) { Line::from(t).style(hover(Style::new())) } else { Line::from(t) });
            x += w + 3;
        }
        let sel = st.profiles.iter().position(|p| *p == st.profile).unwrap_or(0);
        let tabs = Tabs::new(titles)
            .select(sel)
            .highlight_style(Style::new().fg(Color::Black).bg(Color::Cyan).bold())
            .block(Block::bordered().title(title).title_bottom(
                Line::from(format!(
                    " → {} · {} · +{:.1} ms ",
                    st.target.trim_start_matches("alsa_output."),
                    if st.pinned { "hw vol pinned".to_string() } else { format!("hw vol comp +{:.0} dB", st.hw_comp_db) },
                    st.latency_ms
                ))
                .right_aligned()
                .dim(),
            ));
        f.render_widget(tabs, header);

        // Body: sections + parameters.
        let [left, right, levels] =
            Layout::horizontal([Constraint::Length(24), Constraint::Min(30), Constraint::Length(24)]).areas(body);
        self.draw_levels(f, levels);
        let main = Rect { width: right.x + right.width - left.x, ..left };
        match self.screen {
            Screen::Main => self.draw_main(f, left, right, &st),
            Screen::Pick => self.draw_picker(f, main),
            Screen::Tuner => self.draw_tuner(f, main, &st),
        }
        self.draw_meters(f, meters, &st);

        let help = match (&self.msg, self.hovered().and_then(|h| h.hint())) {
            (Some((m, t)), _) if t.elapsed() < Duration::from_secs(4) => Line::from(m.clone().yellow()),
            (_, Some(hint)) => Line::from(hint).cyan(),
            _ => Line::from(match self.screen {
                Screen::Main => " space on/off · 1-6 profile · tab focus · ↑↓ select · ←→ adjust · e toggle · s save · R reset · q quit",
                Screen::Pick => " type to filter · ↑↓ select · enter start · esc back",
                Screen::Tuner => " a/b or ←/→ listen · space flip · 1 prefer A · 2 prefer B · 0 no difference · x auto-switch · esc finish · q quit",
            })
            .dim(),
        };
        f.render_widget(Paragraph::new(help), footer);
    }

    fn draw_not_running(&mut self, f: &mut Frame) {
        let block = Block::bordered().title(" neiro ");
        let inner = block.inner(f.area());
        f.render_widget(block, f.area());
        let mut lines = banner_lines();
        lines.extend([Line::from(""), Line::from("音色").dim(), Line::from(""), Line::from(""), Line::from("The neiro daemon is not running.".bold()), Line::from("")]);
        let button_row = lines.len();
        let label = " ▶ Start daemon (d) ";
        lines.push(Line::from(""));
        lines.extend([Line::from(""), Line::from("q quit · install permanently with: neiro install").dim()]);
        let top = inner.height.saturating_sub(lines.len() as u16) / 2;
        let area = Rect { y: inner.y + top, height: inner.height - top, ..inner };
        // Paragraph centring puts a line at (width - line width) / 2.
        let w = Span::raw(label).width() as u16;
        let r = Rect::new(area.x + area.width.saturating_sub(w) / 2, area.y + button_row as u16, w, 1);
        self.hits.push((r, Hit::StartDaemon));
        let style = Style::new().fg(Color::Black).bg(Color::Cyan).bold();
        lines[button_row] = Line::from(Span::styled(label, if contains(self.mouse, r) { hover(style) } else { style }));
        f.render_widget(Paragraph::new(lines).centered(), area);
        let hint = if contains(self.mouse, r) { Hit::StartDaemon.hint().unwrap_or("") } else { "" };
        let footer = Rect { y: f.area().bottom().saturating_sub(2), height: 1, ..inner };
        f.render_widget(Paragraph::new(Line::from(hint).cyan()), footer);
    }

    fn draw_main(&mut self, f: &mut Frame, left: Rect, right: Rect, st: &StateView) {
        let st = st.clone();
        let focus_style = |on: bool| if on { Style::new().fg(Color::Cyan) } else { Style::new() };

        let sec_block = Block::bordered().title(" Stages ").border_style(focus_style(self.focus == Focus::Sections));
        let sec_inner = sec_block.inner(left);
        let sec_items: Vec<ListItem> = st
            .sections
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mark = match &s.enable {
                    Some(p) if value(&st, p).and_then(|v| v.as_bool()).unwrap_or(false) => "[x] ".green(),
                    Some(_) => "[ ] ".dark_gray(),
                    None => "    ".into(),
                };
                let row = Rect::new(sec_inner.x, sec_inner.y + i as u16, sec_inner.width, 1);
                let item = ListItem::new(Line::from(vec![mark, Span::raw(s.name.clone())]));
                if contains(self.mouse, row) { item.style(Style::new().bg(HOVER_BG)) } else { item }
            })
            .collect();
        let mut sec_state = ListState::default().with_selected(Some(self.sec));
        f.render_stateful_widget(
            List::new(sec_items).block(sec_block).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
            left,
            &mut sec_state,
        );
        for row in 0..sec_inner.height.min(st.sections.len() as u16) {
            let idx = sec_state.offset() + row as usize;
            let y = sec_inner.y + row;
            self.hits.push((Rect::new(sec_inner.x, y, sec_inner.width, 1), Hit::Stage { idx, checkbox: false }));
            if st.sections.get(idx).is_some_and(|s| s.enable.is_some()) {
                self.hits.push((Rect::new(sec_inner.x, y, 3, 1), Hit::Stage { idx, checkbox: true }));
            }
        }

        let items = self.items();
        let sec_name = st.sections.get(self.sec).map(|s| s.name.clone()).unwrap_or_default();
        let right = if sec_name == "Personal EQ" && right.height > 16 {
            let [list, plot] = Layout::vertical([Constraint::Min(6), Constraint::Length(10)]).areas(right);
            let block = Block::bordered().title(" Response (applies on every profile) ");
            let inner = block.inner(plot);
            f.render_widget(block, plot);
            f.render_widget(Paragraph::new(curve_lines(&st.personal, inner.width as usize, inner.height as usize - 1)), inner);
            list
        } else {
            right
        };
        let block = Block::bordered().title(format!(" {sec_name} ")).border_style(focus_style(self.focus == Focus::Params));
        let inner = block.inner(right);
        let width = inner.width as usize;
        // Hover uses last frame's scroll offset; it only shifts when the selection scrolls.
        let off = self.items_state.offset();
        let rows: Vec<ListItem> = items
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let hovered = i >= off && contains(self.mouse, Rect::new(inner.x, inner.y + (i - off) as u16, inner.width, 1));
                let item = ListItem::new(param_line(s, &st, width, hovered));
                if hovered { item.style(Style::new().bg(HOVER_BG)) } else { item }
            })
            .collect();
        self.items_state.select(if self.focus == Focus::Params { Some(self.item) } else { None });
        f.render_stateful_widget(
            List::new(rows).block(block).highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
            right,
            &mut self.items_state,
        );
        let off = self.items_state.offset();
        for row in 0..inner.height.min(items.len().saturating_sub(off) as u16) {
            let idx = off + row as usize;
            let y = inner.y + row;
            self.hits.push((Rect::new(inner.x, y, inner.width, 1), Hit::Param { idx, part: Part::Label }));
            for (x, w, part) in param_parts(&items[idx], &st, width) {
                self.hits.push((Rect::new(inner.x + x, y, w, 1), Hit::Param { idx, part }));
            }
        }
    }

    fn draw_picker(&mut self, f: &mut Frame, area: Rect) {
        let items = self.filtered();
        let title = format!(
            " A/B tuner: choose music · filter: {}_ · {} match{} ",
            self.filter,
            items.len(),
            if items.len() == 1 { "" } else { "es" }
        );
        let block = Block::bordered().title(title).title_bottom(
            Line::from(" ♫ bundled CC-licensed excerpts (credits: samples/CREDITS.md) · your own songs play their fullest 10 s ").dim(),
        );
        let inner = block.inner(area);
        let off = self.pick_offset;
        let rows: Vec<ListItem> = items
            .iter()
            .enumerate()
            .map(|(i, (l, _))| {
                let item = ListItem::new(l.clone());
                let hovered = i >= off && contains(self.mouse, Rect::new(inner.x, inner.y + (i - off) as u16, inner.width, 1));
                if hovered { item.style(Style::new().bg(HOVER_BG)) } else { item }
            })
            .collect();
        let mut ls = ListState::default().with_selected(Some(self.pick)).with_offset(self.pick_offset);
        f.render_stateful_widget(List::new(rows).block(block).highlight_style(Style::new().add_modifier(Modifier::REVERSED)), area, &mut ls);
        self.pick_offset = ls.offset();
        for row in 0..inner.height.min(items.len().saturating_sub(self.pick_offset) as u16) {
            self.hits.push((Rect::new(inner.x, inner.y + row, inner.width, 1), Hit::Pick(self.pick_offset + row as usize)));
        }
    }

    fn draw_tuner(&mut self, f: &mut Frame, area: Rect, st: &StateView) {
        let Some(t) = &st.tuner else { return };
        let block = Block::bordered().title(format!(" Blind A/B tuner · {} ({}) ", t.clip, t.segment));
        let inner = block.inner(area);
        f.render_widget(block, area);
        if t.done {
            self.draw_tuner_result(f, inner, t);
            return;
        }
        let side_style = |i: usize| {
            if t.listening == i { Style::new().fg(Color::Black).bg(Color::Cyan).bold() } else { Style::new().fg(Color::DarkGray) }
        };
        let side_text = |i: usize, name: &str| format!("  {} {name}  ", if t.listening == i { "▶" } else { " " });
        let mut rb = RowBuilder::new(inner.x, inner.y + 1, self.mouse);
        rb.text("   Now hearing:  ", Style::new())
            .button(side_text(0, "A"), side_style(0), Hit::Listen(0))
            .text("   ", Style::new())
            .button(side_text(1, "B"), side_style(1), Hit::Listen(1));
        let hearing = rb.finish(&mut self.hits);

        let w = inner.width.saturating_sub(24).clamp(10, 50) as usize;
        let done = if t.total > 0 { (t.trial * w / t.total.max(1)).min(w) } else { 0 };
        let mut rb = RowBuilder::new(inner.x, inner.y + 6, self.mouse);
        rb.text("     ", Style::new())
            .button(" 1 ", Style::new().reversed(), Hit::Answer(Choice::A))
            .button(" A sounds better ", Style::new(), Hit::Answer(Choice::A))
            .text("   ", Style::new())
            .button(" 2 ", Style::new().reversed(), Hit::Answer(Choice::B))
            .button(" B sounds better ", Style::new(), Hit::Answer(Choice::B))
            .text("   ", Style::new())
            .button(" 0 ", Style::new().reversed(), Hit::Answer(Choice::Same))
            .button(" can't tell ", Style::new(), Hit::Answer(Choice::Same));
        let answers = rb.finish(&mut self.hits);
        let mut rb = RowBuilder::new(inner.x, inner.y + 8, self.mouse);
        rb.text("   Auto-switch (x): ", Style::new()).button(
            t.auto_secs.map_or(" off ".to_string(), |s| format!(" every {s:.0} s ")),
            Style::new().fg(Color::Cyan),
            Hit::AutoSwitch,
        );
        let auto = rb.finish(&mut self.hits);

        let mut lines = vec![
            Line::from(""),
            hearing,
            Line::from(""),
            Line::from(vec![
                Span::raw(format!("   Trial {:>2} of ~{:<3} ", t.trial + 1, t.total.max(t.trial + 1))),
                Span::styled("█".repeat(done), Style::new().fg(Color::Cyan)),
                Span::styled("░".repeat(w - done), Style::new().dim()),
            ]),
            Line::from(""),
            Line::from("   Listen to both (click A / B, or a / b / space), then answer:"),
            answers,
            Line::from(""),
            auto,
            Line::from(""),
        ];
        for tip in [
            "   Both sides are loudness-matched, and which one is the change is random.",
            "   Pause other audio while tuning. Esc ends early and keeps what you chose so far.",
        ] {
            lines.push(Line::from(tip).dim());
        }
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_tuner_result(&mut self, f: &mut Frame, area: Rect, t: &TunerView) {
        let Some(eq) = &t.result else { return };
        let [text, plot] = Layout::vertical([Constraint::Length(PE_BANDS as u16 + 5), Constraint::Min(4)]).areas(area);
        let mut lines = vec![Line::from(format!("   Done after {} answers. Your EQ (now playing):", t.trial)).bold()];
        if t.low_confidence {
            lines.push(Line::from("   ⚠ Some repeated questions got different answers; the result may be noisy.").yellow());
        }
        for (i, b) in eq.bands.iter().enumerate() {
            lines.push(Line::from(format!("     {:<13} {:>+5.1} dB @ {:>6.0} Hz", PE_NAMES[i], b.gain_db, b.freq)));
        }
        lines.push(Line::from(""));
        let mut rb = RowBuilder::new(text.x, text.y + lines.len() as u16, self.mouse);
        rb.text("   ", Style::new())
            .button(" k ", Style::new().reversed(), Hit::Keep(true))
            .button(" keep as personal EQ ", Style::new(), Hit::Keep(true))
            .text("   ", Style::new())
            .button(" d ", Style::new().reversed(), Hit::Keep(false))
            .button(" discard ", Style::new(), Hit::Keep(false));
        lines.push(rb.finish(&mut self.hits));
        f.render_widget(Paragraph::new(lines), text);
        let plot_lines = curve_lines(eq, plot.width.saturating_sub(4) as usize, plot.height.saturating_sub(1) as usize);
        f.render_widget(Paragraph::new(plot_lines).block(Block::new().padding(ratatui::widgets::Padding::horizontal(2))), plot);
    }

    /// Vertical VU meters: pre (input) and post (output), L/R, with peak hold.
    fn draw_levels(&mut self, f: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" Levels ");
        let inner = block.inner(area);
        f.render_widget(block, area);
        let h = inner.height.saturating_sub(2) as usize;
        if h == 0 {
            return;
        }
        // Bar columns: 3-wide scale, a space, then L R, a gap, L R.
        const BAR_X: [u16; 4] = [4, 8, 14, 18];
        const NAMES: [&str; 4] = ["in L", "in R", "out L", "out R"];
        let hovered: Option<usize> = (0..4).find(|&k| contains(self.mouse, Rect::new(inner.x + BAR_X[k], inner.y, 3, inner.height)));
        for k in 0..4 {
            self.hits.push((Rect::new(inner.x + BAR_X[k], inner.y, 3, inner.height), Hit::Level(k)));
        }
        const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
        let rows_of = |db: f32| ((db - VU_FLOOR) / -VU_FLOOR).clamp(0.0, 1.0) * h as f32;
        let row_db = |j: usize| VU_FLOOR * (1.0 - (j as f32 + 0.5) / h as f32);
        let color = |db: f32| if db > -3.0 { Color::Red } else if db > -12.0 { Color::Yellow } else { Color::Green };
        // Scale labels at a few fixed levels.
        let label_at = |j: usize| -> String {
            for mark in [0.0f32, -6.0, -12.0, -24.0, -36.0, -48.0] {
                if (rows_of(mark).ceil() as usize).saturating_sub(1) == j {
                    return format!("{:>3}", mark as i32);
                }
            }
            "   ".into()
        };
        let mut lines = vec![];
        for row in 0..h {
            let j = h - 1 - row; // 0 = bottom
            let mut spans = vec![Span::raw(label_at(j)).dim(), Span::raw(" ")];
            for k in 0..4 {
                if k == 2 {
                    spans.push(Span::raw("   "));
                }
                let fill = (rows_of(self.meters.vu_db[k]) - j as f32).clamp(0.0, 1.0);
                let hold_row = (rows_of(held(self.hold[k])).ceil() as usize).saturating_sub(1);
                let (txt, mut style) = if fill > 0.0 {
                    (EIGHTHS[(fill * 8.0).round() as usize].repeat(3), Style::new().fg(color(row_db(j))))
                } else if j == hold_row && held(self.hold[k]) > VU_FLOOR {
                    ("▔▔▔".to_string(), Style::new().fg(color(held(self.hold[k]))))
                } else {
                    ("   ".to_string(), Style::new())
                };
                if hovered == Some(k) {
                    style = style.bg(HOVER_BG);
                }
                spans.push(Span::styled(txt, style));
                if k % 2 == 0 {
                    spans.push(Span::raw(" "));
                }
            }
            lines.push(Line::from(spans));
        }
        let pk = |a: usize, b: usize| held(self.hold[a]).max(held(self.hold[b])).max(VU_FLOOR);
        lines.push(Line::from("     L   R     L   R").dim());
        lines.push(match hovered {
            // Exact readout for the bar under the pointer: RMS / held peak.
            Some(k) => Line::from(vec![
                Span::raw(format!(" {:<5}", NAMES[k])),
                Span::styled(format!("{:>5.1}", self.meters.vu_db[k].max(VU_FLOOR)), Style::new().fg(color(self.meters.vu_db[k]))),
                Span::raw(" /").dim(),
                Span::styled(format!("{:>5.1}", held(self.hold[k]).max(VU_FLOOR)), Style::new().fg(color(held(self.hold[k])))),
            ]),
            None => Line::from(vec![
                Span::raw(" in "),
                Span::styled(format!("{:>5.1}", pk(0, 1)), Style::new().fg(color(pk(0, 1)))),
                Span::raw(" out "),
                Span::styled(format!("{:>5.1}", pk(2, 3)), Style::new().fg(color(pk(2, 3)))),
            ]),
        });
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_meters(&mut self, f: &mut Frame, area: Rect, st: &StateView) {
        let m = self.meters;
        let n = st.band_freqs.len().max(1);
        let inner = Block::new().borders(Borders::ALL).inner(area);
        let colw = (inner.width as usize / n).max(3);
        let hz = |hz: f32| {
            if hz >= 9500.0 { format!("{:.0}k", hz / 1000.0) } else if hz >= 1000.0 { format!("{:.1}k", hz / 1000.0) } else { format!("{hz:.0}") }
        };
        let mut hovered = None;
        for b in 0..n {
            let r = Rect::new(inner.x + (b * colw) as u16, inner.y, colw as u16, inner.height);
            if contains(self.mouse, r) {
                hovered = Some(b);
            }
            self.hits.push((r, Hit::Band(b)));
        }
        let title = match hovered {
            // Exact numbers for the band under the pointer.
            Some(b) => format!(
                " {}Hz · gain reduction {:.1} dB · IEQ {:+.1} dB · level {:.1} dBFS ",
                if st.band_freqs[b] >= 1000.0 { format!("{:.1} k", st.band_freqs[b] / 1000.0) } else { format!("{:.0} ", st.band_freqs[b]) },
                m.gr_db[b],
                m.ieq_db[b],
                m.level_db[b].max(-120.0)
            ),
            None => format!(
                " Dynamics gain reduction per band · in {:>6.1} dBFS · out {:>6.1} dBFS · limiter {:>5.1} dB ",
                m.in_peak_db, m.out_peak_db, m.limiter_gr_db
            ),
        };
        let block = Block::new().borders(Borders::ALL).title(if hovered.is_some() { Line::from(title).cyan() } else { Line::from(title) });
        f.render_widget(block, area);
        let bar_rows = inner.height.saturating_sub(2) as usize;
        let scale = 24.0 / bar_rows.max(1) as f32; // dB per row
        let hl = |b: usize, s: Style| if hovered == Some(b) { s.bg(HOVER_BG) } else { s };
        let mut lines = vec![];
        for row in 0..bar_rows {
            let spans: Vec<Span> = (0..n)
                .map(|b| {
                    let gr = -m.gr_db.get(b).copied().unwrap_or(0.0);
                    let lvl = gr / scale - row as f32;
                    let ch = if lvl >= 1.0 {
                        "█"
                    } else if lvl > 0.5 {
                        "▀"
                    } else {
                        " "
                    };
                    let color = if gr > 12.0 { Color::Red } else if gr > 6.0 { Color::Yellow } else { Color::Green };
                    Span::styled(format!("{:^w$}", ch.repeat(colw.saturating_sub(1).min(3)), w = colw), hl(b, Style::new().fg(color)))
                })
                .collect();
            lines.push(Line::from(spans));
        }
        let ieq: Vec<Span> = (0..n)
            .map(|b| {
                let v = m.ieq_db.get(b).copied().unwrap_or(0.0);
                let s = if v.abs() < 0.05 { "·".to_string() } else { format!("{v:+.0}") };
                Span::styled(format!("{s:^colw$}"), hl(b, Style::new().fg(if v >= 0.0 { Color::Cyan } else { Color::Magenta })))
            })
            .collect();
        lines.push(Line::from(ieq));
        let labels: Vec<Span> = st
            .band_freqs
            .iter()
            .enumerate()
            .map(|(b, &f)| {
                let s = hz(f);
                let style = if hovered == Some(b) { Style::new().bg(HOVER_BG).bold() } else { Style::new().dim() };
                Span::styled(format!("{s:^colw$}"), style)
            })
            .collect();
        lines.push(Line::from(labels));
        f.render_widget(Paragraph::new(lines), inner);
    }
}

/// Label, value text, slider width and knob position of a float row.
fn float_layout(s: &Spec, v: f32, width: usize) -> (String, String, usize, usize) {
    let Kind::Float { min, max, unit, step } = &s.kind else { unreachable!() };
    let label = format!(" {:<20}", s.label);
    let prec = if *step >= 1.0 { 0 } else if *step >= 0.1 { 1 } else { 2 };
    let val = format!("{v:>8.prec$} {unit:<3}");
    let slider_w = width.saturating_sub(label.chars().count() + val.chars().count() + 3).clamp(4, 40);
    let pos = (((v - min) / (max - min)).clamp(0.0, 1.0) * (slider_w - 1) as f32).round() as usize;
    (label, val, slider_w, pos)
}

/// Clickable sub-areas of a parameter row as (x offset, width, part).
fn param_parts(s: &Spec, st: &StateView, width: usize) -> Vec<(u16, u16, Part)> {
    let label_w = format!(" {:<20}", s.label).chars().count() as u16;
    match &s.kind {
        Kind::Action(_) => vec![(1, s.label.chars().count() as u16 + 4, Part::Action)],
        Kind::Bool => vec![(label_w, 7, Part::Toggle)],
        Kind::Choice(opts) => {
            let mut x = label_w;
            opts.iter()
                .enumerate()
                .map(|(i, o)| {
                    let w = o.chars().count() as u16 + 2;
                    x += w;
                    (x - w, w, Part::Choice(i))
                })
                .collect()
        }
        Kind::Float { .. } => {
            let v = value(st, &s.path).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            let (label, val, slider_w, _) = float_layout(s, v, width);
            vec![((label.chars().count() + val.chars().count() + 1) as u16, slider_w as u16, Part::Slider)]
        }
    }
}

fn param_line(s: &Spec, st: &StateView, width: usize, hovered: bool) -> Line<'static> {
    let v = value(st, &s.path).unwrap_or(serde_json::Value::Null);
    let label = format!(" {:<20}", s.label);
    match &s.kind {
        Kind::Action(_) => {
            let style = Style::new().fg(Color::Black).bg(Color::Cyan).bold();
            Line::from(vec![
                Span::raw(" "),
                Span::styled(format!(" ▶ {} ", s.label), if hovered { hover(style) } else { style }),
                Span::raw("  press Enter or click").dim(),
            ])
        }
        Kind::Bool => {
            let on = v.as_bool().unwrap_or(false);
            Line::from(vec![Span::raw(label), if on { "[x] on".green() } else { "[ ] off".dark_gray() }])
        }
        Kind::Choice(opts) => {
            let cur = v.as_str().unwrap_or("");
            let mut spans = vec![Span::raw(label)];
            for o in opts {
                spans.push(if o == cur { Span::styled(format!(" {o} "), Style::new().reversed()) } else { Span::raw(format!(" {o} ")) });
            }
            Line::from(spans)
        }
        Kind::Float { .. } => {
            let x = v.as_f64().unwrap_or(0.0) as f32;
            let (label, val, slider_w, pos) = float_layout(s, x, width);
            let slider: String = (0..slider_w).map(|i| if i == pos { '●' } else if i < pos { '━' } else { '─' }).collect();
            let color = if hovered { Style::new().fg(Color::LightCyan).bold() } else { Style::new().fg(Color::Cyan) };
            Line::from(vec![Span::raw(label), Span::raw(val), Span::raw(" "), Span::styled(slider, color)])
        }
    }
}

fn start_daemon() {
    let unit_ok = std::process::Command::new("systemctl")
        .args(["--user", "start", "neiro.service"])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if unit_ok {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        use std::os::unix::process::CommandExt;
        let _ = std::process::Command::new(exe)
            .arg("daemon")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn fake_state(tuner: Option<TunerView>) -> StateView {
        let t = crate::tuning::parse(crate::tuning::EMBEDDED_XML).unwrap();
        let params = crate::params::factory(&t, "music").unwrap();
        let mut personal = PersonalEq::default();
        personal.bands[0].gain_db = 4.5;
        personal.bands[3].gain_db = -2.0;
        StateView {
            profile: "music".into(),
            profiles: crate::params::PROFILES.iter().map(|s| s.to_string()).collect(),
            bypass: false,
            dirty: false,
            customised: false,
            sections: crate::params::sections(&params, &t.band_freqs),
            params,
            band_freqs: t.band_freqs.to_vec(),
            latency_ms: 22.3,
            target: "alsa_output.speaker".into(),
            hw_comp_db: 0.0,
            pinned: true,
            personal,
            tuner,
        }
    }

    fn render(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(120, 36)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..36).map(|y| (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn screens_render() {
        let mut app = App::new();
        app.state = Some(fake_state(None));
        let main = render(&mut app);
        println!("{main}");
        assert!(main.contains("Personal EQ") && main.contains("Response (applies on every profile)"));
        assert!(main.contains("q quit"), "footer must fit in 120 columns");
        assert!(main.contains("▶ Find my EQ: blind A/B test") && !main.contains("t A/B tuner"));
        // Enter on the button (first item of Personal EQ) opens the picker; 't' does nothing.
        app.key(KeyCode::Char('t'), KeyModifiers::NONE);
        assert!(app.screen == Screen::Main);
        app.focus = Focus::Params;
        app.item = 0;
        app.key(KeyCode::Right, KeyModifiers::NONE);
        assert!(app.screen == Screen::Main, "arrows must not trigger the button");
        app.key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.screen == Screen::Pick);
        app.screen = Screen::Main;

        app.files = vec![music_dir().join("Album/01. Song.flac")];
        app.screen = Screen::Pick;
        let pick = render(&mut app);
        println!("{pick}");
        assert!(pick.contains("Night Owl") && pick.contains("Like Tetanus In A Wound") && pick.contains("Aces High"));
        assert!(!pick.contains("Synth loop"), "synth loops are only a fallback");
        app.ffmpeg_ok = false;
        assert!(render(&mut app).contains("Synth loop: Electronic"));
        app.ffmpeg_ok = true;
        assert!(pick.contains("Album/01. Song.flac"));

        let mut view = TunerView {
            clip: "Song".into(),
            segment: "1:02–1:12".into(),
            listening: 1,
            auto_secs: None,
            trial: 6,
            total: 24,
            done: false,
            low_confidence: false,
            result: None,
        };
        app.state = Some(fake_state(Some(view.clone())));
        app.screen = Screen::Tuner;
        let tuner = render(&mut app);
        println!("{tuner}");
        assert!(tuner.contains("▶ B") && tuner.contains("Trial  7 of ~24"));
        assert!(!tuner.contains("dB @"), "must stay blind during trials");

        view.done = true;
        let mut res = PersonalEq::default();
        res.bands[0].gain_db = 6.0;
        view.result = Some(res);
        app.state = Some(fake_state(Some(view)));
        let done = render(&mut app);
        println!("{done}");
        assert!(done.contains("+6.0 dB @    120 Hz") && done.contains("keep as personal EQ"));
    }

    #[test]
    fn not_running_screen_shows_banner() {
        let mut app = App::new();
        let out = render(&mut app);
        println!("{out}");
        for l in BANNER {
            assert!(out.contains(l.trim()), "banner line missing: {l}");
        }
        // Wide characters occupy two cells in the test buffer, so check each kanji.
        assert!(out.contains("音") && out.contains("色") && out.contains("not running"));
    }

    fn mouse_at(app: &mut App, kind: MouseEventKind, (x, y): (u16, u16)) {
        app.mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
    }

    fn rect_of(app: &App, hit: &Hit) -> Rect {
        app.hits.iter().find(|(_, h)| h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no hit area for {hit:?}"))
    }

    fn last_set(app: &App) -> Option<(String, serde_json::Value)> {
        app.sent.iter().rev().find_map(|r| match r {
            Request::Set { path, value } => Some((path.clone(), value.clone())),
            _ => None,
        })
    }

    #[test]
    fn mouse_hover_and_click_main_screen() {
        let mut app = App::new();
        app.state = Some(fake_state(None));
        render(&mut app);

        // Hovering a stage row shows its hint; clicking its checkbox toggles the stage.
        let widener = app.state.as_ref().unwrap().sections.iter().position(|s| s.name == "Widener").unwrap();
        let row = rect_of(&app, &Hit::Stage { idx: widener, checkbox: false });
        mouse_at(&mut app, MouseEventKind::Moved, (row.x + 8, row.y));
        assert!(render(&mut app).contains("click: open this stage"));
        let cb = rect_of(&app, &Hit::Stage { idx: widener, checkbox: true });
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (cb.x + 1, cb.y));
        assert_eq!(app.sec, widener);
        assert_eq!(last_set(&app), Some(("/widener/enable".into(), serde_json::json!(true))));

        // Back to Personal EQ: click and drag the bass shelf slider, then wheel it.
        let pe = Hit::Stage { idx: 0, checkbox: false };
        let r = rect_of(&app, &pe);
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (r.x + 8, r.y));
        mouse_at(&mut app, MouseEventKind::Up(MouseButton::Left), (r.x + 8, r.y));
        render(&mut app);
        let bass = app.items().iter().position(|s| s.path == "/personal/bands/0/gain_db").unwrap();
        let slider = rect_of(&app, &Hit::Param { idx: bass, part: Part::Slider });
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (slider.x + slider.width - 1, slider.y));
        assert_eq!(last_set(&app), Some(("/personal/bands/0/gain_db".into(), serde_json::json!(12.0))));
        mouse_at(&mut app, MouseEventKind::Drag(MouseButton::Left), (slider.x, slider.y + 3));
        assert_eq!(last_set(&app), Some(("/personal/bands/0/gain_db".into(), serde_json::json!(-12.0))));
        mouse_at(&mut app, MouseEventKind::Up(MouseButton::Left), (slider.x, slider.y));
        assert!(app.drag.is_none());
        mouse_at(&mut app, MouseEventKind::ScrollUp, (slider.x + 2, slider.y));
        // fake_state has bass at +4.5 dB; one wheel notch is one 0.5 dB step.
        assert_eq!(last_set(&app), Some(("/personal/bands/0/gain_db".into(), serde_json::json!(5.0))));

        // Hovering the slider brightens it and explains drag/wheel.
        mouse_at(&mut app, MouseEventKind::Moved, (slider.x + 1, slider.y));
        assert!(render(&mut app).contains("click or drag: set value"));

        // The tuner button opens the picker.
        let button = rect_of(&app, &Hit::Param { idx: 0, part: Part::Action });
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (button.x + 2, button.y));
        assert!(app.screen == Screen::Pick);
    }

    #[test]
    fn mouse_tabs_power_and_meters() {
        let mut app = App::new();
        app.state = Some(fake_state(None));
        render(&mut app);
        let tab = rect_of(&app, &Hit::Tab(0));
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (tab.x + 1, tab.y));
        assert!(matches!(app.sent.last(), Some(Request::SetProfile { profile }) if profile == "dynamic"));
        let power = rect_of(&app, &Hit::Power);
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (power.x, power.y));
        assert!(matches!(app.sent.last(), Some(Request::Bypass { on: None })));

        // Hovering a band shows its exact numbers in the meter title.
        app.meters.gr_db[4] = -6.5;
        let band = rect_of(&app, &Hit::Band(4));
        mouse_at(&mut app, MouseEventKind::Moved, (band.x + 1, band.y + 2));
        let out = render(&mut app);
        assert!(out.contains("469 Hz · gain reduction -6.5 dB"), "{out}");
        // Hovering a level bar shows its readout.
        app.meters.vu_db = [-20.0, -21.0, -12.5, -13.0];
        let lvl = rect_of(&app, &Hit::Level(2));
        mouse_at(&mut app, MouseEventKind::Moved, (lvl.x + 1, lvl.y + 1));
        assert!(render(&mut app).contains("out L-12.5"));
    }

    #[test]
    fn mouse_picker_and_tuner() {
        let mut app = App::new();
        app.state = Some(fake_state(None));
        app.screen = Screen::Pick;
        render(&mut app);
        let second = rect_of(&app, &Hit::Pick(1));
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (second.x + 3, second.y));
        assert_eq!(app.pick, 1);
        assert!(app.sent.is_empty(), "first click only selects");
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (second.x + 3, second.y));
        assert!(matches!(app.sent.last(), Some(Request::TunerStart { source: TunerSource::Sample { .. } })));

        let view = TunerView {
            clip: "Song".into(),
            segment: "1:02–1:12".into(),
            listening: 0,
            auto_secs: None,
            trial: 3,
            total: 24,
            done: false,
            low_confidence: false,
            result: None,
        };
        app.state = Some(fake_state(Some(view)));
        app.screen = Screen::Tuner;
        // Flash messages (here "preparing…") outrank hover hints for a few seconds.
        app.msg = None;
        render(&mut app);
        let b = rect_of(&app, &Hit::Listen(1));
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (b.x + 2, b.y));
        assert!(matches!(app.sent.last(), Some(Request::TunerListen { side: 1 })));
        let ans = app.hits.iter().rev().find(|(_, h)| *h == Hit::Answer(Choice::B)).unwrap().0;
        mouse_at(&mut app, MouseEventKind::Moved, (ans.x + 1, ans.y));
        assert!(render(&mut app).contains("click: this side sounds better"));
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (ans.x + 1, ans.y));
        assert!(matches!(app.sent.last(), Some(Request::TunerAnswer { choice: Choice::B })));
        let auto = rect_of(&app, &Hit::AutoSwitch);
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), (auto.x + 1, auto.y));
        assert!(matches!(app.sent.last(), Some(Request::TunerAutoSwitch { secs: Some(_) })));
    }

    #[test]
    fn levels_panel_renders() {
        let mut app = App::new();
        app.meters.vu_db = [-6.0, -20.0, -2.0, -45.0];
        app.hold = [(-1.0, Instant::now()), (-12.0, Instant::now()), (-0.5, Instant::now()), (-30.0, Instant::now())];
        let mut term = Terminal::new(TestBackend::new(24, 20)).unwrap();
        term.draw(|f| app.draw_levels(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text: Vec<String> = (0..20)
            .map(|y| (0..24).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect();
        println!("{}", text.join("\n"));
        let all = text.join("\n");
        assert!(all.contains("█") && all.contains("▔") && all.contains(" in"));
    }
}
