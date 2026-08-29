//! The actual run path. The engine emits typed `Event`s and
//! `progress::Renderer` turns them into linear output. This module is just
//! the wiring: build the engine, handle Ctrl-c, drive the renderer, and
//! return an exit code.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use fiex_engine::{Config, ConflictPolicy, Engine, Event, TransferMode};
use tokio::sync::mpsc;

use crate::progress::{install_ctrl_c_handler, Renderer};

/// Run the engine, drive the renderer, return when the engine is done.
/// Returns a non-zero exit code if the engine reported any errors or the
/// user hit Ctrl-c (130, conventional SIGINT exit).
pub async fn run(
    cfg: Config,
    sources: Vec<PathBuf>,
    dest: PathBuf,
    mode: TransferMode,
    force_plain: bool,
) -> Result<i32> {
    // 1. If the user picked `Prompt` as the conflict policy on a TTY,
    //    remind them it's not yet wired up — the engine treats Prompt
    //    as "skip with a log line" so they'd silently make no progress.
    if matches!(cfg.conflict_policy, ConflictPolicy::Prompt) && std::io::stdin().is_terminal() {
        eprintln!(
            "fiex: conflict-policy=prompt is not yet wired into the runner; \
             the engine will skip conflicts and log them. Use --conflict \
             overwrite|skip|rename-old|rename-new for non-interactive use."
        );
    }

    // 2. Build the engine and wire Ctrl-c to its cancel handle.
    let engine = Engine::new(cfg)?;
    let handle = engine.handle();
    install_ctrl_c_handler(Arc::new(handle.clone()));

    // 3. Engine events flow into the renderer.
    let (event_tx, event_rx) = mpsc::unbounded_channel::<Event>();

    // 4. Render task.
    let renderer_sources = sources.clone();
    let renderer_dest = dest.clone();
    let renderer = Renderer::new(force_plain);
    let render_task = tokio::spawn(async move {
        renderer
            .drive(event_rx, &renderer_sources, &renderer_dest)
            .await
    });

    // 5. Engine task.
    let engine_dest = dest.clone();
    let engine_sources = sources.clone();
    let engine_handle = tokio::spawn(async move {
        engine
            .run(engine_sources, engine_dest, mode, event_tx)
            .await
    });

    let engine_result = engine_handle.await?;
    render_task.await??;

    // 6. Translate the engine result into an exit code. Ctrl-c is
    //    expected (130, conventional SIGINT).
    let errors = match &engine_result {
        Ok(_) => 0,
        Err(fiex_engine::EngineError::Cancelled) => 130,
        Err(e) => {
            eprintln!("fiex: engine error: {e}");
            1
        }
    };
    Ok(errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fiex_engine::FileOutcome;
    use std::path::PathBuf;

    #[test]
    fn file_outcome_has_all_variants() {
        // Compile-time check that all five variants exist.
        let _ = [
            FileOutcome::Copied,
            FileOutcome::Moved,
            FileOutcome::Skipped,
            FileOutcome::Resumed,
            FileOutcome::Reflinked,
        ];
        let _: PathBuf = PathBuf::from("/tmp");
    }
}
