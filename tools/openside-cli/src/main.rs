// OpenSide TUI — ratatui front-end for the Python engine over NDJSON.
//
// All signing/install logic lives in `ipaside_engine serve`; this binary only
// renders, collects keys/mouse and streams engine frames. Fully menu-driven:
// no typing — pick a command, pick an .ipa / an app, engine does the rest.

mod logo;

use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::OnceLock;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListState, Paragraph};
use ratatui::Terminal;
use serde_json::{json, Value};

// ---- palette (ratscope pixel theme) ----
const CY: Color = Color::Rgb(41, 173, 255);
const MAG: Color = Color::Rgb(255, 119, 168);
const GRN: Color = Color::Rgb(0, 228, 54);
const RED: Color = Color::Rgb(255, 0, 77);
const YEL: Color = Color::Rgb(255, 236, 39);
const GRAY: Color = Color::Rgb(131, 118, 156);
const BG: Color = Color::Rgb(16, 16, 26);
const DARK: Color = Color::Rgb(10, 10, 15);

/// every painted cell carries the app bg so transparent terminals stay solid
fn st(fg: Color) -> Style {
    Style::new().fg(fg).bg(BG)
}

static ENGINE_VERSION: OnceLock<String> = OnceLock::new();

// ---- engine process ----
struct Engine {
    child: Child,
}

