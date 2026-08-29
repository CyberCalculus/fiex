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

use crate::progress::{install_ctrl_c_handler, interactive_prompt, Renderer};

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
    // 1. Build the engine and wire Ctrl-c to its cancel handle.
    let engine = Engine::new(cfg.clone())?;
    let handle = engine.handle();
    install_ctrl_c_handler(Arc::new(handle.clone()));

    // 2. Pick a prompt callback. The user only gets an interactive
    //    prompt when they explicitly picked --conflict prompt AND
    //    stdin is a TTY. Anything else (piped input, a non-prompt
    //    policy, or no TTY) gets the engine's default skip-with-log
    //    behavior so the run never blocks.
    let prompt = if matches!(cfg.conflict_policy, ConflictPolicy::Prompt)
        && std::io::stdin().is_terminal()
    {
        interactive_prompt()
    } else {
        Engine::default_prompt_skip()
    };

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
    let prompt_for_engine = prompt.clone();
    let engine_handle = tokio::spawn(async move {
        engine
            .run(
                engine_sources,
                engine_dest,
                mode,
                event_tx,
                prompt_for_engine,
            )
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
    use fiex_engine::FileOutcome;

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
    }
}
