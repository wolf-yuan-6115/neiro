//! `neiro tui`: ratatui front-end for the daemon.

use crate::dsp::chain::MeterSnapshot;
use crate::ipc::{Client, Request, StateView, TunerSource, TunerView};
use crate::params::{Kind, PersonalEq, Spec, ACTION_TUNER, PERSONAL_PREFIX, PE_BANDS, PE_NAMES};
use crate::demo::Demo;
use crate::samples::SAMPLES;
use crate::tuner::Choice;
use std::path::PathBuf;
use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
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
    let mut app = App::new();
    let res = app.main_loop(&mut terminal);
    // Don't leave the A/B clip looping after the UI is gone.
    if app.state.as_ref().is_some_and(|s| s.tuner.is_some()) {
        app.request(Request::TunerStop { keep: false });
    }
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
            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind != KeyEventKind::Release && !self.key(k.code, k.modifiers) {
                        return Ok(());
                    }
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
            KeyCode::Enter => {
                let Some((label, source)) = self.filtered().into_iter().nth(self.pick) else { return };
                self.flash(format!("preparing {label}…"));
                self.request(Request::TunerStart { source });
            }
            _ => {}
        }
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
            KeyCode::Char('x') => {
                let secs = if t.auto_secs.is_some() { None } else { Some(AUTO_SWITCH_SECS) };
                self.request(Request::TunerAutoSwitch { secs });
            }
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
            Kind::Float { min, max, step, .. } => {
                let v = cur.as_f64().unwrap_or(0.0) as f32;
                let mult = if coarse { 10.0 } else { 1.0 };
                let nv = ((v + dir * step * mult) / step).round() * step;
                let nv = nv.clamp(*min, *max);
                if cur.is_u64() {
                    serde_json::json!(nv.round() as u64)
                } else {
                    serde_json::json!((nv * 1000.0).round() / 1000.0)
                }
            }
        };
        self.request(Request::Set { path: spec.path, value });
    }

    fn draw(&mut self, f: &mut Frame) {
        let [header, body, meters, footer] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(8), Constraint::Length(9), Constraint::Length(1)])
                .areas(f.area());
        let Some(st) = self.state.clone() else {
            let block = Block::bordered().title(" neiro ");
            let inner = block.inner(f.area());
            f.render_widget(block, f.area());
            let mut lines = banner_lines();
            lines.extend([
                Line::from(""),
                Line::from("音色").dim(),
                Line::from(""),
                Line::from(""),
                Line::from("The neiro daemon is not running.".bold()),
                Line::from(""),
                Line::from("Press d to start it (systemd unit if installed, else a detached process), q to quit."),
                Line::from("Install permanently with: neiro install").dim(),
            ]);
            let top = inner.height.saturating_sub(lines.len() as u16) / 2;
            let area = Rect { y: inner.y + top, height: inner.height - top, ..inner };
            f.render_widget(Paragraph::new(lines).centered(), area);
            return;
        };

        // Header: power state + profile tabs.
        let power = if st.bypass {
            Span::styled(" BYPASS ", Style::new().bg(Color::DarkGray).fg(Color::White).bold())
        } else {
            Span::styled(" ● ON ", Style::new().bg(Color::Green).fg(Color::Black).bold())
        };
        let titles: Vec<Line> = st.profiles.iter().enumerate().map(|(i, p)| Line::from(format!("{} {}", i + 1, p))).collect();
        let sel = st.profiles.iter().position(|p| *p == st.profile).unwrap_or(0);
        let mut title = vec![Span::raw(" neiro "), power];
        if st.dirty {
            title.push(Span::styled(" unsaved ", Style::new().fg(Color::Yellow)));
        } else if st.customised {
            title.push(Span::styled(" custom ", Style::new().fg(Color::Cyan)));
        }
        let tabs = Tabs::new(titles)
            .select(sel)
            .highlight_style(Style::new().fg(Color::Black).bg(Color::Cyan).bold())
            .block(Block::bordered().title(Line::from(title)).title_bottom(
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

        let help = match &self.msg {
            Some((m, t)) if t.elapsed() < Duration::from_secs(4) => Line::from(m.clone().yellow()),
            _ => Line::from(match self.screen {
                Screen::Main => " space on/off · 1-6 profile · tab focus · ↑↓ select · ←→ adjust · e toggle · s save · R reset · q quit",
                Screen::Pick => " type to filter · ↑↓ select · enter start · esc back",
                Screen::Tuner => " a/b or ←/→ listen · space flip · 1 prefer A · 2 prefer B · 0 no difference · x auto-switch · esc finish · q quit",
            })
            .dim(),
        };
        f.render_widget(Paragraph::new(help), footer);
    }

    fn draw_main(&mut self, f: &mut Frame, left: Rect, right: Rect, st: &StateView) {
        let st = st.clone();
        let sec_items: Vec<ListItem> = st
            .sections
            .iter()
            .map(|s| {
                let mark = match &s.enable {
                    Some(p) if value(&st, p).and_then(|v| v.as_bool()).unwrap_or(false) => "[x] ".green(),
                    Some(_) => "[ ] ".dark_gray(),
                    None => "    ".into(),
                };
                ListItem::new(Line::from(vec![mark, Span::raw(s.name.clone())]))
            })
            .collect();
        let focus_style = |on: bool| if on { Style::new().fg(Color::Cyan) } else { Style::new() };
        let mut sec_state = ListState::default().with_selected(Some(self.sec));
        f.render_stateful_widget(
            List::new(sec_items)
                .block(Block::bordered().title(" Stages ").border_style(focus_style(self.focus == Focus::Sections)))
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
            left,
            &mut sec_state,
        );

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
        let width = right.width.saturating_sub(2) as usize;
        let rows: Vec<ListItem> = items.iter().map(|s| ListItem::new(param_line(s, &st, width))).collect();
        self.items_state.select(if self.focus == Focus::Params { Some(self.item) } else { None });
        f.render_stateful_widget(
            List::new(rows)
                .block(Block::bordered().title(format!(" {sec_name} ")).border_style(focus_style(self.focus == Focus::Params)))
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
            right,
            &mut self.items_state,
        );
    }

    fn draw_picker(&self, f: &mut Frame, area: Rect) {
        let items = self.filtered();
        let rows: Vec<ListItem> = items.iter().map(|(l, _)| ListItem::new(l.clone())).collect();
        let title = format!(
            " A/B tuner: choose music · filter: {}_ · {} match{} ",
            self.filter,
            items.len(),
            if items.len() == 1 { "" } else { "es" }
        );
        let mut ls = ListState::default().with_selected(Some(self.pick));
        f.render_stateful_widget(
            List::new(rows)
                .block(Block::bordered().title(title).title_bottom(
                    Line::from(" ♫ bundled CC-licensed excerpts (credits: samples/CREDITS.md) · your own songs play their fullest 10 s ").dim(),
                ))
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
            area,
            &mut ls,
        );
    }

    fn draw_tuner(&self, f: &mut Frame, area: Rect, st: &StateView) {
        let Some(t) = &st.tuner else { return };
        let block = Block::bordered().title(format!(" Blind A/B tuner · {} ({}) ", t.clip, t.segment));
        let inner = block.inner(area);
        f.render_widget(block, area);
        if t.done {
            self.draw_tuner_result(f, inner, t);
            return;
        }
        let side = |i: usize, name: &str| {
            let on = t.listening == i;
            let style = if on { Style::new().fg(Color::Black).bg(Color::Cyan).bold() } else { Style::new().fg(Color::DarkGray) };
            Span::styled(format!("  {} {name}  ", if on { "▶" } else { " " }), style)
        };
        let w = inner.width.saturating_sub(24).clamp(10, 50) as usize;
        let done = if t.total > 0 { (t.trial * w / t.total.max(1)).min(w) } else { 0 };
        let mut lines = vec![
            Line::from(""),
            Line::from(vec![Span::raw("   Now hearing:  "), side(0, "A"), Span::raw("   "), side(1, "B")]),
            Line::from(""),
            Line::from(vec![
                Span::raw(format!("   Trial {:>2} of ~{:<3} ", t.trial + 1, t.total.max(t.trial + 1))),
                Span::styled("█".repeat(done), Style::new().fg(Color::Cyan)),
                Span::styled("░".repeat(w - done), Style::new().dim()),
            ]),
            Line::from(""),
            Line::from("   Listen to both (a / b, or space to flip), then answer:"),
            Line::from(vec![
                Span::raw("     "),
                Span::styled(" 1 ", Style::new().reversed()),
                Span::raw(" A sounds better    "),
                Span::styled(" 2 ", Style::new().reversed()),
                Span::raw(" B sounds better    "),
                Span::styled(" 0 ", Style::new().reversed()),
                Span::raw(" can't tell"),
            ]),
            Line::from(""),
            Line::from(format!(
                "   Auto-switch (x): {}",
                t.auto_secs.map_or("off".to_string(), |s| format!("every {s:.0} s"))
            )),
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

    fn draw_tuner_result(&self, f: &mut Frame, area: Rect, t: &TunerView) {
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
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(" k ", Style::new().reversed()),
            Span::raw(" keep as personal EQ    "),
            Span::styled(" d ", Style::new().reversed()),
            Span::raw(" discard"),
        ]));
        f.render_widget(Paragraph::new(lines), text);
        let plot_lines = curve_lines(eq, plot.width.saturating_sub(4) as usize, plot.height.saturating_sub(1) as usize);
        f.render_widget(Paragraph::new(plot_lines).block(Block::new().padding(ratatui::widgets::Padding::horizontal(2))), plot);
    }

    /// Vertical VU meters: pre (input) and post (output), L/R, with peak hold.
    fn draw_levels(&self, f: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" Levels ");
        let inner = block.inner(area);
        f.render_widget(block, area);
        let h = inner.height.saturating_sub(2) as usize;
        if h == 0 {
            return;
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
                let (txt, style) = if fill > 0.0 {
                    (EIGHTHS[(fill * 8.0).round() as usize].repeat(3), Style::new().fg(color(row_db(j))))
                } else if j == hold_row && held(self.hold[k]) > VU_FLOOR {
                    ("▔▔▔".to_string(), Style::new().fg(color(held(self.hold[k]))))
                } else {
                    ("   ".to_string(), Style::new())
                };
                spans.push(Span::styled(txt, style));
                if k % 2 == 0 {
                    spans.push(Span::raw(" "));
                }
            }
            lines.push(Line::from(spans));
        }
        let pk = |a: usize, b: usize| held(self.hold[a]).max(held(self.hold[b])).max(VU_FLOOR);
        lines.push(Line::from("     L   R     L   R").dim());
        lines.push(Line::from(vec![
            Span::raw(" in "),
            Span::styled(format!("{:>5.1}", pk(0, 1)), Style::new().fg(color(pk(0, 1)))),
            Span::raw(" out "),
            Span::styled(format!("{:>5.1}", pk(2, 3)), Style::new().fg(color(pk(2, 3)))),
        ]));
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_meters(&self, f: &mut Frame, area: Rect, st: &StateView) {
        let m = &self.meters;
        let title = format!(
            " Dynamics gain reduction per band · in {:>6.1} dBFS · out {:>6.1} dBFS · limiter {:>5.1} dB ",
            m.in_peak_db, m.out_peak_db, m.limiter_gr_db
        );
        let block = Block::new().borders(Borders::ALL).title(title);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let n = st.band_freqs.len().max(1);
        let colw = (inner.width as usize / n).max(3);
        let bar_rows = inner.height.saturating_sub(2) as usize;
        let scale = 24.0 / bar_rows.max(1) as f32; // dB per row
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
                    Span::styled(format!("{:^w$}", ch.repeat(colw.saturating_sub(1).min(3)), w = colw), Style::new().fg(color))
                })
                .collect();
            lines.push(Line::from(spans));
        }
        let ieq: Vec<Span> = (0..n)
            .map(|b| {
                let v = m.ieq_db.get(b).copied().unwrap_or(0.0);
                let s = if v.abs() < 0.05 { "·".to_string() } else { format!("{v:+.0}") };
                Span::styled(format!("{s:^colw$}"), Style::new().fg(if v >= 0.0 { Color::Cyan } else { Color::Magenta }))
            })
            .collect();
        lines.push(Line::from(ieq));
        let labels: Vec<Span> = st
            .band_freqs
            .iter()
            .map(|&hz| {
                let s = if hz >= 9500.0 { format!("{:.0}k", hz / 1000.0) } else if hz >= 1000.0 { format!("{:.1}k", hz / 1000.0) } else { format!("{hz:.0}") };
                Span::raw(format!("{s:^colw$}")).dim()
            })
            .collect();
        lines.push(Line::from(labels));
        f.render_widget(Paragraph::new(lines), inner);
    }
}