impl Engine {
    fn spawn() -> io::Result<(Engine, Receiver<Value>)> {
        let python = match std::env::var("OPENSIDE_ENGINE_PYTHON") {
            Ok(p) if !p.is_empty() => p,
            _ => {
                let local = "./src/iPASide.Engine/.venv/bin/python";
                if std::path::Path::new(local).exists() {
                    local.to_string()
                } else {
                    "python".to_string()
                }
            }
        };
        // The engine package is not installed in the venv; `python -m` needs the
        // dir that contains `ipaside_engine/` on cwd or PYTHONPATH.
        let engine_dir = std::env::var("OPENSIDE_ENGINE_DIR")
            .ok()
            .filter(|d| std::path::Path::new(d).join("ipaside_engine").is_dir())
            .or_else(|| {
                ["./src/iPASide.Engine", "../src/iPASide.Engine"]
                    .iter()
                    .find(|d| std::path::Path::new(d).join("ipaside_engine").is_dir())
                    .map(|d| d.to_string())
            })
            .or_else(|| {
                let home = std::env::var("HOME").ok()?;
                ["projects/openside", "iPASide"]
                    .iter()
                    .map(|p| format!("{home}/{p}/src/iPASide.Engine"))
                    .find(|c| std::path::Path::new(c).join("ipaside_engine").is_dir())
            });
        let mut cmd = Command::new(python);
        cmd.args(["-m", "ipaside_engine", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &engine_dir {
            cmd.current_dir(dir).env("PYTHONPATH", dir);
        }
        let mut child = cmd.spawn()?;

        let stdout = child.stdout.take().expect("engine stdout");
        let stderr = child.stderr.take().expect("engine stderr");
        let (tx, rx) = mpsc::channel();
        let err_tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().flatten() {
                let _ = err_tx.send(json!({"type": "stderr", "line": line}));
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) if !line.is_empty() => {
                        if let Ok(v) = serde_json::from_str::<Value>(&line) {
                            if tx.send(v).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        Ok((Engine { child }, rx))
    }

    fn send(&mut self, id: u64, args: &[String]) -> io::Result<()> {
        let frame = json!({ "id": id, "args": args });
        if let Some(stdin) = self.child.stdin.as_mut() {
            stdin.write_all(frame.to_string().as_bytes())?;
            stdin.write_all(b"\n")?;
            stdin.flush()?;
        }
        Ok(())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, r#"{{"type":"shutdown"}}"#);
            let _ = stdin.flush();
        }
        self.child.kill().ok();
    }
}

// ---- actions ----
/// What happens when the user picks something in a dialog.
enum Choice {
    Sideload(String),  // ipa path
    Uninstall(String), // bundle id
}

enum Item {
    /// engine command, no interaction needed
    Run(&'static str),
    /// open the .ipa picker, then run `sideload <path>`
    PickIpa,
    /// fetch installs, pick an app, then run `uninstall <bundle>`
    PickApp,
}

/// aligned with MENU — one-line description shown under the list
const MENU_DESC: &[&str] = &[
    "engine self-check",
    "connected iPhones",
    "selected device details",
    "sideloaded apps + expiry",
    "cached signed .ipas",
    "anisette provisioning status",
    "engine build",
    "sign + install an .ipa",
    "remove a sideloaded app",
    "re-sign apps expiring soon",
];

const MENU: &[(&str, Item)] = &[
    ("doctor", Item::Run("doctor")),
    ("devices", Item::Run("devices")),
    ("device-info", Item::Run("device-info")),
    ("installs", Item::Run("installs")),
    ("signed .ipas", Item::Run("signed")),
    ("anisette status", Item::Run("anisette")),
    ("version", Item::Run("version")),
    ("sideload an .ipa…", Item::PickIpa),
    ("uninstall an app…", Item::PickApp),
    ("refresh expiring", Item::Run("refresh")),
];

struct Dialog {
    title: String,
    items: Vec<(String, Choice)>,
    sel: usize,
}

struct App {
    selection: usize,
    log: Vec<Line<'static>>,
    /// per-command app view; replaces raw JSON dump in the output pane
    panel: Vec<Line<'static>>,
    last_cmd: String,
    spinner: Option<String>,
    devices: Option<Value>,
    dialog: Option<Dialog>,
    /// set while an `installs` fetch is in flight to feed the app picker
    fetching: Option<Fetch>,
    next_id: u64,
}

enum Fetch {
    Uninstall,
}

impl App {
    fn new() -> Self {
        Self {
            selection: 0,
            panel: vec![],
            last_cmd: String::new(),
            log: vec![Line::from(Span::styled(
                "openside — pick with ↑↓, run with ⏎",
                st(GRAY),
            ))],
            spinner: None,
            devices: None,
            dialog: None,
            fetching: None,
            next_id: 1,
        }
    }

    fn push(&mut self, line: Line<'static>) {
        self.log.push(line);
        if self.log.len() > 1000 {
            self.log.drain(0..self.log.len() - 1000);
        }
    }

    fn note(&mut self, text: impl Into<String>) {
        self.push(Line::from(Span::styled(text.into(), st(GRAY))));
    }

    fn run(&mut self, args: Vec<String>) {
        self.next_id += 1;
        let id = self.next_id;
        self.last_cmd = args.first().cloned().unwrap_or_default();
        self.push(Line::from(Span::styled(
            format!("» run {}", args.join(" ")),
            st(CY),
        )));
        STAGED.with(|s| {
            s.borrow_mut()
                .push((id, serde_json::to_string(&args).unwrap_or_default()))
        });
    }
    fn handle_frame(&mut self, frame: Value) {
        match frame.get("type").and_then(Value::as_str) {
            Some("ready") => {
                if let Some(v) = frame.get("version").and_then(Value::as_str) {
                    let _ = ENGINE_VERSION.set(v.to_string());
                }
                self.note(format!(
                    "engine ready · v{}",
                    frame.get("version").cloned().unwrap_or(Value::Null)
                ));
            }
            Some("progress") => {
                self.spinner = frame.get("line").and_then(Value::as_str).map(str::to_string);
            }
            Some("result") => {
                self.spinner = None;
                let ok = frame.get("ok").and_then(Value::as_bool).unwrap_or(false);
                let data = frame.get("data").unwrap_or(&Value::Null);
                let err = frame.get("error").unwrap_or(&Value::Null);
                self.panel = if ok {
                    command_view(&self.last_cmd, data)
                } else {
                    render_value(err, RED)
                };
                // feed the app picker when that fetch was for uninstall
                if ok && matches!(self.fetching, Some(Fetch::Uninstall)) {
                    self.fetching = None;
                    if let Some(dialog) = app_picker(frame.get("data").unwrap_or(&Value::Null)) {
                        self.dialog = Some(dialog);
                    } else {
                        self.note("no sideloaded apps recorded");
                    }
                }
            }
            Some("event") => {
                if frame.get("name").and_then(Value::as_str) == Some("devices") {
                    let data = frame.get("data").cloned().unwrap_or(Value::Null);
                    let count = data.as_array().map(|a| a.len()).unwrap_or(0);
                    self.devices = Some(data);
                    self.push(Line::from(format!("devices changed — {count} connected")));
                }
            }
            Some("stderr") => {
                let line = frame.get("line").and_then(Value::as_str).unwrap_or("");
                let red = Line::from(Span::styled(line.to_string(), st(RED)));
                if !self.panel.is_empty() {
                    self.panel.push(red);
                } else {
                    self.log.push(red);
                }
            }
            _ => {
                let text = serde_json::to_string(&frame).unwrap_or_default();
                self.note(format!("frame {text}"));
            }
        }
    }
}

thread_local! {
    /// commands queued by App, sent to the engine by the main loop
    static STAGED: std::cell::RefCell<Vec<(u64, String)>> = std::cell::RefCell::new(vec![]);
}

/// installs JSON records (name/bundle_id/…) → uninstall picker
fn app_picker(data: &Value) -> Option<Dialog> {
    let mut items = Vec::new();
    if let Some(list) = data.as_array() {
        for record in list {
            let bundle = record.get("bundle_id").and_then(Value::as_str);
            if let Some(bundle) = bundle {
                let name = record.get("name").and_then(Value::as_str).unwrap_or(bundle);
                items.push((
                    format!("{name} — {bundle}"),
                    Choice::Uninstall(bundle.to_string()),
                ));
            }
        }
    }
    if items.is_empty() {
        return None;
    }
    Some(Dialog {
        title: "uninstall which app?".into(),
        items,
        sel: 0,
    })
}

/// scan common dirs for sideloadable .ipas (env override: OPENSIDE_IPA_DIRS)
fn ipa_candidates() -> Vec<String> {
    let dirs = std::env::var("OPENSIDE_IPA_DIRS").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| format!("{h}/Downloads:{h}/Desktop:{h}/macos-vm/shared"))
            .unwrap_or_default()
    });
    let mut found = Vec::new();
    for dir in dirs.split(':').filter(|d| !d.is_empty()) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_ipa = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("ipa"))
                == Some(true);
            if is_ipa {
                found.push(path.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    found
}

// ---- render a JSON value as colored terminal lines ----
fn render_value(value: &Value, base: Color) -> Vec<Line<'static>> {
    match value {
        Value::String(s) => vec![Line::from(s.clone())],
        Value::Number(n) => vec![Line::from(Span::styled(n.to_string(), st(YEL)))],
        Value::Bool(b) => vec![Line::from(Span::styled(
            b.to_string(),
            st(if *b { GRN } else { MAG }),
        ))],
        Value::Object(map) => map
            .iter()
            .flat_map(|(key, val)| {
                let shown = match val {
                    Value::String(s) => s.clone(),
                    Value::Null => "empty".to_string(),
                    other => {
                        let compact = serde_json::to_string(other).unwrap_or_default();
                        if compact.len() > 200 {
                            format!("{}…", &compact[..200])
                        } else {
                            compact
                        }
                    }
                };
                vec![Line::from(vec![
                    Span::styled(format!("  {key}"), st(CY)),
                    Span::styled(format!("   {shown}"), st(base)),
                ])]
            })
            .collect(),
        Value::Array(items) if items.is_empty() => {
            vec![Line::from(Span::styled("  · empty", st(GRAY)))]
        }
        Value::Array(items) => items.iter().flat_map(|v| render_value(v, base)).collect(),
        Value::Null => vec![Line::from(Span::styled("  · empty", st(GRAY)))],
    }
}

// ---- per-command app views (panels replace raw JSON) ----
fn command_view(cmd: &str, data: &Value) -> Vec<Line<'static>> {
    match cmd {
        "doctor" => doctor_view(data),
        "devices" => devices_view(data),
        "installs" => installs_view(data),
        _ => render_value(data, Color::White),
    }
}

