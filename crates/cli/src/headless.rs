//! Headless runner — same engine, no TUI. Useful for CI / scripts.

use std::path::PathBuf;

use anyhow::Result;
use fiex_engine::{Config, Engine, Event, TransferMode};
use tokio::sync::mpsc;

pub async fn run(
    cfg: Config,
    sources: Vec<PathBuf>,
    dest: PathBuf,
    mode: TransferMode,
) -> Result<()> {
    let engine = Engine::new(cfg)?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let dest_clone = dest.clone();
    let handle = tokio::spawn(async move { engine.run(sources, dest_clone, mode, tx).await });

    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Started {
                files_total,
                bytes_total,
            } => {
                eprintln!(
                    "fiex: starting — {files_total} files, {} bytes",
                    bytes_total
                );
            }
            Event::FileCompleted {
                outcome,
                source,
                bytes,
                ..
            } => {
                eprintln!("fiex: {:?} {} ({} bytes)", outcome, source.display(), bytes);
            }
            Event::FileError {
                source, message, ..
            } => {
                eprintln!("fiex: error {}: {}", source.display(), message);
            }
            Event::Progress(p) => {
                if p.files_done % 16 == 0 || p.bytes_done == p.bytes_total {
                    eprintln!(
                        "fiex: {}/{} files, {}/{} bytes ({:.1} MB/s, ETA {:?})",
                        p.files_done,
                        p.files_total,
                        p.bytes_done,
                        p.bytes_total,
                        p.current_speed_bps / 1_000_000.0,
                        p.eta
                    );
                }
            }
            Event::Log { level, message } => {
                eprintln!("fiex: [{:?}] {}", level, message);
            }
            Event::Done { success, errors } => {
                if success {
                    eprintln!("fiex: done");
                } else {
                    eprintln!("fiex: completed with {errors} error(s)");
                }
            }
            _ => {}
        }
    }
    handle.await??;
    Ok(())
}
