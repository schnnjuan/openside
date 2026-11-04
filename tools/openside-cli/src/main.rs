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
// Maki palette — from the reference art: night-blue bg, dark green hair,
// violet glasses, warm skin. Roles unchanged (info/success/warn/destroy).
// Palette extracted from the character art (user-supplied hexes):
// greens drive everything, purple is the accent, gold = warning,
// warm shadow = destructive; bg/outline come from the darkest greens.
const CY: Color = Color::Rgb(0x24, 0xA0, 0x67); // verde reflexo médio
const MAG: Color = Color::Rgb(0xAA, 0x4E, 0xCB); // roxo reflexo
const GRN: Color = Color::Rgb(0x64, 0xE0, 0xA4); // verde reflexo claro
const RED: Color = Color::Rgb(0xAE, 0x55, 0x44); // contorno quente
const YEL: Color = Color::Rgb(0xBE, 0x9D, 0x7A); // ouro da íris
const GRAY: Color = Color::Rgb(0x5D, 0x8A, 0x77); // verde dim derivado
const BG: Color = Color::Rgb(0x01, 0x2D, 0x2D); // contorno verde-escuro
const DARK: Color = Color::Rgb(0x01, 0x1D, 0x1D);

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
    Uninstall(String), // bundle id
    Refresh(String),   // bundle id
    Forget(String),    // bundle id
    /// arbitrary `<cmd> <one-arg>` (inspect/sign .ipa)
    RunWith(String, String),
}

enum DialogKind {
    Pick { items: Vec<(String, Choice)>, sel: usize },
    Input { prompt: String, masked: bool, buffer: String },
}

enum Item {
    /// non-selectable group title
    Header,
    /// engine command, no interaction needed
    Run(&'static str),
    /// open the .ipa picker, then run `<cmd> <path>`
    PickIpa(&'static str),
    /// fetch installs, pick an app, then run the mapped command
    PickApp(AppAction),
    /// sign in to an Apple ID (email → password → 2FA wizard)
    Login,
}

#[derive(Clone, Copy)]
enum AppAction {
    Uninstall,
    Forget,
    RefreshOne,
}

impl Item {
    fn selectable(&self) -> bool {
        !matches!(self, Item::Header)
    }
}

/// aligned with MENU — one-line description shown under the list
const MENU_DESC: &[&str] = &[
    "getting started",
    "engine self-check",
    "connected iPhones",
    "selected device details",
    "sign in to an Apple ID",
    "sideloading",
    "sign + install an .ipa",
    "sign an .ipa into a new file",
    "sideloaded apps + expiry",
    "re-sign apps expiring soon",
    "re-sign one app by record",
    "cached signed .ipas",
    "apple id",
    "development teams on the account",
    "certs / app ids / devices slots",
    "every signed-in Apple ID",
    "device",
    "show developer mode state",
    "pairing file status",
    "Dopamine advice",
    "apps on the device",
    "details for one device",
    "ipa tooling",
    "read an .ipa metadata",
    "infrastructure",
    "anisette provisioning status",
    "Apple USB device-support status",
    "remove a sideload record",
    "remove a sideloaded app",
    "engine build",
];

const MENU: &[(&str, Item)] = &[
    ("— getting started", Item::Header),
    ("doctor", Item::Run("doctor")),
    ("devices", Item::Run("devices")),
    ("device-info", Item::Run("device-info")),
    ("login apple id…", Item::Login),
    ("— sideloading", Item::Header),
    ("sideload an .ipa…", Item::PickIpa("sideload")),
    ("sign an .ipa…", Item::PickIpa("sign")),
    ("installs", Item::Run("installs")),
    ("refresh expiring", Item::Run("refresh")),
    ("refresh one app…", Item::PickApp(AppAction::RefreshOne)),
    ("signed .ipas", Item::Run("signed")),
    ("— apple id", Item::Header),
    ("teams", Item::Run("teams")),
    ("slots", Item::Run("slots")),
    ("accounts", Item::Run("login --accounts")),
    ("— device", Item::Header),
    ("developer-mode", Item::Run("developer-mode")),
    ("pairing", Item::Run("pairing")),
    ("jailbreak", Item::Run("jailbreak")),
    ("installed apps", Item::Run("apps")),
    ("device-info", Item::Run("device-info")),
    ("— ipa tooling", Item::Header),
    ("inspect an .ipa…", Item::PickIpa("inspect")),
    ("— infrastructure", Item::Header),
    ("anisette status", Item::Run("anisette")),
    ("apple-support", Item::Run("apple-support")),
    ("forget an app…", Item::PickApp(AppAction::Forget)),
    ("uninstall an app…", Item::PickApp(AppAction::Uninstall)),
    ("version", Item::Run("version")),
];

struct Dialog {
    title: String,
    kind: DialogKind,
}

/// login wizard: email collected first, then the app-specific password,
/// then the 2FA code. Password hits disk only inside an 0600 temp file the
/// engine reads per request; removed as soon as login answers.
struct LoginFlow {
    email: String,
    password: String,
    creds: std::path::PathBuf,
}

struct App {
    login: Option<LoginFlow>,
    selection: usize,
    log: Vec<Line<'static>>,
    /// per-command app view; replaces raw JSON dump in the output pane
    panel: Vec<Line<'static>>,
    last_cmd: String,
    spinner: Option<String>,
    devices: Option<Value>,
    dialog: Option<Dialog>,
    /// app action waiting for the next `installs` result
    app_action: Option<AppAction>,
    /// apps with signature expiry in under 15 days (installs data)
    expiring: usize,
    /// apps already expired (installs data)
    expired: usize,
    next_id: u64,
}

fn write_creds(email: &str, password: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("openside-login-{}-{nonce}.json", std::process::id()));
    let body = serde_json::json!({ "email": email, "password": password });
    #[cfg(unix)]
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&path).expect("create creds file");
        f.write_all(body.to_string().as_bytes()).expect("write creds");
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(0o600),
        );
    }
    path
}