fn status_ink(status: &str) -> (&'static str, Color) {
    match status {
        "fail" | "expired" => ("✗", RED),
        "warn" => ("!", YEL),
        _ => ("✓", GRN),
    }
}

fn doctor_view(data: &Value) -> Vec<Line<'static>> {
    let mut out = vec![];
    let overall = data.get("overall").and_then(Value::as_str).unwrap_or("fail");
    let (mark, col) = status_ink(overall);
    out.push(Line::from(Span::styled(
        format!("{mark} overall: {overall}"),
        st(col).add_modifier(Modifier::BOLD),
    )));
    if let Some(checks) = data.get("checks").and_then(Value::as_array) {
        for check in checks {
            let name = check.get("name").and_then(Value::as_str).unwrap_or("?");
            let status = check.get("status").and_then(Value::as_str).unwrap_or("fail");
            let detail = check.get("detail").and_then(Value::as_str).unwrap_or("");
            let (mark, col) = status_ink(status);
            out.push(Line::from(vec![
                Span::styled(format!("{mark} "), st(col)),
                Span::styled(format!("{name:<22}"), st(Color::White)),
                Span::styled(detail.to_string(), st(GRAY)),
            ]));
        }
    }
    out
}

fn devices_view(data: &Value) -> Vec<Line<'static>> {
    let Some(list) = data.as_array() else {
        return render_value(data, Color::White);
    };
    if list.is_empty() {
        return vec![Line::from(Span::styled(
            "○ no iPhone connected — plug in via USB, unlock, tap Trust",
            st(GRAY),
        ))];
    }
    list.iter()
        .map(|d| {
            let serial = d.get("serial").and_then(Value::as_str).unwrap_or("?");
            let tail = &serial[serial.len().saturating_sub(8)..];
            let conn = d.get("connection_type").and_then(Value::as_str).unwrap_or("?");
            let id = d.get("device_id").map(Value::to_string).unwrap_or_default();
            Line::from(vec![
                Span::styled("● ", st(GRN)),
                Span::styled("iPhone  ", st(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{conn}  …{tail}  id {id}"), st(GRAY)),
            ])
        })
        .collect()
}

fn installs_view(data: &Value) -> Vec<Line<'static>> {
    let Some(list) = data.as_array() else {
        return render_value(data, Color::White);
    };
    if list.is_empty() {
        return vec![Line::from(Span::styled(
            "· nothing sideloaded yet",
            st(GRAY),
        ))];
    }
    list.iter()
        .map(|r| {
            let name = r.get("name").and_then(Value::as_str).unwrap_or("?");
            let bundle = r.get("bundle_id").and_then(Value::as_str).unwrap_or("?");
            let expired = r.get("expired").and_then(Value::as_bool).unwrap_or(false);
            let days = r.get("days_left").and_then(Value::as_f64);
            let (badge, col) = if expired {
                ("✗ expired".to_string(), RED)
            } else if let Some(d) = days.filter(|&d| d <= 2.0) {
                (format!("◐ {d:.0}d left"), RED)
            } else if let Some(d) = days.filter(|&d| d <= 7.0) {
                (format!("◐ {d:.0}d left"), YEL)
            } else if let Some(d) = days {
                (format!("● {d:.0}d left"), GRN)
            } else {
                ("● unknown".to_string(), GRAY)
            };
            Line::from(vec![
                Span::styled(format!("{badge:<11} "), st(col)),
                Span::styled(format!("{name:<20.20}"), st(Color::White)),
                Span::styled(bundle.to_string(), st(GRAY)),
            ])
        })
        .collect()
}

