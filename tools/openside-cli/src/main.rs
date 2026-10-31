// OpenSide TUI — ratatui front-end for the Python engine over NDJSON.
//
// All signing/insta logic lives in `ipaside_engine serve`; this binary only
// renders, collects keys/mouse and streams engine frames.

mod logo;

use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListState, Paragraph};
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
        let mut child = Command::new(python)
            .args(["-m", "ipaside_engine", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().expect("engine stdout");
        let (tx, rx) = mpsc::channel();
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
                Style::new().fg(GRAY),
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
        self.push(Line::from(Span::styled(text.into(), Style::new().fg(GRAY))));
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
            Some("ready") => self.note(format!(
                "engine ready · v{}",
                frame.get("version").cloned().unwrap_or(Value::Null)
            )),
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
                        Span::styled(":", Style::new().fg(GRAY)),
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
            Style::new().fg(GRAY),
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
    Span::styled(format!("{head}{text}"), Style::new().fg(color))
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
            let chunks = Layout::vertical([
                Constraint::Length(LOGO_HEIGHT as u16),
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

            // header: logo + menu side by side
            let header = Layout::horizontal([Constraint::Length(60), Constraint::Min(0)])
                .split(chunks[0]);
            let logo = logo_lines("openside");
            f.render_widget(Paragraph::new(logo), header[0]);

            // menu + log
            let body = Layout::horizontal([Constraint::Length(28), Constraint::Min(0)])
                .split(chunks[1]);
            f.render_stateful_widget(
                List::new(MENU.iter().map(|m| m.to_string()))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_set(border::ROUNDED)
                            .border_style(Style::new().fg(GRAY))
                            .title(Span::styled("menu", Style::new().fg(CY))),
                    )
                    .highlight_symbol(FIELD_ARROW)
                    .highlight_style(Style::new().fg(Color::White).add_modifier(Modifier::BOLD)),
                body[0],
                &mut list,
            );
            let log_area = body[1];
            let scroll = app.scroll.clamp(
                0,
                app.log.iter().len().saturating_sub(1).min(u16::MAX as usize) as u16,
            );
            f.render_widget(Paragraph::new(app.log.clone()).scroll((scroll, 0)), log_area);

            // input
            let input_spans = vec![
                Span::styled("> ", Style::new().fg(CY)),
                Span::raw(app.input.clone()),
            ];
            f.render_widget(
                Paragraph::new(Line::from(input_spans)).style(Style::new().bg(BG)),
                chunks[2],
            );

            // status
            let status = match (&app.spinner, app.pending) {
                (Some(line), _) => Line::from(Span::styled(format!("{line}…"), Style::new().fg(YEL))),
                (None, Some(id)) => Line::from(Span::styled(
                    format!("waiting for {id}…"),
                    Style::new().fg(GRAY),
                )),
                _ => Line::from(Span::styled(
                    format!("ready · {} connected", connected(&app.devices)),
                    Style::new().fg(GRAY),
                )),
            };
            f.render_widget(Paragraph::new(status), chunks[3]);
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

const LOGO_HEIGHT: usize = 4;
const FIELD_ARROW: &str = "» ";

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