fn drop_creds(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

impl App {
    fn new() -> Self {
        Self {
            login: None,
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
            app_action: None,
            expiring: 0,
            expired: 0,
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
                    let mut out = vec![Line::from(Span::styled(
                        "✗ erro do engine",
                        st(RED).add_modifier(Modifier::BOLD),
                    ))];
                    out.extend(render_value(err, 0));
                    out
                };
                // the next `installs` result after an app action feeds the picker
                if ok && self.app_action.is_some() {
                    if let Some(action) = self.app_action.take() {
                        let data = frame.get("data").unwrap_or(&Value::Null);
                        let picker = app_picker(data, choice_for(action));
                        if let Some(dialog) = picker {
                            self.dialog = Some(dialog);
                        } else {
                            self.note("no sideloaded apps recorded");
                        }
                    }
                }
                // header counters: expiry window of the installs (recomputed, not accumulated)
                if ok && self.last_cmd == "installs" {
                    self.expired = 0;
                    self.expiring = 0;
                    for item in data.as_array().unwrap_or(&vec![]) {
                        let expired = item.get("expired").and_then(Value::as_bool).unwrap_or(false);
                        let days = item.get("days_left").and_then(Value::as_u64).unwrap_or(999);
                        if expired {
                            self.expired += 1;
                        } else if days < 15 {
                            self.expiring += 1;
                        }
                    }
                }
                // login wizard lifecycle
                if self.last_cmd == "login" {
                    let status = frame
                        .get("data")
                        .and_then(|d| d.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("ok");
                    if ok && status == "2fa_required" && self.login.is_some() {
                        self.dialog = Some(Dialog {
                            title: "2FA code:".into(),
                            kind: DialogKind::Input {
                                prompt: "2FA code:".into(),
                                masked: true,
                                buffer: String::new(),
                            },
                        });
                    } else {
                        if let Some(flow) = self.login.take() {
                            drop_creds(&flow.creds);
                        }
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
                // raw frame is machine data: only the kind goes to the ticker
                let kind = frame.get("type").and_then(Value::as_str).unwrap_or("?");
                self.note(format!("frame {kind} ignorado"));
            }
        }
    }
}

thread_local! {
    /// commands queued by App, sent to the engine by the main loop
    static STAGED: std::cell::RefCell<Vec<(u64, String)>> = std::cell::RefCell::new(vec![]);
}

/// installs JSON records (name/bundle_id/…) → app-action picker
fn app_picker(data: &Value, make_choice: fn(String) -> Choice) -> Option<Dialog> {
    let mut items = Vec::new();
    if let Some(list) = data.as_array() {
        for record in list {
            let bundle = record.get("bundle_id").and_then(Value::as_str);
            if let Some(bundle) = bundle {
                let name = record.get("name").and_then(Value::as_str).unwrap_or(bundle);
                items.push((format!("{name} — {bundle}"), make_choice(bundle.to_string())));
            }
        }
    }
    if items.is_empty() {
        return None;
    }
    Some(Dialog {
        title: "which app?".into(),
        kind: DialogKind::Pick { items, sel: 0 },
    })
}

fn choice_for(action: AppAction) -> fn(String) -> Choice {
    match action {
        AppAction::Uninstall => Choice::Uninstall,
        AppAction::Forget => Choice::Forget,
        AppAction::RefreshOne => Choice::Refresh,
    }
}

/// scan for sideloadable .ipas — same roots/depth/cap as the engine
/// (`_candidate_ipas`), so the picker never shows an illusion of files
fn ipa_candidates() -> Vec<String> {
    let env = std::env::var("IPASIDE_IPA_DIRS")
        .or_else(|_| std::env::var("OPENSIDE_IPA_DIRS"))
        .unwrap_or_default();
    let dirs: Vec<String> = if env.is_empty() {
        vec![
            ".".to_string(),
            format!("{}/Downloads", home_dir()),
            format!("{}/Desktop", home_dir()),
            format!("{}/Documents", home_dir()),
        ]
    } else {
        env.split(':').filter(|d| !d.is_empty()).map(String::from).collect()
    };
    // BFS one level of subdirectories, like the engine (depth 2)
    let mut found: Vec<(std::time::SystemTime, String)> = vec![];
    let mut queue: Vec<(std::path::PathBuf, u8)> = dirs
        .iter()
        .map(|d| (std::path::PathBuf::from(d), 0))
        .collect();
    let mut seen = std::collections::HashSet::new();
    while let Some((dir, level)) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_file = path.is_file();
            if is_file && path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("ipa")) == Some(true) {
                if let (Ok(key), Ok(meta)) = (path.canonicalize(), entry.metadata()) {
                    if seen.insert(key) {
                        found.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), path.to_string_lossy().into_owned()));
                    }
                }
            } else if path.is_dir() && level < 2 {
                queue.push((path, level + 1));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0)); // mais recente primeiro
    found.into_iter().take(40).map(|(_, p)| p).collect()
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_default()
}

// ---- render a JSON value as colored terminal lines ----
/// scalar ink by type: numbers amber, bools ✓/✗, nulls dim
fn scalar_ink(value: &Value) -> Span<'static> {
    match value {
        Value::String(s) => Span::styled(tidy_value(s), st(Color::White)),
        Value::Number(n) => Span::styled(n.to_string(), st(YEL)),
        Value::Bool(b) => Span::styled(
            if *b { "✓" } else { "✗" }.to_string(),
            st(if *b { GRN } else { RED }),
        ),
        _ => Span::styled("—", st(GRAY)),
    }
}

fn render_value(value: &Value, depth: usize) -> Vec<Line<'static>> {
    let pad = "  ".repeat(depth + 1);
    match value {
        Value::String(s) => vec![Line::from(format!("{pad}{s}"))],
        Value::Number(n) => vec![Line::from(Span::styled(n.to_string(), st(YEL)))],
        Value::Bool(_) => vec![Line::from(scalar_ink(value))],
        Value::Null => vec![Line::from(Span::styled("—", st(GRAY)))],
        Value::Object(map) => map
            .iter()
            .flat_map(|(key, val)| {
                let key_s = Span::styled(format!("  {}", humanize(key)), st(CY));
                match val {
                    Value::Object(_) | Value::Array(_) => {
                        let mut out = vec![Line::from(format!("  {}", humanize(key)))];
                        out.extend(render_value(val, depth + 1));
                        out
                    }
                    _ => vec![Line::from(vec![key_s, Span::raw("  "), scalar_ink(val)])],
                }
            })
            .collect(),
        Value::Array(items) if items.is_empty() => {
            vec![Line::from(Span::styled("  · empty", st(GRAY)))]
        }
        // cada item do array em sequência; um marcador entre registros para ler como lista
        Value::Array(items) => items
            .iter()
            .enumerate()
            .flat_map(|(i, item)| {
                let mut out = render_value(item, depth);
                if i + 1 < items.len() && matches!(item, Value::Object(_)) {
                    out.push(Line::from(Span::styled(
                        "  ·".to_string(),
                        st(GRAY),
                    )));
                }
                out
            })
            .collect(),
    }
}