fn param_line(s: &Spec, st: &StateView, width: usize) -> Line<'static> {
    let v = value(st, &s.path).unwrap_or(serde_json::Value::Null);
    let label = format!(" {:<20}", s.label);
    match &s.kind {
        Kind::Action(_) => Line::from(vec![
            Span::raw(" "),
            Span::styled(format!(" ▶ {} ", s.label), Style::new().fg(Color::Black).bg(Color::Cyan).bold()),
            Span::raw("  press Enter").dim(),
        ]),
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
        Kind::Float { min, max, unit, step } => {
            let x = v.as_f64().unwrap_or(0.0) as f32;
            let prec = if *step >= 1.0 { 0 } else if *step >= 0.1 { 1 } else { 2 };
            let val = format!("{x:>8.prec$} {unit:<3}");
            let slider_w = width.saturating_sub(label.len() + val.len() + 3).clamp(4, 40);
            let pos = (((x - min) / (max - min)).clamp(0.0, 1.0) * (slider_w - 1) as f32).round() as usize;
            let slider: String = (0..slider_w).map(|i| if i == pos { '●' } else if i < pos { '━' } else { '─' }).collect();
            Line::from(vec![Span::raw(label), Span::raw(val), Span::raw(" "), Span::styled(slider, Style::new().fg(Color::Cyan))])
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
