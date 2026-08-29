//! TUI runner — owns the terminal and drives the ratatui event loop.
//!
//! The TUI crate owns the rendering. The CLI owns the tokio runtime and the
//! terminal bring-up/teardown.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event as CtEvent, EventStream, KeyEventKind};
use fiex_engine::{Config, Event, TransferMode};
use fiex_tui::{App, Theme};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::interval;

pub fn run(cfg: Config, sources: Vec<PathBuf>, dest: PathBuf, mode: TransferMode) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move { run_async(cfg, sources, dest, mode).await })
}

async fn run_async(
    cfg: Config,
    sources: Vec<PathBuf>,
    dest: PathBuf,
    mode: TransferMode,
) -> Result<()> {
    let theme = Theme::by_name(&cfg.theme);

    // Engine event channel.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();

    // Engine task.
    let engine_cfg = cfg.clone();
    let engine_dest = dest.clone();
    let engine_sources = sources.clone();
    let engine_event_tx = event_tx.clone();
    let engine_handle = tokio::spawn(async move {
        let engine = match fiex_engine::Engine::new(engine_cfg) {
            Ok(e) => e,
            Err(e) => {
                let _ = engine_event_tx.send(Event::Log {
                    level: fiex_engine::LogLevel::Error,
                    message: format!("engine init failed: {e}"),
                });
                return;
            }
        };
        let _ = engine
            .run(engine_sources, engine_dest, mode, engine_event_tx)
            .await;
    });

    // Terminal bring-up.
    let mut stdout = std::io::stdout();
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Event stream from crossterm.
    let mut input_stream = EventStream::new();
    // Tick timer for repaint.
    let mut tick = interval(Duration::from_millis(100));

    // Build the app.
    let left_dir = sources
        .first()
        .cloned()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let right_dir = dest.clone();
    let mut app = App::new(theme, left_dir, right_dir);

    // Let the user browse, then run when they hit Ctrl-r.
    loop {
        // Drain engine events.
        while let Ok(ev) = event_rx.try_recv() {
            app.apply_event(ev);
        }

        // Render if dirty (60fps cap).
        if app.should_redraw() {
            terminal.draw(|f| fiex_tui::render::draw(f, &mut app))?;
            app.mark_redrawn();
        }

        tokio::select! {
            _ = tick.tick() => {}
            maybe = input_stream.next() => {
                match maybe {
                    Some(Ok(CtEvent::Key(k))) if k.kind == KeyEventKind::Press => {
                        let cmd = fiex_tui::input::map_key(k);
                        if matches!(cmd, fiex_tui::input::Command::RunTransfer) {
                            let _handle = app.spawn_transfer(
                                sources.clone(),
                                dest.clone(),
                                mode,
                                cfg.clone(),
                                event_tx.clone(),
                            );
                            // handle stored on app
                        } else {
                            app.handle_command(cmd);
                        }
                    }
                    Some(Ok(CtEvent::Resize(_, _))) => {
                        // Terminal resize: just mark dirty and let draw re-layout.
                        app.dirty = true;
                    }
                    _ => {}
                }
            }
        }

        if matches!(app.mode, fiex_tui::AppMode::Quitting) {
            break;
        }
    }

    // Restore terminal.
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
    )?;
    terminal.show_cursor()?;

    // Wait for engine to finish.
    let _ = engine_handle.await;
    Ok(())
}
