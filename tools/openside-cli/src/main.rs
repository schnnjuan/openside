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
                    Value::Null => "—".to_string(),
                    Value::Bool(b) => b.to_string(),
                    other => compact(other),
                };
                vec![Line::from(vec![
                    Span::styled(format!("  {}", humanize(key)), st(CY)),
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
        "device-info" => kv_view(data),
        "version" => vec![
            Line::from(Span::styled(
                format!("tui {}", env!("CARGO_PKG_VERSION")),
                st(MAG).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!(
                    "engine {}",
                    match data.get("version").and_then(Value::as_str) {
                        Some(v) => v.to_string(),
                        None => data.to_string(),
                    }
                ),
                st(CY).add_modifier(Modifier::BOLD),
            )),
        ],
        // accounts/slots output is a dict of account/device summaries
        "login" if data.get("accounts").is_some() || data.get("authenticated").is_some() => {
            login_status_view(data)
        }
        _ => render_value(data, Color::White),
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

/// aligned two-column key/value panel (device-info & friends)
fn kv_view(data: &Value) -> Vec<Line<'static>> {
    let Some(map) = data.as_object() else {
        return render_value(data, Color::White);
    };
    let width = map
        .keys()
        .map(|k| humanize(k).chars().count())
        .max()
        .unwrap_or(0)
        .min(24);
    map.iter()
        .map(|(key, value)| {
            let shown = match value {
                Value::Null => "—".to_string(),
                Value::Bool(b) => b.to_string(),
                other => compact(other),
            };
            Line::from(vec![
                Span::styled(format!("{:>width$}  ", humanize(key), width = width), st(CY)),
                Span::styled(shown, st(Color::White)),
            ])
        })
        .collect()
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
    render_value(data, Color::White)
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
/// line-by-line gradient: rgb lerp between stops over the 12 pixel rows
fn mix(a: Color, b: Color, t: f32) -> Color {
    let (r1, g1, b1) = match a {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    let (r2, g2, b2) = match b {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0, 0, 0),
    };
    let t = t.clamp(0.0, 1.0);
    Color::Rgb(
        (r1 as f32 + (r2 as f32 - r1 as f32) * t) as u8,
        (g1 as f32 + (g2 as f32 - g1 as f32) * t) as u8,
        (b1 as f32 + (b2 as f32 - b1 as f32) * t) as u8,
    )
}

/// gradient color of the pixel row: CY → MAG → GRN piecewise
fn row_fade(row: usize, rows: usize) -> Color {
    let span = (rows - 1).max(1) as f32;
    let mid = span / 2.0;
    if row as f32 <= mid {
        mix(CY, MAG, row as f32 / mid)
    } else {
        mix(MAG, GRN, (row as f32 - mid) / (span - mid))
    }
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
        app.note("no .ipa in OPENSIDE_IPA_DIRS (default Downloads/Desktop/macos-vm/shared)");
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
                Constraint::Length(logo::SLOT_ROWS as u16 / 2 + 1),
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
            let header = Layout::horizontal([
                Constraint::Length(1),
                Constraint::Length(57),
                Constraint::Min(0),
            ])
            .split(rows[0]);
            f.render_widget(Paragraph::new(logo_lines("openside")), header[1]);
            let n_devices = connected(&app.devices);
            let busy = app.spinner.is_some() || app.app_action.is_some();
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("fix-it style sideloader · v0.1.0  ", st(GRAY)),
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
                header[2],
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
        // first row starts at CY, last row ends at GRN, mid drifts to MAG
        assert_eq!(row_fade(0, 12), CY);
        let last = row_fade(11, 12);
        assert_eq!(last, GRN, "última linha chega no verde, got {last:?}");
        let mid = row_fade(6, 12);
        let (r, _, b) = match mid {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => (0, 0, 0),
        };
        assert!(r > 130 && r < 200 && b > 170, "meio tende ao violeta: {mid:?}");
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
        assert!(v[0].spans[0].content.contains("tui 0.1.0"));
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
        assert!(rows[0].spans[2].content.contains("802E"));
        assert!(rows[0].spans[0].content.contains("●"));
    }

    #[test]
    fn app_picker_empty() {
        assert!(app_picker(&json!([]), Choice::Forget).is_none());
        assert!(app_picker(&json!(null), Choice::Forget).is_none());
    }
}