// ---- per-command app views (panels replace raw JSON) ----
fn command_view(cmd: &str, data: &Value) -> Vec<Line<'static>> {
    match cmd {
        "doctor" => doctor_view(data),
        "devices" => devices_view(data),
        "installs" => installs_view(data),
        "device-info" => kv_view(data),
        "version" => {
            let engine = match data.get("version").and_then(Value::as_str) {
                Some(v) => v.to_string(),
                None => data.to_string(),
            };
            vec![
                Line::from(Span::styled(
                    format!(" openside v{} ", env!("CARGO_PKG_VERSION")),
                    Style::new().fg(BG).bg(MAG).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    format!(" engine {engine} "),
                    Style::new().fg(BG).bg(CY).add_modifier(Modifier::BOLD),
                )),
            ]
        }
        // accounts/slots output is a dict of account/device summaries
        "login" if data.get("accounts").is_some() || data.get("authenticated").is_some() => {
            login_status_view(data)
        }
        "anisette" => anisette_view(data),
        "refresh" | "sideload" | "install" => refresh_view(data),
        "sign" | "forget" | "uninstall" => op_view(data),
        "signed" => signed_view(data),
        _ => render_value(data, 0),
    }
}

/// "ios_version" → "iOS Version"; "udid" → "UDID"
fn humanize(key: &str) -> String {
    key.split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| match part {
            "ios" => "iOS".to_string(),
            "udid" | "uuid" | "ecid" | "id" | "anisette" | "url" | "gsm" => part.to_uppercase(),
            "app" => "App".to_string(),
            other => {
                let mut c = other.chars();
                match c.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

fn compact(value: &Value) -> String {
    let raw = serde_json::to_string(value).unwrap_or_default();
    if raw.len() > 160 {
        format!("{}…", &raw[..159])
    } else {
        raw
    }
}

/// validity gauge ▰▰▰▰▱▱ against the 30-day signing window, colored
/// by urgency of what remains
fn bar_text(days: f64) -> (String, Color) {
    let filled = (days * 10.0 / 30.0).round().clamp(1.0, 10.0) as usize;
    let g = format!("{}{}", "▰".repeat(filled), "▱".repeat(10 - filled));
    (g, if days > 7.0 { GRN } else if days > 2.0 { YEL } else { RED })
}

/// paths under $HOME show with ~, the way a human reads them
fn tidy_path(s: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() && s.starts_with(&home) {
        format!("~{}", &s[home.len()..])
    } else {
        s.to_string()
    }
}

/// long blobs (anisette headers, hex, base64): preview only — the raw value
/// is machine data and useless wrapped in the panel
fn tidy_value(s: &str) -> String {
    let t = tidy_path(s);
    let n = t.chars().count();
    if n > 96 {
        format!("{}…", t.chars().take(96).collect::<String>())
    } else if n >= 64
        && t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=_-".contains(c))
    {
        format!("{}… ({} chars)", t.chars().take(16).collect::<String>(), n)
    } else {
        t
    }
}

/// bytes in human units
fn mb(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        None => "?".to_string(),
    }
}

/// march: results of installed/no-registry operations: uninstall/forget/sign
fn op_view(data: &Value) -> Vec<Line<'static>> {
    // {"ok":true,"uninstalled":"b"}
    if let Some(bundle) = data.get("uninstalled").and_then(Value::as_str) {
        return vec![Line::from(vec![
            Span::styled("  ✓ ", st(GRN)),
            Span::styled("uninstalled  ", st(GRAY)),
            Span::styled(bundle.to_string(), st(Color::White).add_modifier(Modifier::BOLD)),
        ])];
    }
    // {"status":"signed","output":"…ipa"} (o erro cai no corpo do if fora)
    if let Some(path) = data.get("output").and_then(Value::as_str) {
        if data.get("status").and_then(Value::as_str) == Some("signed") {
            return vec![Line::from(vec![
                Span::styled("  ✓ ", st(GRN)),
                Span::styled("assinado  ", st(GRAY)),
                Span::styled(tidy_path(path), st(YEL).add_modifier(Modifier::BOLD)),
            ])];
        }
    }
    // {"ok":bool,"bundle_id":"…"} (forget)
    if let Some(bundle) = data.get("bundle_id").and_then(Value::as_str) {
        if data.get("ok").and_then(Value::as_bool).is_some() {
            let ok = data.get("ok").and_then(Value::as_bool).unwrap_or(false);
            return vec![Line::from(vec![
                Span::styled(if ok { "  ✓ " } else { "  ✗ " }, st(if ok { GRN } else { YEL })),
                Span::styled(
                    if ok { "restituído do registro  " } else { "não está no registro  " },
                    st(GRAY),
                ),
                Span::styled(bundle.to_string(), st(Color::White)),
            ])];
        }
    }
    render_value(data, 0)
}

/// refresh/sideload: per-app outcome + summary
fn refresh_view(data: &Value) -> Vec<Line<'static>> {
    let Some(list) = data.get("refreshed").and_then(Value::as_array) else {
        return render_value(data, 0);
    };
    let mut out = vec![];
    for r in list {
        let bundle = r.get("bundle_id").and_then(Value::as_str).unwrap_or("?");
        match r.get("status").and_then(Value::as_str) {
            Some("installed") => out.push(Line::from(vec![
                Span::styled("  ✓ ", st(GRN)),
                Span::styled(bundle.to_string(), st(Color::White)),
                Span::styled(
                    match r.get("expires_at").and_then(Value::as_str) {
                        Some(at) if !at.is_empty() => format!("  expira {at}"),
                        _ => String::new(),
                    },
                    st(GRAY),
                ),
            ])),
            _ => {
                let err = r
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("falhou (sem detalhe)");
                out.push(Line::from(vec![
                    Span::styled("  ✗ ", st(RED)),
                    Span::styled(bundle.to_string(), st(Color::White)),
                    Span::styled(format!("  {err}"), st(GRAY)),
                ]));
            }
        }
    }
    if let Some(n) = data.get("count").and_then(Value::as_u64) {
        out.push(Line::from(Span::styled(format!("  {n} app(s)"), st(GRAY))));
    }
    if out.is_empty() {
        return vec![Line::from(Span::styled("· nothing to refresh", st(GRAY)))];
    }
    out
}

