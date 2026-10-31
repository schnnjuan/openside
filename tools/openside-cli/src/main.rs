// OpenSide TUI — ratatui front-end for the Python engine over NDJSON.
//
// All signing/insta logic lives in `ipaside_engine serve`; this binary only
// renders, collects keys/mouse and streams engine frames.

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

// ---- theme (physical ratscope palette, pixel theme) ----
const CY: Color = Color::Rgb(41, 173, 255);
const MAG: Color = Color::Rgb(255, 119, 168);
const GRN: Color = Color::Rgb(0, 228, 54);
const RED: Color = Color::Rgb(255, 0, 77);
const YEL: Color = Color::Rgb(255, 236, 39);
const GRAY: Color = Color::Rgb(131, 118, 156);
const BG: Color = Color::Rgb(16, 16, 26);

/// every painted cell carries the app bg so transparent terminals stay solid
fn st(fg: Color) -> Style {
    Style::new().fg(fg).bg(BG)
}

static ENGINE_VERSION: OnceLock<String> = OnceLock::new();

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
        let engine_dir = std::env::var("OPENSIDE_ENGINE_DIR").ok().filter(|d| {
            std::path::Path::new(d).join("ipaside_engine").is_dir()
        }).or_else(|| {
            ["./src/iPASide.Engine", "../src/iPASide.Engine"].iter()
                .find(|d| std::path::Path::new(d).join("ipaside_engine").is_dir())
                .map(|d| d.to_string())
        }).or_else(|| {
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

const MENU: &[&str] = &["doctor", "devices", "device-info", "installs", "signed", "version"];

struct App {
    selection: usize,
    input: String,
    log: Vec<Line<'static>>,
    scroll: u16,
    spinner: Option<String>,
    devices: Option<Value>,
    next_id: u64,
    pending: Option<u64>,
}

impl App {
    fn new() -> Self {
        Self {
            selection: 0,
            input: String::new(),
            log: vec![Line::from(Span::styled(
                "openside — type a command, arrows pick from the menu",
                st(GRAY),
            ))],
            scroll: 0,
            spinner: None,
            devices: None,
            next_id: 1,
            pending: None,
        }
    }

    fn push(&mut self, line: Line<'static>) {
        self.log.push(line);
        if self.log.len() > 1000 {
            self.log.drain(0..self.log.len() - 1000);
        }
        self.scroll = u16::MAX; // clamp below = stick to bottom
    }

    fn note(&mut self, text: impl Into<String>) {
        self.push(Line::from(Span::styled(text.into(), st(GRAY))));
    }

    fn dispatch(&mut self) -> Option<Vec<String>> {
        let args: Vec<String> = if self.input.trim().is_empty() {
            vec![MENU[self.selection].to_string()]
        } else {
            self.input.trim().split_whitespace().map(str::to_string).collect()
        };
        self.input.clear();
        self.next_id += 1;
        self.pending = Some(self.next_id);
        Some(args)
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
                self.pending = None;
                let id = frame.get("id").and_then(Value::as_u64).unwrap_or(0);
                if let Some(got) = self.pending {
                    if id != 0 && got != id {
                        return; // stale answer after a reset
                    }
                }
                let ok = frame
                    .get("ok")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let (mark, mark_color) = if ok { ("✓", GRN) } else { ("✗", RED) };
                self.push(Line::from(vec![
                    Span::styled(mark, Style::new().fg(mark_color)),
                    Span::raw(" "),
                ]));
                for line in render_value(
                    frame.get("data").or_else(|| frame.get("error")).unwrap_or(&Value::Null),
                    1,
                    if ok { Color::White } else { RED },
                ) {
                    self.push(line);
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
                self.push(Line::from(Span::styled(line.to_string(), st(RED))));
            }
            _ => {
                let text = serde_json::to_string(&frame).unwrap_or_default();
                self.note(format!("frame {text}"));
            }
        }
    }
}

// ---- render a JSON value as colored terminal lines ----
fn render_value(value: &Value, depth: usize, base: Color) -> Vec<Line<'static>> {
    let pad = "  ".repeat(depth);
    match value {
        Value::String(s) => vec![Line::from(format!("{pad}{s}"))],
        Value::Number(n) => vec![span_line(&pad, n, YEL).into()],
        Value::Bool(b) => vec![span_line(&pad, b, if *b { GRN } else { MAG }).into()],
        Value::Object(map) => map
            .iter()
            .flat_map(|(key, val)| match val {
                Value::Object(_) | Value::Array(_) => {
                    let mut lines = vec![Line::from(vec![
                        span_line(&pad, key, CY),
                        Span::styled(":", st(GRAY)),
                    ])];
                    lines.extend(render_value(val, depth + 1, base));
                    lines
                }
                Value::String(s) => vec![Line::from(vec![
                    span_line(&pad, key, CY),
                    span_line("  ", s.clone(), base),
                ])],
                other => vec![Line::from(vec![
                    span_line(&pad, key, CY),
                    span_line("  ", other.to_string(), base),
                ])],
            })
            .collect(),
        Value::Array(items) if items.is_empty() => vec![Line::from(format!("{pad}· none"))],
        Value::Array(items) => {
            let scalar = items.iter().all(|v| !matches!(v, Value::Object(_)));
            if scalar {
                vec![Line::from(format!(
                    "{pad}{}",
                    items
                        .iter()
                        .map(|v| scalar_str(v))
                        .collect::<Vec<_>>()
                        .join("  ")
                ))]
            } else {
                items
                    .iter()
                    .flat_map(|v| render_value(v, depth + 1, base))
                    .collect()
            }
        }
        Value::Null => vec![Line::from(Span::styled(
            format!("{pad}· empty"),
            st(GRAY),
        ))],
    }
}

