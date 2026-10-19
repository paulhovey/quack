//! Full-screen ratatui client (docs/plan/06-tui.md).
//!
//! The render loop merges terminal input, engine events and a 16 ms frame ticker.
//! Engine events only mark the UI dirty; frames are drawn on the tick, so streaming
//! never triggers a draw per delta.

mod app;
mod charts;
mod commands;
mod fuzzy;
mod input;
mod markdown;
mod modal;
mod theme;
mod ui;
mod wrap;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    EventStream,
};
use futures::StreamExt;
use quack_config::Loaded;
use quack_core::EngineHandle;
use tokio::sync::broadcast::error::RecvError;

pub use app::{App, AppInfo};

const FRAME: Duration = Duration::from_millis(16);

/// `--trace-startup`: draw one frame, write the timings as JSON to `path`, and quit.
#[derive(Debug, Clone)]
pub struct StartupTrace {
    /// Taken at the top of `main`.
    pub started: Instant,
    pub config_loaded: Duration,
    pub path: PathBuf,
}

impl StartupTrace {
    fn write(&self) -> io::Result<()> {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        std::fs::write(
            &self.path,
            format!(
                "{{\"config_ms\":{:.2},\"first_frame_ms\":{:.2}}}\n",
                ms(self.config_loaded),
                ms(self.started.elapsed())
            ),
        )
    }
}

/// Run the TUI until the user quits. Restores the terminal on exit and on panic.
pub async fn run(
    mut engine: EngineHandle,
    config: Arc<Loaded>,
    trace: Option<StartupTrace>,
) -> io::Result<()> {
    let mut app = App::new(AppInfo::from_config(&config));
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(io::stdout(), EnableBracketedPaste);
    let result = event_loop(&mut terminal, &mut app, &mut engine, trace.as_ref()).await;
    let _ = crossterm::execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    engine.shutdown().await;
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    engine: &mut EngineHandle,
    trace: Option<&StartupTrace>,
) -> io::Result<()> {
    let mut input = EventStream::new();
    // `ui.mouse`: capture the mouse so the wheel scrolls; followed across `/reload`.
    let mut mouse = false;
    let mut tick = tokio::time::interval(FRAME);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    terminal.draw(|f| ui::draw(f, app))?;
    if let Some(t) = trace {
        return t.write();
    }

    while !app.should_quit() {
        tokio::select! {
            ev = input.next() => match ev {
                Some(Ok(ev)) => app.on_terminal(ev),
                Some(Err(e)) => return Err(e),
                None => break,
            },
            ev = engine.events.recv() => match ev {
                Ok(ev) => {
                    if matches!(ev, quack_protocol::Event::ConfigReloaded) {
                        app.reconfigure(AppInfo::from_config(&engine.config()));
                    }
                    app.on_engine(ev);
                }
                Err(RecvError::Lagged(n)) => tracing::warn!(skipped = n, "ui lagged behind engine events"),
                Err(RecvError::Closed) => break,
            },
            _ = tick.tick() => {
                app.on_tick();
                if app.take_dirty() {
                    terminal.draw(|f| ui::draw(f, app))?;
                }
            }
        }
        if app.wants_mouse() != mouse {
            mouse = app.wants_mouse();
            if mouse {
                crossterm::execute!(io::stdout(), EnableMouseCapture)?;
            } else {
                crossterm::execute!(io::stdout(), DisableMouseCapture)?;
            }
        }
        if let Some(text) = app.take_clipboard() {
            copy(terminal, &text)?;
        }
        for op in app.take_ops() {
            if !engine.submit(op).await {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Put text on the clipboard through the terminal (OSC 52), which works over SSH and needs
/// no clipboard library. Most current terminals support it; some ask the user first.
fn copy(terminal: &mut ratatui::DefaultTerminal, text: &str) -> io::Result<()> {
    use std::io::Write;
    let out = terminal.backend_mut();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod copy_tests {
    #[test]
    fn base64_matches_the_standard() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(super::base64(input.as_bytes()), want);
        }
    }
}