/// anisette: status dict or refresh-header dict
fn anisette_view(data: &Value) -> Vec<Line<'static>> {
    if let Some(map) = data.as_object() {
        // dict of header→blob (refresh): preview each value
        if map.keys().any(|k| k.starts_with("X-")) {
            return map
                .iter()
                .map(|(k, v)| {
                    Line::from(vec![
                        Span::styled(format!("  {k}"), st(CY)),
                        Span::styled(
                            format!("  {}", tidy_value(v.as_str().unwrap_or("?"))),
                            st(GRAY),
                        ),
                    ])
                })
                .collect();
        }
        if let Some(available) = data.get("available") {
            let ok = available.as_bool().unwrap_or(false);
            let provider = data.get("provider").and_then(Value::as_str).unwrap_or("?");
            let version = data
                .get("package_version")
                .and_then(Value::as_str)
                .unwrap_or("");
            let cached = data.get("state_cached").and_then(Value::as_bool).unwrap_or(false);
            let servers = data.get("servers").and_then(Value::as_array);
            let mut out = vec![Line::from(vec![
                Span::styled(if ok { "✓ " } else { "✗ " }, st(if ok { GRN } else { RED })),
                Span::styled(
                    if version.is_empty() {
                        "anisette".to_string()
                    } else {
                        format!("anisette {version}")
                    },
                    st(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  provider {provider}"),
                    st(CY),
                ),
                Span::styled(
                    if cached { "  · state em cache" } else { "  · sem state em cache" },
                    st(GRAY),
                ),
            ])];
            if let Some(list) = servers {
                if !list.is_empty() {
                    let names: Vec<String> = list
                        .iter()
                        .map(|s| s.as_str().unwrap_or("?").to_string())
                        .collect();
                    out.push(Line::from(Span::styled(
                        format!("  {} server(s): {}", list.len(), names.join(", ")),
                        st(GRAY),
                    )));
                }
            }
            return out;
        }
    }
    render_value(data, 0)
}

/// signed: count/directory/files with human sizes
fn signed_view(data: &Value) -> Vec<Line<'static>> {
    let count = data.get("count").and_then(Value::as_u64);
    if let Some(count) = count {
        let bytes = data.get("bytes").and_then(Value::as_u64);
        let mut out = vec![Line::from(vec![
            Span::styled("  ● ", st(CY)),
            Span::styled(
                format!("{count} ipa(s) · {}", mb(bytes)),
                st(Color::White).add_modifier(Modifier::BOLD),
            ),
        ])];
        if let Some(dir) = data.get("directory").and_then(Value::as_str) {
            out.push(Line::from(Span::styled(format!("  {}", tidy_path(dir)), st(CY))));
        }
        if let Some(files) = data.get("files").and_then(Value::as_array) {
            for f in files {
                let name = f.get("name").and_then(Value::as_str).unwrap_or("?");
                let size = f.get("bytes").and_then(Value::as_u64);
                let modified = f.get("modified").and_then(Value::as_str).unwrap_or("");
                out.push(Line::from(vec![
                    Span::styled(format!("  {name}"), st(Color::White)),
                    Span::styled(format!("  {}  {modified}", mb(size)), st(GRAY)),
                ]));
            }
        }
        return out;
    }
    render_value(data, 0)
}

/// aligned two-column key/value panel (device-info & friends)
fn kv_view(data: &Value) -> Vec<Line<'static>> {
    let Some(map) = data.as_object() else {
        return render_value(data, 0);
    };
    let width = map
        .keys()
        .map(|k| humanize(k).chars().count())
        .max()
        .unwrap_or(0)
        .min(24);
    let mut out: Vec<Line<'static>> = vec![];
    for (key, value) in map {
        let key_s = format!("{:>width$}  ", humanize(key), width = width);
        match value {
            // nested: indent, structured, no raw JSON string
            Value::Object(_) | Value::Array(_) => {
                out.push(Line::from(Span::styled(key_s.trim_end().to_string(), st(CY))));
                out.extend(render_value(value, 1));
            }
            other => {
                let shown = match other {
                    Value::Null => "—".to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::String(s) => tidy_value(s),
                    Value::Number(n) => n.to_string(),
                    other => compact(other),
                };
                let ink = match other {
                    Value::Bool(b) => st(if *b { GRN } else { RED }),
                    Value::Number(_) => st(YEL),
                    Value::Null | Value::String(_) => st(if other.is_null() { GRAY } else { Color::White }),
                    _ => st(Color::White),
                };
                out.push(Line::from(vec![
                    Span::styled(key_s, st(CY)),
                    Span::styled(shown, ink),
                ]));
            }
        }
    }
    out
}

/// accounts/slots summary: email + team table, or the generic fall-back
fn login_status_view(data: &Value) -> Vec<Line<'static>> {
    if let Some(state) = data.get("authenticated") {
        let authed = state.as_bool().unwrap_or(false);
        let email = data
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return vec![Line::from(vec![
            Span::styled(if authed { "✓ " } else { "✗ " }, st(if authed { GRN } else { RED })),
            Span::styled("signed in: ", st(GRAY)),
            Span::styled(email.to_string(), st(Color::White).add_modifier(Modifier::BOLD)),
        ])];
    }
    if let Some(list) = data.get("accounts").and_then(Value::as_array) {
        let mut out = vec![];
        for account in list {
            let email = account.get("email").and_then(Value::as_str).unwrap_or("?");
            let team = account.get("team_id").and_then(Value::as_str).unwrap_or("-");
            let active = account.get("active").and_then(Value::as_bool).unwrap_or(false);
            out.push(Line::from(vec![
                Span::styled(if active { "● " } else { "○ " }, st(if active { GRN } else { GRAY })),
                Span::styled(format!("{email:<34}"), st(Color::White)),
                Span::styled(format!("team {team}"), st(GRAY)),
            ]));
        }
        out.push(Line::from(Span::styled(
            format!("
  {} account(s)", list.len()),
            st(GRAY),
        )));
        return out;
    }
    render_value(data, 0)
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
    out.push(Line::from(vec![Span::styled(
        format!(" {mark} {overall} ", overall = overall.to_uppercase()),
        Style::new()
            .fg(if overall == "ok" { BG } else { Color::White })
            .bg(col)
            .add_modifier(Modifier::BOLD),
    )]));
    if let Some(checks) = data.get("checks").and_then(Value::as_array) {
        let mut counts = (0usize, 0usize, 0usize); // ok, warn, fail
        for check in checks {
            let name = check.get("name").and_then(Value::as_str).unwrap_or("?");
            let status = check.get("status").and_then(Value::as_str).unwrap_or("fail");
            let detail = check.get("detail").and_then(Value::as_str).unwrap_or("");
            let (mark, col) = status_ink(status);
            match status {
                "warn" => counts.1 += 1,
                "fail" | "expired" => counts.2 += 1,
                _ => counts.0 += 1,
            }
            out.push(Line::from(vec![
                Span::styled(format!("  {mark} "), st(col)),
                Span::styled(format!("{name:<22}"), st(Color::White)),
                Span::styled(detail.to_string(), st(GRAY)),
            ]));
        }
        out.push(Line::from(Span::styled(
            format!(
                "  {} ok · {} warn · {} fail",
                counts.0, counts.1, counts.2
            ),
            st(GRAY),
        )));
    }
    out
}