// ---- header logo, half-block rendering ----
fn logo_lines(word: &str) -> Vec<Line<'static>> {
    let g = logo::grid(word);
    let ramp = [CY, CY, MAG, MAG, GRN, GRN];
    (0..g.len().div_ceil(2))
        .map(|pair| {
            let top_row = &g[pair * 2];
            let bot_row: Vec<bool> = g
                .get(pair * 2 + 1)
                .cloned()
                .unwrap_or_else(|| vec![false; top_row.len()]);
            let color = ramp[pair.min(ramp.len() - 1)];
            let mut spans = Vec::with_capacity(top_row.len());
            for (top, bottom) in top_row.iter().zip(bot_row.iter()) {
                if !top && !bottom {
                    spans.push(Span::raw(" "));
                } else {
                    spans.push(Span::styled(
                        "▀",
                        Style::new()
                            .fg(if *top { color } else { BG })
                            .bg(if *bottom { color } else { BG }),
                    ));
                }
            }
            Line::from(spans)
        })
        .collect()
}

// ---- chrome (ratscope grammar: solid bg + pixel frame + inner title) ----
fn pblock(col: Color) -> Block<'static> {
    Block::bordered()
        .border_set(border::QUADRANT_OUTSIDE)
        .border_style(st(col))
        .style(Style::new().bg(BG))
}

