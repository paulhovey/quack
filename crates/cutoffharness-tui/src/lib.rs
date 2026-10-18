//! Full-screen ratatui client (docs/plan/06-tui.md).
//!
//! The render loop merges terminal input, engine events and a 16 ms frame ticker.
//! Engine events only mark the UI dirty; frames are drawn on the tick, so streaming
//! never triggers a draw per delta.

mod app;
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

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EventStream};
use cutoffharness_config::Loaded;
use cutoffharness_core::EngineHandle;
use futures::StreamExt;
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
    let _ = crossterm::execute!(io::stdout(), DisableBracketedPaste);
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
                Ok(ev) => app.on_engine(ev),
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
        for op in app.take_ops() {
            if !engine.submit(op).await {
                return Ok(());
            }
        }
    }
    Ok(())
}