fn devices_view(data: &Value) -> Vec<Line<'static>> {
    let Some(list) = data.as_array() else {
        return render_value(data, 0);
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
            let is_usb = conn.eq_ignore_ascii_case("usb");
            let label = if is_usb { "USB" } else { "WIFI" };
            let id = d.get("device_id").map(Value::to_string).unwrap_or_default();
            Line::from(vec![
                Span::styled("  ● ", st(if is_usb { GRN } else { MAG })),
                Span::styled("iPhone", st(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {label} "), Style::new().fg(BG).bg(if is_usb { CY } else { MAG })),
                Span::styled(format!(" …{tail}  id {id}"), st(GRAY)),
            ])
        })
        .collect()
}

fn installs_view(data: &Value) -> Vec<Line<'static>> {
    let Some(list) = data.as_array() else {
        return render_value(data, 0);
    };
    if list.is_empty() {
        return vec![Line::from(Span::styled(
            "· nothing sideloaded yet",
            st(GRAY),
        ))];
    }
    let mut sorted: Vec<&Value> = list.iter().collect();
    // expiring/expired first, then by days left so the urgent ones lead
    // urgent first: expired as -1.0, then by days left ascending
    sorted.sort_by(|a, b| {
        let key = |r: &Value| {
            let expired = r.get("expired").and_then(Value::as_bool).unwrap_or(false);
            if expired {
                -1.0
            } else {
                r.get("days_left").and_then(Value::as_f64).unwrap_or(f64::INFINITY)
            }
        };
        key(a).total_cmp(&key(b))
    });
    sorted
        .into_iter()
        .map(|r| {
            let name = r.get("name").and_then(Value::as_str).unwrap_or("?");
            let bundle = r.get("bundle_id").and_then(Value::as_str).unwrap_or("?");
            let expired = r.get("expired").and_then(Value::as_bool).unwrap_or(false);
            let days = r.get("days_left").and_then(Value::as_f64);
            let (badge, col, gauge) = if expired {
                ("✗ expired".to_string(), RED, String::new())
            } else if let Some(d) = days {
                let (g, gcol) = bar_text(d);
                (format!("{d:.0}d left"), gcol, g)
            } else {
                ("? unknown".to_string(), GRAY, String::new())
            };
            let mut spans = vec![Span::styled("  ● ", st(col))];
            if gauge.is_empty() {
                // sem gauge: mantém a coluna do badge alinhada com as outras linhas
                spans.push(Span::raw("           "));
            } else {
                spans.push(Span::styled(gauge, st(col)));
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(badge, st(col)));
            spans.push(Span::styled(format!("  {name:<20.20}"), st(Color::White)));
            spans.push(Span::styled(bundle.to_string(), st(GRAY)));
            Line::from(spans)
        })
        .chain(std::iter::once(Line::from(Span::styled(
            format!(
                "  {} app{} · {} expired",
                list.len(),
                if list.len() == 1 { "" } else { "s" },
                list.iter()
                    .filter(|r| r.get("expired").and_then(Value::as_bool).unwrap_or(false))
                    .count()
            ),
            st(GRAY),
        ))))
        .collect()
}

// ---- header logo, half-block rendering ----
/// line-by-line gradient: rgb lerp between stops over the 12 pixel rows
/// light→dark green tones of the theme: the fade runs over several stops
/// so the transition appears smooth without muddying
const FADE_STOPS: [(u8, u8, u8); 3] =
    [(0x64, 0xE0, 0xA4), (0x24, 0xA0, 0x67), (0x02, 0x61, 0x48)];

/// gradient color of the pixel row: light green (top) → dark green (bottom)
fn row_fade(row: usize, rows: usize) -> Color {
    let span = rows.saturating_sub(1).max(1) as f32;
    let u = (row as f32 / span) * (FADE_STOPS.len() - 1) as f32;
    let i = (u as usize).min(FADE_STOPS.len() - 2);
    let t = (u - i as f32).clamp(0.0, 1.0);
    let (a, b) = (FADE_STOPS[i], FADE_STOPS[i + 1]);
    Color::Rgb(
        (f32::from(a.0) + (f32::from(b.0) - f32::from(a.0)) * t) as u8,
        (f32::from(a.1) + (f32::from(b.1) - f32::from(a.1)) * t) as u8,
        (f32::from(a.2) + (f32::from(b.2) - f32::from(a.2)) * t) as u8,
    )
}