fn box_title(f: &mut ratatui::Frame, area: Rect, title: &str, col: Color) -> Rect {
    let b = pblock(col);
    f.render_widget(&b, area);
    let inner = b.inner(area);
    let chunks: [Rect; 2] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" ■ {title}"),
            st(col).add_modifier(Modifier::BOLD),
        ))),
        chunks[0],
    );
    chunks[1]
}

/// max while following the tail; otherwise frozen offset clamped in range
fn real_scroll(len: usize, height: u16, stored: u16, bottom: bool) -> u16 {
    let max = len.saturating_sub(height as usize) as u16;
    if bottom {
        max
    } else {
        stored.min(max)
    }
}

fn connected(devices: &Option<Value>) -> usize {
    devices
        .as_ref()
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

const FOOT: &str = " ↑↓/click pick · click again or ⏎ run · esc quit ";

/// run the currently selected menu entry (key ⏎ or second click)
fn activate_menu(app: &mut App) {
    match &MENU[app.selection].1 {
        Item::Run(cmd) => app.run(vec![(*cmd).to_string()]),
        Item::PickIpa => {
            let found = ipa_candidates();
            if found.is_empty() {
                app.note("no .ipa in OPENSIDE_IPA_DIRS (default Downloads/Desktop/macos-vm/shared)");
            } else {
                app.dialog = Some(Dialog {
                    title: "sideload which .ipa?".into(),
                    items: found
                        .into_iter()
                        .map(|p| {
                            let label = p.rsplit('/').next().unwrap_or(&p).to_string();
                            (label, Choice::Sideload(p))
                        })
                        .collect(),
                    sel: 0,
                });
            }
        }
        Item::PickApp => {
            app.fetching = Some(Fetch::Uninstall);
            app.run(vec!["installs".into()]);
        }
    }
}

/// apply the chosen dialog item and close it
fn confirm_dialog(app: &mut App) {
    if let Some(mut dialog) = app.dialog.take() {
        let (_, choice) = dialog.items.remove(dialog.sel);
        match choice {
            Choice::Sideload(path) => app.run(vec!["sideload".into(), path]),
            Choice::Uninstall(bundle) => app.run(vec!["uninstall".into(), bundle]),
        }
    }
}

fn main() -> io::Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let (mut engine, rx) = match Engine::spawn() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("failed to spawn engine: {e}");
            return Err(e);
        }
    };
    let mut app = App::new();
    let mut log_bottom = true; // stick to the tail until the user scrolls
    let mut log_stored: u16 = 0;
    let mut last_click: Option<(u16, u16)> = None;
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    terminal.hide_cursor()?;

    loop {
        while let Ok(frame) = rx.try_recv() {
            app.handle_frame(frame);
        }
        // flush queued runs to the engine
        let queued: Vec<(u64, String)> = STAGED.with(|s| std::mem::take(&mut *s.borrow_mut()));
        for (id, args_json) in queued {
            let args: Vec<String> =
                serde_json::from_str(&args_json).expect("staged args roundtrip");
            engine.send(id, &args)?;
        }

        let mut hit_menu_list = Rect::default();
        let mut hit_dialog_list = Rect::default();
        terminal.draw(|f| {
            let area = f.area();
            f.render_widget(Block::default().style(Style::new().bg(BG)), area);

            let rows = Layout::vertical([
                Constraint::Length(logo::SLOT_ROWS as u16 / 2 + 1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
            let header = Layout::horizontal([Constraint::Length(57), Constraint::Min(0)])
                .split(rows[0]);
            f.render_widget(Paragraph::new(logo_lines("openside")), header[0]);
            let n_devices = connected(&app.devices);
            let busy = app.spinner.is_some() || app.fetching.is_some();
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" anx set ", Style::new().fg(DARK).bg(GRAY)),
                    Span::raw(" "),
                    Span::styled(format!(" engine {} ", ENGINE_VERSION.get().map(String::as_str).unwrap_or("…")), Style::new().fg(BG).bg(CY)),
                    Span::raw(" "),
                    Span::styled(
                        format!(" {dot} {n_devices} device{plural} ", dot = if n_devices > 0 { "●" } else { "○" }, plural = if n_devices == 1 { "" } else { "s" }),
                        Style::new().fg(BG).bg(if n_devices > 0 { GRN } else { GRAY }),
                    ),
                    if busy { Span::raw(" ".to_string()) } else { Span::raw(" ".to_string()) },
                ]))
                .alignment(ratatui::layout::Alignment::Right)
                .style(Style::new().bg(BG)),
                header[1],
            );

            let body = Layout::horizontal([Constraint::Length(32), Constraint::Min(0)])
                .split(rows[1]);
            let menu_cols: [Rect; 2] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(
                    box_title(f, body[0], "commands", CY),
                );
            hit_menu_list = menu_cols[0];
            let mut list = ListState::default();
            list.select(Some(app.selection));
            f.render_stateful_widget(
                List::new(
                    MENU.iter()
                        .map(|(name, _)| format!("▪ {name}"))
                        .collect::<Vec<String>>(),
                )
                .highlight_symbol("» ")
                .highlight_style(st(Color::White).add_modifier(Modifier::BOLD)),
                menu_cols[0],
                &mut list,
            );
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {} ", MENU_DESC[app.selection]),
                    st(GRAY).add_modifier(Modifier::DIM),
                )))
                .style(Style::new().bg(BG)),
                menu_cols[1],
            );

            let panel_live = !app.panel.is_empty();
            let title = if panel_live {
                format!("{} ", if app.last_cmd.is_empty() { "output" } else { &app.last_cmd })
            } else {
                format!("ticker — {} lines", app.log.len())
            };
            let log_inner = box_title(f, body[1], &title, MAG);
            if panel_live {
                f.render_widget(
                    Paragraph::new(app.panel.clone()).style(Style::new().bg(BG)),
                    log_inner,
                );
            } else {
                let vis = log_inner.height.saturating_sub(1);
                let scroll = real_scroll(app.log.len(), vis, log_stored, log_bottom);
                f.render_widget(
                    Paragraph::new(app.log.clone())
                        .style(Style::new().bg(BG))
                        .scroll((scroll, 0)),
                    log_inner,
                );
            }

            // footer
            f.render_widget(
                Block::default().style(Style::new().bg(DARK)),
                rows[2],
            );
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(FOOT, st(GRAY))))
                    .style(Style::new().bg(DARK)),
                rows[2],
            );
            let status_text = match (&app.spinner, &app.fetching) {
                (Some(line), _) => format!("{line}… "),
                (_, Some(_)) => "fetching app list… ".to_string(),
                _ => format!("ready · {} connected", connected(&app.devices)),
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    status_text,
                    if app.spinner.is_some() { st(YEL) } else { st(GRN) },
                )))
                .style(Style::new().bg(DARK))
                .alignment(ratatui::layout::Alignment::Right),
                rows[2],
            );

            // dialog overlay (last = on top)
            if let Some(dlog) = &app.dialog {
                let width = (57.min(area.width.saturating_sub(4))).max(20);
                let height = (dlog.items.len() as u16 + 5).min(rows[1].height.saturating_sub(2));
                let x = area.x + (area.width.saturating_sub(width)) / 2;
                let y = rows[1].y + (rows[1].height.saturating_sub(height)) / 2;
                let inner = box_title(f, Rect { x, y, width, height }, &dlog.title, YEL);
                hit_dialog_list = inner;
                let mut dsel = ListState::default();
                dsel.select(Some(dlog.sel));
                f.render_stateful_widget(
                    List::new(
                        dlog.items
                            .iter()
                            .map(|(label, _)| label.clone())
                            .collect::<Vec<String>>(),
                    )
                    .highlight_symbol("» ")
                    .highlight_style(st(Color::White).add_modifier(Modifier::BOLD)),
                    inner,
                    &mut dsel,
                );
            }
        })?;

        if !crossterm::event::poll(Duration::from_millis(50))? {
            continue;
        }
        let ev = crossterm::event::read()?;
        // dialog consumes navigation first
        if let Some(dialog) = app.dialog.as_mut() {
            if let crossterm::event::Event::Key(key) = &ev {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    match key.code {
                        crossterm::event::KeyCode::Up => dialog.sel = dialog.sel.saturating_sub(1),
                        crossterm::event::KeyCode::Down => {
                            dialog.sel = (dialog.sel + 1).min(dialog.items.len() - 1)
                        }
                        crossterm::event::KeyCode::PageUp => dialog.sel = dialog.sel.saturating_sub(10),
                        crossterm::event::KeyCode::PageDown => {
                            dialog.sel = (dialog.sel + 10).min(dialog.items.len() - 1)
                        }
                        crossterm::event::KeyCode::Esc => app.dialog = None,
                        crossterm::event::KeyCode::Enter => confirm_dialog(&mut app),
                        _ => {}
                    }
                    continue;
                }
            }
            continue;
        }
        match ev {
            crossterm::event::Event::Key(key)
                if key.kind == crossterm::event::KeyEventKind::Press =>
            {
                match key.code {
                    crossterm::event::KeyCode::Up => app.selection = app.selection.saturating_sub(1),
                    crossterm::event::KeyCode::Down => {
                        app.selection = (app.selection + 1).min(MENU.len() - 1)
                    }
                    crossterm::event::KeyCode::Home => app.selection = 0,
                    crossterm::event::KeyCode::End => app.selection = MENU.len() - 1,
                    crossterm::event::KeyCode::Enter => activate_menu(&mut app),
                    crossterm::event::KeyCode::PageUp => {
                        log_bottom = false;
                    }
                    crossterm::event::KeyCode::PageDown => {
                        log_bottom = true;
                    }
                    crossterm::event::KeyCode::Esc => break,
                    crossterm::event::KeyCode::Char('c')
                        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        break
                    }
                    _ => {}
                }
            }
            crossterm::event::Event::Mouse(mouse) => match mouse.kind {
                crossterm::event::MouseEventKind::ScrollUp => {
                    log_bottom = false;
                    log_stored = log_stored.saturating_sub(2);
                }
                crossterm::event::MouseEventKind::ScrollDown => log_bottom = true,
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                    let (mx, my) = (mouse.column, mouse.row);
                    if app.dialog.is_some() {
                        // inside the dialog list?
                        if my >= hit_dialog_list.y && my < hit_dialog_list.y + hit_dialog_list.height
                            && mx >= hit_dialog_list.x
                        {
                            if let Some(d) = app.dialog.as_mut() {
                                let idx = (my - hit_dialog_list.y) as usize;
                                d.sel = idx.min(d.items.len() - 1);
                                if d.sel as u16 == my - hit_dialog_list.y
                                    && d.items.len() > 0
                                    && (d.sel as u16) == my - hit_dialog_list.y
                                    && last_click == Some((mx, my))
                                {
                                    confirm_dialog(&mut app);
                                    last_click = None;
                                } else {
                                    last_click = Some((mx, my));
                                }
                            }
                        } else {
                            // click outside the dialog closes it
                            app.dialog = None;
                            last_click = None;
                        }
                    } else if my >= hit_menu_list.y
                        && my < hit_menu_list.y + hit_menu_list.height
                        && mx >= hit_menu_list.x
                    {
                        let idx = ((my - hit_menu_list.y) as usize).min(MENU.len() - 1);
                        if idx == app.selection && last_click.is_some() {
                            activate_menu(&mut app);
                            last_click = None;
                        } else {
                            app.selection = idx;
                            last_click = Some((mx, my));
                        }
                    } else {
                        last_click = None;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    crossterm::terminal::disable_raw_mode()?;
    terminal.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_gives_pairs_and_ink() {
        let lines = logo_lines("openside");
        assert_eq!(
            lines.len(),
            logo::grid("openside").len().div_ceil(2),
            "logo desenha pares de pixels"
        );
        let styled = lines.iter().flat_map(|l| l.spans.clone()).count();
        assert!(styled > 50, "logo precisa ter ▀ desenhado");
    }

    #[test]
    fn app_picker_from_installs_json() {
        let data: Value = serde_json::from_str(
            r#"[{"name":"WhatsApp","bundle_id":"net.whatsapp.WhatsApp","expired":false,"days_left":5},
                {"name":"X","bundle_id":"x.app"}]"#,
        )
        .unwrap();
        let dialog = app_picker(&data).unwrap();
        assert_eq!(dialog.items.len(), 2);
        assert!(matches!(dialog.items[1].1, Choice::Uninstall(ref b) if b == "x.app"));
        assert!(dialog.title.contains("uninstall"));
    }

    #[test]
    fn doctor_panel() {
        let d: Value = serde_json::from_str(
            r#"{"overall":"ok","checks":[{"name":"Python runtime","status":"ok","detail":"Python 3.14"},{"name":"AMDS","status":"fail","detail":"not found"}]}"#,
        )
        .unwrap();
        let view = doctor_view(&d);
        assert_eq!(view.len(), 3, "cabeçalho + 2 checks");
        assert!(view[0].spans[0].content.contains("overall"));
        assert!(view[2].spans[0].content.contains("✗"), "check fail → ✗");
    }

    #[test]
    fn installs_panel() {
        let d: Value = serde_json::from_str(
            r#"[{"name":"Um batismo de nome comprido demais","bundle_id":"a.b","expired":false,"days_left":30},
                {"name":"FWhtUf","bundle_id":"c.d","expired":true,"days_left":null},
                {"name":"Y","bundle_id":"e.f","expired":false,"days_left":1.5}]"#,
        )
        .unwrap();
        let view = installs_view(&d);
        assert_eq!(view.len(), 3);
        assert!(view[1].spans[0].content.contains("expired"), "expirado → badge");
        assert!(view[2].spans[0].content.contains("d left"), "dias → badge");
        assert!(view[0].spans[1].content.len() <= 20, "nome cortado a 20");
    }

    #[test]
    fn devices_panel() {
        let empty = devices_view(&serde_json::json!([]));
        assert!(empty[0].spans[0].content.contains("no iPhone"));
        let d = serde_json::json!([{"serial":"00008030-0009454E0C84802E","connection_type":"USB","device_id":1}]);
        let rows = devices_view(&d);
        assert!(rows[0].spans[2].content.contains("402E"));
        assert!(rows[0].spans[0].content.contains("●"));
    }

    #[test]
    fn app_picker_empty() {
        assert!(app_picker(&json!([])).is_none());
        assert!(app_picker(&json!(null)).is_none());
    }
}