fn scalar_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn span_line(head: &str, text: impl std::fmt::Display, color: Color) -> Span<'static> {
    Span::styled(format!("{head}{text}"), st(color))
}

// ---- header logo, half-block rendering ----
fn logo_lines(word: &str) -> Vec<Line<'static>> {
    let g = logo::grid(word);
    let ramp = [CY, CY, MAG, MAG, GRN, GRN];
    let base = Style::new();
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
                        base.fg(if *top { color } else { BG }).bg(if *bottom { color } else { BG }),
                    ));
                }
            }
            Line::from(spans)
        })
        .collect()
}

// ---- chrome (same grammar as ratscope: solid bg + pixel frame + inner title) ----
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

fn real_scroll(len: usize, height: u16, wanted: u16) -> u16 {
    let max = len.saturating_sub(height as usize) as u16;
    if wanted >= u16::MAX - 1 { max } else { wanted.min(max) }
}

// ---- draw ----
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
        let mut list = ListState::default();
        list.select(Some(app.selection));

        terminal.draw(|f| {
            let area = f.area();
            f.render_widget(Block::default().style(Style::new().bg(BG)), area);

            let rows = Layout::vertical([
                Constraint::Length(LOGO_HEIGHT as u16 + 1),
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
            let header = Layout::horizontal([Constraint::Length(LOGO_COLS + 2), Constraint::Min(0)])
                .split(rows[0]);
            f.render_widget(Paragraph::new(logo_lines("openside")), header[0]);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("version ", st(GRAY)),
                    Span::styled(ENGINE_VERSION.get().map(|s| s.as_str()).unwrap_or("…"), st(GRAY)),
                ]))
                .alignment(ratatui::layout::Alignment::Right)
                .style(Style::new().bg(BG)),
                header[1],
            );

            let body = Layout::horizontal([Constraint::Length(32), Constraint::Min(0)])
                .split(rows[1]);

            // menu
            let menu_inner = box_title(f, body[0], "engine", CY);
            let mut list = ListState::default();
            list.select(Some(app.selection));
            f.render_stateful_widget(
                List::new(
                    MENU.iter()
                        .map(|m| m.to_string())
                        .collect::<Vec<String>>(),
                )
                .highlight_symbol("» ")
                .highlight_style(st(Color::White).add_modifier(Modifier::BOLD)),
                menu_inner,
                &mut list,
            );

            // output
            let log_inner = box_title(f, body[1], "output", MAG);
            let vis = log_inner.height;
            let scroll = real_scroll(app.log.len(), vis.saturating_sub(1), app.scroll);
            f.render_widget(
                Paragraph::new(app.log.clone())
                    .style(Style::new().bg(BG))
                    .scroll((scroll, 0)),
                log_inner,
            );

            // input bar (solid darker chrome) with caret via terminal cursor
            f.render_widget(Block::default().style(Style::new().bg(Color::Rgb(10, 10, 15))), rows[2]);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" > ", st(CY).add_modifier(Modifier::BOLD)),
                    Span::styled(app.input.clone(), st(Color::White)),
                ]))
                .style(Style::new().bg(Color::Rgb(10, 10, 15))),
                rows[2],
            );

            // footer: hints left, status right
            f.render_widget(Block::default().style(Style::new().bg(Color::Rgb(10, 10, 15))), rows[3]);
            let status_text = match (&app.spinner, app.pending) {
                (Some(line), _) => format!("{line}…"),
                (None, Some(id)) => format!("waiting {id}…"),
                _ => format!("ready · {} connected", connected(&app.devices)),
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(FOOT, st(GRAY))))
                    .style(Style::new().bg(Color::Rgb(10, 10, 15))),
                rows[3],
            );
            let status_col = area.width.saturating_sub(status_text.len() as u16 + 2);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    status_text,
                    if app.spinner.is_some() { st(YEL) } else { st(GRN) },
                )))
                .style(Style::new().bg(Color::Rgb(10, 10, 15)))
                .alignment(ratatui::layout::Alignment::Right),
                rows[3],
            );
            let _ = status_col;

            // terminal caret sits inside the input bar
            f.set_cursor_position(ratatui::layout::Position::new(rows[2].x + 3 + app.input.len() as u16, rows[2].y));
        })?;

        if !crossterm::event::poll(Duration::from_millis(50))? {
            continue;
        }
        match crossterm::event::read()? {
            crossterm::event::Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Press => {
                match key.code {
                    crossterm::event::KeyCode::Up => app.selection = app.selection.saturating_sub(1),
                    crossterm::event::KeyCode::Down => {
                        app.selection = (app.selection + 1).min(MENU.len() - 1)
                    }
                    crossterm::event::KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(5),
                    crossterm::event::KeyCode::PageDown => app.scroll = app.scroll.saturating_add(5),
                    crossterm::event::KeyCode::Enter => {
                        if let Some(args) = app.dispatch() {
                            let id = app.pending.unwrap_or(0);
                            engine.send(id, &args)?;
                            app.note(format!("> {}", args.join(" ")));
                        }
                    }
                    crossterm::event::KeyCode::Backspace => {
                        app.input.pop();
                    }
                    crossterm::event::KeyCode::Esc => break,
                    crossterm::event::KeyCode::Char('c')
                        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        break
                    }
                    crossterm::event::KeyCode::Char(c) => app.input.push(c),
                    _ => {}
                }
            }
            crossterm::event::Event::Mouse(mouse) => match mouse.kind {
                crossterm::event::MouseEventKind::Down(_) => {}
                crossterm::event::MouseEventKind::ScrollUp => {
                    app.scroll = app.scroll.saturating_sub(3)
                }
                crossterm::event::MouseEventKind::ScrollDown => {
                    app.scroll = app.scroll.saturating_add(3)
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

fn connected(devices: &Option<Value>) -> usize {
    devices.as_ref().and_then(Value::as_array).map(|a| a.len()).unwrap_or(0)
}

const LOGO_HEIGHT: usize = 6;
const LOGO_COLS: u16 = 55;
const FOOT: &str = " ↑↓ menu · ⏎ run · ⇞⇟ scroll · esc quit ";

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn logo_gives_4_rows_and_ink() {
        let lines = logo_lines("openside");
        assert_eq!(lines.len(), logo::grid("openside").len() / 2);
        let styled = lines.iter().flat_map(|l| l.spans.clone()).count();
        assert!(styled > 50, "logo precisa ter ▀ desenhado");
    }

}