fn logo_lines(word: &str) -> Vec<Line<'static>> {
    let g = logo::grid(word);
    let total = g.len();
    (0..total.div_ceil(2))
        .map(|pair| {
            let top_row = &g[pair * 2];
            let bot_row: Vec<bool> = g
                .get(pair * 2 + 1)
                .cloned()
                .unwrap_or_else(|| vec![false; top_row.len()]);
            let top_color = row_fade(pair * 2, total);
            let bot_color = row_fade(pair * 2 + 1, total);
            let mut spans = Vec::with_capacity(top_row.len());
            for (top, bottom) in top_row.iter().zip(bot_row.iter()) {
                if !top && !bottom {
                    spans.push(Span::raw(" "));
                } else {
                    spans.push(Span::styled(
                        "▀",
                        Style::new()
                            .fg(if *top { top_color } else { BG })
                            .bg(if *bottom { bot_color } else { BG }),
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
        .padding(ratatui::widgets::Padding::horizontal(1))
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

fn relative_selection(from: usize, dir: i32, count: usize) -> usize {
    let mut i = from;
    loop {
        if dir < 0 {
            i = i.saturating_sub(1);
        } else {
            i = (i + 1).min(count - 1);
        }
        if dir < 0 && i == 0 && !MENU[0].1.selectable() {
            return i;
        }
        if MENU[i].1.selectable() {
            return i;
        }
        if i == 0 || (dir > 0 && i == count - 1 && !MENU[i].1.selectable()) {
            return from;
        }
    }
}

/// open the .ipa picker for a command that takes a path
fn open_ipa_picker(app: &mut App, cmd: &'static str) {
    let found = ipa_candidates();
    if found.is_empty() {
        app.note("no .ipa in ~/Downloads, ~/Desktop, ~/Documents or cwd (set IPASIDE_IPA_DIRS to add dirs)");
    } else {
        app.dialog = Some(Dialog {
            title: format!("{cmd} which .ipa?"),
            kind: DialogKind::Pick {
                items: found
                    .into_iter()
                    .map(|p| {
                        let label = p.rsplit('/').next().unwrap_or(&p).to_string();
                        (label, Choice::RunWith(cmd.to_string(), p))
                    })
                    .collect(),
                sel: 0,
            },
        });
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
        Item::Run(cmd) => {
            let parts: Vec<String> = cmd.split_whitespace().map(String::from).collect();
            app.run(parts);
        }
        Item::PickIpa(cmd) => open_ipa_picker(app, cmd),
        Item::PickApp(action) => {
            app.app_action = Some(*action);
            app.run(vec!["installs".into()]);
        }
        Item::Login => start_login(app),
        Item::Header => {}
    }
}

fn start_login(app: &mut App) {
    open_input_dialog(app, "Apple ID e-mail:", false);
}

fn open_input_dialog(app: &mut App, prompt: &str, masked: bool) {
    app.dialog = Some(Dialog {
        title: prompt.to_string(),
        kind: DialogKind::Input {
            prompt: prompt.to_string(),
            masked,
            buffer: String::new(),
        },
    });
}

/// submit a text-input dialog: drives the login wizard stages
fn submit_input(app: &mut App, buffer: String) {
    let value = buffer.trim().to_string();
    let stage = match app.dialog.take() {
        Some(dialog) => dialog.title,
        None => return,
    };
    if value.is_empty() {
        return;
    }
    if stage.contains("e-mail") {
        let existed = app.login.is_some();
        open_input_dialog(app, "app-specific password:", true);
        app.login = Some(match app.login.take() {
            Some(flow) => LoginFlow { email: value, ..flow },
            None => LoginFlow {
                email: value,
                password: String::new(),
                creds: std::path::PathBuf::new(),
            },
        });
        let _notify = existed;
        return;
    }
    if stage.contains("password") {
        if let Some(flow) = app.login.as_mut() {
            flow.password = value;
            flow.creds = write_creds(&flow.email, &flow.password);
        }
        let email = app
            .login
            .as_ref()
            .map(|f| f.email.clone())
            .unwrap_or_default();
        let creds = app
            .login
            .as_ref()
            .map(|f| f.creds.display().to_string())
            .unwrap_or_default();
        app.run(vec![
            "login".into(),
            "--email".into(),
            email,
            "--creds-file".into(),
            creds,
        ]);
        return;
    }
    if stage.contains("2FA") {
        let (email, creds) = match app.login.as_ref() {
            Some(flow) => (
                flow.email.clone(),
                flow.creds.display().to_string(),
            ),
            None => return,
        };
        app.run(vec![
            "login".into(),
            "--email".into(),
            email,
            "--creds-file".into(),
            creds,
            "--code".into(),
            value,
        ]);
    }
}

/// apply the chosen dialog item and close it
fn confirm_dialog(app: &mut App) {
    if let Some(dialog) = app.dialog.take() {
        match dialog.kind {
            DialogKind::Pick { items, sel } => match items.into_iter().nth(sel) {

                Some((_, Choice::RunWith(cmd, arg))) => app.run(vec![cmd, arg]),
                Some((_, Choice::Uninstall(bundle))) => app.run(vec!["uninstall".into(), bundle]),
                Some((_, Choice::Refresh(bundle))) => app.run(vec!["refresh".into(), bundle]),
                Some((_, Choice::Forget(bundle))) => app.run(vec!["forget".into(), bundle]),
                _ => {}
            },
            _ => {}
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
                Constraint::Length(7),
                Constraint::Length(2),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
            // logo com 1 linha de respiro no topo; 2 linhas de gape antes do corpo
            let head_row: [Rect; 2] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(6),
            ])
            .areas(rows[0]);
            let header =
                Layout::horizontal([Constraint::Length(58), Constraint::Min(0)]).split(head_row[1]);
            let slot: [Rect; 3] = Layout::horizontal([
                Constraint::Length(2),
                Constraint::Length(56),
                Constraint::Min(0),
            ])
            .areas(header[0]);
            f.render_widget(Paragraph::new(logo_lines("openside")), slot[1]);
            // badges centrados na altura do logo (sem linha de estado — o footer mostra)
            let bands: [Rect; 3] = Layout::vertical([
                Constraint::Length(4),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .areas(head_row[1]);
            let n_devices = connected(&app.devices);
            let mut chips = vec![
                Span::styled("v0.1.0  ", st(GRAY)),
                Span::styled(" anx set ", Style::new().fg(DARK).bg(GRAY)),
                Span::raw(" "),
                Span::styled(format!(" engine {} ", ENGINE_VERSION.get().map(String::as_str).unwrap_or("…")), Style::new().fg(BG).bg(CY)),
                Span::raw(" "),
            ];
            if app.expired > 0 {
                chips.push(Span::styled(
                    format!(" ! {expired} expired ", expired = app.expired),
                    Style::new().fg(Color::White).bg(RED),
                ));
                chips.push(Span::raw(" "));
            }
            if app.expiring > 0 {
                chips.push(Span::styled(
                    format!(" ⚠ {exp} expiring ", exp = app.expiring),
                    Style::new().fg(DARK).bg(YEL),
                ));
                chips.push(Span::raw(" "));
            }
            chips.push(Span::styled(
                format!(" {dot} {n_devices} device{plural} ", dot = if n_devices > 0 { "●" } else { "○" }, plural = if n_devices == 1 { "" } else { "s" }),
                Style::new().fg(BG).bg(if n_devices > 0 { GRN } else { GRAY }),
            ));
            f.render_widget(
                Paragraph::new(Line::from(chips))
                    .alignment(ratatui::layout::Alignment::Right)
                    .style(Style::new().bg(BG)),
                bands[1],
            );

            let body = Layout::horizontal([Constraint::Length(32), Constraint::Min(0)])
                .split(rows[2]);
            let menu_cols: [Rect; 2] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(
                    box_title(f, body[0], "commands", CY),
                );
            hit_menu_list = menu_cols[0];
            let mut list = ListState::default();
            if MENU[app.selection].1.selectable() {
                list.select(Some(app.selection));
            }
            let items = MENU
                .iter()
                .map(|(name, item)| match item {
                    Item::Header => format!("  {name}"),
                    _ => format!(" ▪ {name}"),
                })
                .collect::<Vec<String>>();
            f.render_stateful_widget(
                List::new(items)
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
                let vis = log_inner.height.saturating_sub(1);
                let scroll = real_scroll(app.panel.len(), vis, log_stored, log_bottom);
                f.render_widget(
                    Paragraph::new(app.panel.clone())
                        .style(Style::new().bg(BG))
                        .wrap(ratatui::widgets::Wrap { trim: false })
                        .scroll((scroll, 0)),
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
                rows[3],
            );
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(FOOT, st(GRAY))))
                    .style(Style::new().bg(DARK)),
                rows[3],
            );
            let status_text = match (&app.spinner, &app.app_action) {
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
                rows[3],
            );

            // dialog overlay (last = on top)
            if let Some(dlog) = &app.dialog {
                match &dlog.kind {
                    DialogKind::Pick { items, .. } => {
                        let width = (57.min(area.width.saturating_sub(4))).max(20);
                        let height =
                            (items.len() as u16 + 5).min(rows[2].height.saturating_sub(2));
                        let x = area.x + (area.width.saturating_sub(width)) / 2;
                        let y = rows[2].y + (rows[2].height.saturating_sub(height)) / 2;
                        let inner = box_title(f, Rect { x, y, width, height }, &dlog.title, YEL);
                        hit_dialog_list = inner;
                        let sel_now = match &dlog.kind {
                            DialogKind::Pick { sel, .. } => *sel,
                            _ => 0,
                        };
                        let mut dsel = ListState::default();
                        dsel.select(Some(sel_now));
                        f.render_stateful_widget(
                            List::new(
                                items.iter().map(|(label, _)| label.clone()).collect::<Vec<String>>(),
                            )
                            .highlight_symbol("» ")
                            .highlight_style(st(Color::White).add_modifier(Modifier::BOLD)),
                            inner,
                            &mut dsel,
                        );
                    }
                    DialogKind::Input { prompt, masked, buffer } => {
                        let shown = if *masked && !buffer.is_empty() {
                            "●".repeat(buffer.len())
                        } else if buffer.is_empty() {
                            "▮".to_string()
                        } else {
                            format!("{buffer}▮")
                        };
                        let width = 46.min(area.width.saturating_sub(4)).max(24);
                        let height = 5.min(rows[2].height.saturating_sub(2));
                        let x = area.x + (area.width.saturating_sub(width)) / 2;
                        let y = rows[2].y + (rows[2].height.saturating_sub(height)) / 2;
                        let inner = box_title(f, Rect { x, y, width, height }, &dlog.title, YEL);
                        hit_dialog_list = inner;
                        let dims: [Rect; 2] =
                            Layout::vertical([Constraint::Length(1), Constraint::Min(0)])
                                .areas(inner);
                        f.render_widget(
                            Paragraph::new(Line::from(Span::styled(
                                format!("  {prompt}"),
                                st(GRAY).add_modifier(Modifier::DIM),
                            ))),
                            dims[0],
                        );
                        f.render_widget(
                            Paragraph::new(Line::from(Span::styled(shown, st(Color::White).add_modifier(Modifier::BOLD)))),
                            dims[1],
                        );
                    }
                }
            }
        })?;

        if !crossterm::event::poll(Duration::from_millis(50))? {
            continue;
        }
        let ev = crossterm::event::read()?;
        // dialog consumes navigation first
        if let Some(Dialog { title: _, kind }) = app.dialog.as_mut() {
            if let crossterm::event::Event::Key(key) = &ev {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    match kind {
                        DialogKind::Pick { items, sel } => match key.code {
                            crossterm::event::KeyCode::Up => *sel = sel.saturating_sub(1),
                            crossterm::event::KeyCode::Down => {
                                *sel = (*sel + 1).min(items.len().saturating_sub(1))
                            }
                            crossterm::event::KeyCode::PageUp => {
                                *sel = sel.saturating_sub(10)
                            }
                            crossterm::event::KeyCode::PageDown => {
                                *sel = (*sel + 10).min(items.len().saturating_sub(1))
                            }
                            crossterm::event::KeyCode::Esc => app.dialog = None,
                            crossterm::event::KeyCode::Enter => confirm_dialog(&mut app),
                            _ => {}
                        },
                        DialogKind::Input { buffer, .. } => match key.code {
                            crossterm::event::KeyCode::Esc => {
                                // abandon a wizard: nothing sent yet in this stage
                                if let Some(flow) = app.login.take() {
                                    drop_creds(&flow.creds);
                                }
                                app.dialog = None;
                            }
                            crossterm::event::KeyCode::Enter => {
                                let buffer = buffer.clone();
                                submit_input(&mut app, buffer);
                            }
                            crossterm::event::KeyCode::Backspace => {
                                buffer.pop();
                            }
                            crossterm::event::KeyCode::Char(c) => buffer.push(c),
                            _ => {}
                        },
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
                    crossterm::event::KeyCode::Up => {
                        app.selection = relative_selection(app.selection, -1, MENU.len())
                    }
                    crossterm::event::KeyCode::Down => {
                        app.selection = relative_selection(app.selection, 1, MENU.len())
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
                        if my >= hit_dialog_list.y
                            && my < hit_dialog_list.y + hit_dialog_list.height
                            && mx >= hit_dialog_list.x
                        {
                            if let Some(Dialog { kind, .. }) = app.dialog.as_mut() {
                                if let DialogKind::Pick { items, sel } = kind {
                                    let idx = (my - hit_dialog_list.y) as usize;
                                    let idx = idx.min(items.len().saturating_sub(1));
                                    if idx == *sel && last_click == Some((mx, my)) {
                                        confirm_dialog(&mut app);
                                        last_click = None;
                                    } else {
                                        *sel = idx;
                                        last_click = Some((mx, my));
                                    }
                                }
                            }
                        } else {
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
    fn logo_gradient_line_by_line() {
        // first row = light green top, last row = dark green bottom, no violet
        assert_eq!(row_fade(0, 12), GRN);
        let last = row_fade(11, 12);
        assert_eq!(last, Color::Rgb(0x02, 0x61, 0x48), "última linha chega no verde escuro, got {last:?}");
        let (a, m, b) = match row_fade(6, 12) {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => (0, 0, 0),
        };
        // mid entia: verde médio do tema (canal G domina, sem violeta)
        assert!(m > a && m > b, "meio é verde: {a} {m} {b}");
        assert!(a < 100 && b < 110, "meio não tem violeta nem vermelho");
    }

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
        let dialog = app_picker(&data, Choice::Uninstall).unwrap();
        let DialogKind::Pick { items, .. } = dialog.kind else {
            panic!("picker devolve DialogKind::Pick");
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(items[1].1, Choice::Uninstall(ref b) if b == "x.app"));
    }

    #[test]
    fn doctor_panel() {
        let d: Value = serde_json::from_str(
            r#"{"overall":"ok","checks":[{"name":"Python runtime","status":"ok","detail":"Python 3.14"},{"name":"AMDS","status":"fail","detail":"not found"}]}"#,
        )
        .unwrap();
        let view = doctor_view(&d);
        assert_eq!(view.len(), 4, "cabeçalho + 2 checks + rodapé");
        assert!(view[0].spans[0].content.contains("OK"), "badge de saúde");
        assert!(view[2].spans[0].content.contains("✗"), "check fail → ✗");
        assert!(view[3].spans[0].content.contains("1 ok"), "rodapé conta por status");
        assert!(view[3].spans[0].content.contains("1 fail"), "rodapé conta por status");
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
        assert_eq!(view.len(), 4, "3 apps + rodapé");
        assert!(view[0].spans[2].content.contains("expired"), "expirado vai pro topo");
        assert!(view[1].spans[1].content.contains("▰"), "gauge quando há dias");
        assert!(view[1].spans[3].content.contains("d left"));
        assert!(view[2].spans[3].content.contains("30d left"));
        assert!(view[2].spans[4].content.trim().chars().count() <= 20, "nome cortado a 20");
        assert!(view[3].spans[0].content.contains("3 apps"), "rodapé resume");
        assert!(view[3].spans[0].content.contains("1 expired"), "rodapé conta expirados");
    }

    #[test]
    fn humanize_keys() {
        assert_eq!(humanize("ios_version"), "iOS Version");
        assert_eq!(humanize("udid"), "UDID");
        assert_eq!(humanize("bundle_id"), "Bundle ID");
        assert_eq!(humanize("device_name"), "Device Name");
    }

    #[test]
    fn kv_view_aligned() {
        let d = serde_json::json!({"udid":"00008030","name":"iPhone","ios_version":"18.5"});
        let view = kv_view(&d);
        assert_eq!(view.len(), 3);
        // chave mais longa tem 11; demais linhas alinhadas a 11 + 2
        let first = view[0].spans[0].content.clone();
        assert_eq!(first.chars().count(), 13);
    }

    #[test]
    fn version_panel_and_login_accounts() {
        let v = command_view("version", &serde_json::json!({"version":"1.2.10"}));
        assert!(v[0].spans[0].content.contains("openside v0.1.0"));
        assert!(v[1].spans[0].content.contains("engine 1.2.10"));
        let acc = command_view("login", &serde_json::json!({"accounts":[{"email":"a@b","team_id":"T1","active":true}]}));
        assert!(acc[0].spans[0].content.contains("●"));
        let auth = command_view("login", &serde_json::json!({"authenticated":true,"email":"a@b"}));
        assert!(auth[0].spans[1].content.contains("signed in"));
    }

    #[test]
    fn devices_panel() {
        let empty = devices_view(&serde_json::json!([]));
        assert!(empty[0].spans[0].content.contains("no iPhone"));
        let d = serde_json::json!([{"serial":"00008030-0009454E0C84802E","connection_type":"USB","device_id":1}]);
        let rows = devices_view(&d);
        assert!(rows[0].spans[3].content.contains("802E"));
        assert!(rows[0].spans[0].content.contains("●"));
        assert!(rows[0].spans[2].content.contains("USB"), "chip de conexão");
    }

    #[test]
    fn app_picker_empty() {
        assert!(app_picker(&json!([]), Choice::Forget).is_none());
        assert!(app_picker(&json!(null), Choice::Forget).is_none());
    }

    #[test]
    fn refresh_and_op_views() {
        let r = command_view("refresh", &json!({"status":"done","count":2,"refreshed":[
            {"bundle_id":"a.b","status":"installed","expires_at":"2026-11-30"},
            {"bundle_id":"c.d","status":"error","error":"pairing lost"}]}));
        assert_eq!(r.len(), 3, "2 apps + rodapé");
        assert!(r[0].spans[0].content.contains("✓"));
        assert!(r[0].spans[2].content.contains("2026-11-30"));
        assert!(r[1].spans[0].content.contains("✗"));
        assert!(r[1].spans[2].content.contains("pairing lost"));
        let u = command_view("uninstall", &json!({"ok":true,"uninstalled":"x.app"}));
        assert!(u[0].spans[2].content.contains("x.app"));
        let forget = command_view("forget", &json!({"ok":false,"bundle_id":"x.app"}));
        assert!(forget[0].spans[1].content.contains("não está no registro"));
    }

    #[test]
    fn anisette_and_signed_views() {
        // dict de headers → preview truncado, não blob cru
        let h = command_view("anisette", &json!({"X-Apple-I-Machine-ID":"XXXXXXXXYFYYYYYYYYYbbbbbbbbbbbbbccccccccccccdddddddddddddeeeeeeeeeeeeeeee"}));
        assert!(h[0].spans[0].content.contains("X-Apple-I-Machine-ID"));
        assert!(h[0].spans[1].content.contains("…"), "blob truncado");
        assert!(h[0].spans[1].content.chars().count() < 40, "preview curto");
        let st = command_view("anisette", &json!({"available":true,"provider":"remote","package_version":"1.1.22","state_cached":true,"servers":["svc.example.com"]}));
        assert!(st[0].spans[1].content.contains("1.1.22"));
        assert!(st[1].spans[0].content.contains("svc.example.com"));
        let sg = command_view("signed", &json!({"count":2,"bytes":31457280,"directory":format!("{}/Documents/Signed", std::env::var("HOME").unwrap_or_default()),"files":[{"name":"a.ipa","bytes":31457280,"modified":"2026-11-01"}]}));
        assert!(sg[0].spans[1].content.contains("2 ipa(s) · 30.0 MB"));
        assert!(sg[1].spans[0].content.contains("~/Documents/Signed"), "caminho com ~");
    }

    #[test]
    fn tidy_paths_and_blobs() {
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(tidy_path(&format!("{home}/Signed/a.ipa")), "~/Signed/a.ipa");
        let hex64: String = "a1b2c3d4".repeat(10);
        let t = tidy_value(&hex64);
        assert!(t.contains('…') && t.chars().count() < 40, "blob → preview, got {t}");
        assert_eq!(tidy_value("curto"), "curto");
        assert!(!tidy_value("caminho/normal.ipa").contains('…'), "caminho comum intacto");
    }
}

