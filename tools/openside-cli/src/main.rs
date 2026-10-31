// Opening cut of the OpenSide TUI.
//
// This is intentionally small: a process around the existing Python engine so we can
// get a first TUI running without touching any engine behavior. Every command is
// still the engine's own; all Rust does here is render, collect keystrokes and
// stream NDJSON from `ipaside_engine serve`.

use std::env;
use std::io::{self, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};
use ratatui::style::Stylize;
use ratatui::widgets::{Block, Borders, List};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

fn engine_python_path() -> Option<String> {
    if let Ok(explicit) = env::var("OPENSIDE_ENGINE_PYTHON") {
        if !explicit.is_empty() {
            return Some(explicit);
        }
    }
    // Prefer the repo venv so development "just works" from the project root.
    let local = "./src/iPASide.Engine/.venv/bin/python";
    if std::path::Path::new(local).exists() {
        return Some(local.to_string());
    }
    None
}

struct Engine {
    child: Child,
}

impl Engine {
    fn spawn() -> io::Result<(Engine, Receiver<Value>)> {
        let python = engine_python_path().unwrap_or_else(|| "python".into());
        let mut child = Command::new(python)
            .args(["-m", "ipaside_engine", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().expect("engine stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if line.is_empty() {
                            continue;
                        }
                        if let Ok(value) = serde_json::from_str::<Value>(&line) {
                            if tx.send(value).is_err() {
                                break;
                            }
                        }
                    }
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
        // Best effort: the engine exits on a `{"type":"shutdown"}` line.
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = writeln!(stdin, r#"{{"type":"shutdown"}}"#);
            let _ = stdin.flush();
        }
        self.child.kill().ok();
    }
}

/// The command set the TUI starts with. Each maps one-to-one onto engine commands
/// and is only a rendering hint; typing a raw command still works.
const MENU: &[&str] = &["doctor", "devices", "device-info", "installs", "signed"];

struct App {
    selection: usize,
    input: String,
    lines: Vec<String>,
    next_id: u64,
}

impl App {
    fn new() -> Self {
        Self {
            selection: 0,
            input: String::new(),
            lines: vec![
                "openside — tui booting".into(),
                "type a command or Enter to send the selected one".into(),
            ],
            next_id: 1,
        }
    }

    fn up(&mut self) {
        self.selection = self.selection.saturating_sub(1).max(0);
    }

    fn down(&mut self) {
        self.selection = (self.selection + 1).min(MENU.len() - 1);
    }

    fn dispatch_selected(&mut self) -> Option<Vec<String>> {
        let cmd = MENU.get(self.selection)?.to_string();
        self.next_id += 1;
        Some(vec![cmd])
    }

    fn dispatch_typed(&mut self) -> Option<Vec<String>> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input.clear();
        let args: Vec<String> = text.split_whitespace().map(str::to_string).collect();
        self.next_id += 1;
        Some(args)
    }

    fn note(&mut self, text: impl Into<String>) {
        self.lines.push(text.into());
    }

    fn handle_frame(&mut self, frame: Value) {
        match frame.get("type").and_then(Value::as_str) {
            Some("ready") => self.lines.push(format!("engine ready, version {}", frame["version"])),
            Some("progress") => {
                if let Some(line) = frame.get("line").and_then(Value::as_str) {
                    self.lines.push(format!("… {line}"));
                }
            }
            Some("result") => {
                let ok = frame.get("ok").and_then(Value::as_bool).unwrap_or(false);
                if ok {
                    self.lines.push(format!("result: {:?}", frame.get("data")));
                } else {
                    self.lines.push(format!("error: {:?}", frame.get("error")));
                }
            }
            _ => self.lines.push(format!("frame: {frame}")),
        }
        if self.lines.len() > 200 {
            self.lines.drain(0..self.lines.len() - 200);
        }
    }
}

fn main() -> io::Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let (mut engine, rx) = Engine::spawn()?;
    let mut app = App::new();
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    terminal.hide_cursor()?;

    loop {
        // Non-blocking event push from the engine, otherwise wait for a keystroke.
        if let Ok(frame) = rx.try_recv() {
            app.handle_frame(frame);
        }
        terminal.draw(|frame| {
            let area = frame.size();
            let block = Block::default().borders(Borders::ALL).title("openside");
            let inner_block = block.inner(area);
            frame.render_widget(block, area);
            let list = List::new(
                MENU.iter()
                    .enumerate()
                    .map(|(index, item)| item.to_string())
                    .collect::<Vec<String>>(),
            )
            .highlight_symbol("» ")
            .highlight_style(ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD));
            frame.render_stateful_widget(list, inner_block, &mut ratatui::widgets::ListState::default());
        })?;
        if crossterm::event::poll(Duration::from_millis(50))? {
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                match key.code {
                    crossterm::event::KeyCode::Up => app.up(),
                    crossterm::event::KeyCode::Down => app.down(),
                    crossterm::event::KeyCode::Enter => {
                        if let Some(args) = app.dispatch_typed().or_else(|| app.dispatch_selected()) {
                            let id = app.next_id - 1;
                            engine.send(id, &args)?;
                            app.note(format!("sent {id} {args:?}"));
                        }
                    }
                    crossterm::event::KeyCode::Char('q') => break,
                    crossterm::event::KeyCode::Esc => break,
                    _ => {}
                }
            }
        }
    }

    crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen)?;
    crossterm::terminal::disable_raw_mode()?;
    terminal.show_cursor()?;
    Ok(())
}
