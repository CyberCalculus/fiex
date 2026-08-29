//! High-level engine: scan → plan → transfer → emit events.
//!
//! The engine is constructed once with a [`Config`] and then driven via
//! [`Engine::run`]. It does not own a UI; it pushes events through the
//! `events` channel the caller passed in.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::error::{EngineError, EngineResult};
use crate::event::{Event, FileOutcome, LogLevel, Progress};
use crate::metadata::{copy_xattrs, MetadataSnapshot};
use crate::policy::{ResolvedTarget, SymlinkPolicy};
use crate::scan::{scan_tree, ItemKind, ScanOptions, ScannedItem};
use crate::transfer::{copy_file_with_progress, copy_symlink, move_file, CopyOutcome};

/// A pre-computed list of (source, destination) pairs for the engine to act
/// on. Useful for tests and for callers that want to preview a run.
#[derive(Debug, Clone)]
pub struct Plan {
    pub entries: Vec<PlanEntry>,
}

#[derive(Debug, Clone)]
pub struct PlanEntry {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub kind: ItemKind,
    pub size: u64,
}

impl Plan {
    pub fn bytes_total(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }
    pub fn files_total(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| matches!(e.kind, ItemKind::File))
            .count() as u64
    }
}

/// A handle to a running engine. Drop it to request cancellation.
#[derive(Clone, Debug)]
pub struct EngineHandle {
    cancel: Arc<AtomicBool>,
}

impl EngineHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// The engine.
#[derive(Debug)]
pub struct Engine {
    config: Config,
    cancel: Arc<AtomicBool>,
}

impl Engine {
    pub fn new(config: Config) -> EngineResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn handle(&self) -> EngineHandle {
        EngineHandle {
            cancel: self.cancel.clone(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Build a plan without transferring anything. The plan is a snapshot of
    /// the source tree at the time of the call.
    pub fn plan(&self, sources: &[PathBuf], dest_root: &Path) -> EngineResult<Plan> {
        let mut entries = Vec::new();
        for src in sources {
            let canon_root = canonical_root(src)?;
            let opts = ScanOptions {
                root: Arc::new(canon_root.clone()),
                symlinks: self.config.symlink_policy,
                forbid_symlink_escape: !self.config.allow_symlink_escape,
            };
            let (tx, rx) = crossbeam_channel::unbounded();
            let root_arc = Arc::new(canon_root.clone());
            let opts_cl = opts.clone();
            // Detached scan thread — its lifetime is bounded by `rx` being
            // consumed below; once we drop `rx`, the channel closes and
            // the scan thread exits on its own.
            std::thread::spawn(move || scan_tree(root_arc, opts_cl, tx));
            for msg in rx.iter() {
                let item = msg?;
                let dst = match compute_dst(&item, src, dest_root) {
                    Some(d) => d,
                    None => continue,
                };
                entries.push(PlanEntry {
                    source: item.source,
                    destination: dst,
                    kind: item.kind,
                    size: item.size,
                });
            }
        }
        Ok(Plan { entries })
    }

    /// Run the engine. This consumes the engine, drives the scan + transfer
    /// pipeline, and pushes events through `events`. Returns when the plan
    /// completes (or is cancelled).
    pub async fn run(
        self,
        sources: Vec<PathBuf>,
        dest_root: PathBuf,
        mode: TransferMode,
        events: mpsc::UnboundedSender<Event>,
    ) -> EngineResult<()> {
        let plan = self.plan(&sources, &dest_root)?;
        let plan_entries = plan.entries.clone();
        let total_files = plan.files_total();
        let total_bytes = plan.bytes_total();

        events
            .send(Event::Started {
                files_total: total_files,
                bytes_total: total_bytes,
            })
            .ok();

        let mut dirs: Vec<PlanEntry> = plan_entries
            .iter()
            .filter(|e| matches!(e.kind, ItemKind::Dir))
            .cloned()
            .collect();
        let files: Vec<PlanEntry> = plan_entries
            .iter()
            .filter(|e| matches!(e.kind, ItemKind::File))
            .cloned()
            .collect();
        let symlinks: Vec<PlanEntry> = plan_entries
            .iter()
            .filter(|e| matches!(e.kind, ItemKind::Symlink))
            .cloned()
            .collect();
        // Dirs in deterministic order so parents are created first.
        dirs.sort_by(|a, b| a.destination.cmp(&b.destination));

        // Create all directories first (cheap, mostly no I/O contention).
        for d in &dirs {
            if self.cancel.load(Ordering::SeqCst) {
                return Err(EngineError::Cancelled);
            }
            if let Err(e) = std::fs::create_dir_all(&d.destination) {
                let _ = events.send(Event::FileError {
                    source: d.source.clone(),
                    destination: d.destination.clone(),
                    message: e.to_string(),
                });
            }
        }

        // Then symlinks.
        for s in &symlinks {
            if self.cancel.load(Ordering::SeqCst) {
                return Err(EngineError::Cancelled);
            }
            if matches!(self.config.symlink_policy, SymlinkPolicy::Skip) {
                continue;
            }
            if let Err(e) = copy_symlink(&s.source, &s.destination) {
                let _ = events.send(Event::FileError {
                    source: s.source.clone(),
                    destination: s.destination.clone(),
                    message: e.to_string(),
                });
            }
        }

        // Stream files into a crossbeam channel; a pool of N
        // workers pulls from it. Bounded channel gives backpressure.
        let (file_tx, file_rx): (Sender<PlanEntry>, Receiver<PlanEntry>) =
            crossbeam_channel::bounded(self.config.parallelism.max(1) * 4);
        let cancel = self.cancel.clone();
        // The producer task below clones the sender; we drop our local
        // copy here so the channel closes once the producer finishes and
        // the workers have drained. Without this, the channel would stay
        // open for the rest of `run()` and workers would block on `recv()`
        // even after the producer is done.
        let producer_tx = file_tx.clone();
        drop(file_tx);

        // Producer task: pushes all files. If we're cancelled early, we
        // stop pushing and drop the sender so workers exit.
        let producer = {
            let cancel = cancel.clone();
            tokio::task::spawn_blocking(move || {
                for f in files {
                    if cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    if producer_tx.send(f).is_err() {
                        break;
                    }
                }
                drop(producer_tx);
            })
        };

        // Worker pool: each worker is a blocking task; inside each, we
        // pull from the shared crossbeam Receiver and process the file.
        let parallelism = self.config.parallelism.max(1);
        let cfg = self.config.clone();
        let ev = events.clone();
        let bytes_done = Arc::new(AtomicU64::new(0));
        let files_done = Arc::new(AtomicU64::new(0));
        let next_index = Arc::new(AtomicU64::new(0));
        let errors = Arc::new(AtomicU64::new(0));
        let started = Instant::now();
        let files_total = total_files;

        let mut worker_handles = Vec::new();
        for _ in 0..parallelism {
            let file_rx = file_rx.clone();
            let cancel = cancel.clone();
            let cfg = cfg.clone();
            let ev = ev.clone();
            let bytes_done = bytes_done.clone();
            let files_done = files_done.clone();
            let next_index = next_index.clone();
            let errors = errors.clone();
            let h = tokio::task::spawn_blocking(move || {
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    let entry = match file_rx.recv() {
                        Ok(e) => e,
                        Err(_) => break, // channel closed → all producers done
                    };
                    let index = next_index.fetch_add(1, Ordering::SeqCst);
                    if let Err(e) = process_file(
                        index,
                        &entry,
                        mode,
                        &cfg,
                        &ev,
                        &bytes_done,
                        &files_done,
                        files_total,
                        total_bytes,
                        started,
                    ) {
                        errors.fetch_add(1, Ordering::SeqCst);
                        let _ = ev.send(Event::FileError {
                            source: entry.source.clone(),
                            destination: entry.destination.clone(),
                            message: e.to_string(),
                        });
                    }
                }
            });
            worker_handles.push(h);
        }
        // Drop the original receiver so the producer closing its sender
        // causes all workers to see channel close.
        drop(file_rx);

        let _ = producer.await;
        for h in worker_handles {
            let _ = h.await;
        }

        let success = errors.load(Ordering::SeqCst) == 0 && !self.cancel.load(Ordering::SeqCst);
        let _ = events.send(Event::Done {
            success,
            errors: errors.load(Ordering::SeqCst),
        });
        if !success && self.cancel.load(Ordering::SeqCst) {
            return Err(EngineError::Cancelled);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    Copy,
    Move,
}

#[allow(clippy::too_many_arguments)]
fn process_file(
    index: u64,
    entry: &PlanEntry,
    mode: TransferMode,
    cfg: &Config,
    events: &mpsc::UnboundedSender<Event>,
    bytes_done: &Arc<AtomicU64>,
    files_done: &Arc<AtomicU64>,
    files_total: u64,
    total_bytes: u64,
    started: Instant,
) -> EngineResult<()> {
    if matches!(cfg.symlink_policy, SymlinkPolicy::Skip) && matches!(entry.kind, ItemKind::Symlink)
    {
        return Ok(());
    }

    // Resolve conflict on the destination.
    let target = match cfg
        .conflict_policy
        .resolve(&entry.source, &entry.destination)
    {
        ResolvedTarget::Overwrite { target } => target,
        ResolvedTarget::Skip => {
            let _ = events.send(Event::Log {
                level: LogLevel::Info,
                message: format!("skip {}", entry.source.display()),
            });
            return Ok(());
        }
        ResolvedTarget::RenameOld { target } => {
            if target.exists() {
                let side = crate::policy::unique_path(&target);
                let _ = std::fs::rename(&target, &side);
            }
            target
        }
        ResolvedTarget::RenameNew { target } => target,
        ResolvedTarget::Prompt { destination, .. } => {
            // Non-interactive run: Prompt policy means skip with a log
            // line. The CLI performs the interactive y/n/a prompt before
            // running the engine, so this branch only fires when the
            // user explicitly disabled prompting (e.g. non-TTY without
            // --prompt).
            let _ = events.send(Event::Log {
                level: LogLevel::Info,
                message: format!(
                    "prompt mode: skipping {} (no interactive prompt available)",
                    destination.display()
                ),
            });
            return Ok(());
        }
    };

    let _ = events.send(Event::FileStarted {
        index: files_done.load(Ordering::SeqCst),
        source: entry.source.clone(),
        destination: target.clone(),
        bytes: entry.size,
    });

    let snap = if cfg.preserve_metadata {
        MetadataSnapshot::capture(&entry.source).ok()
    } else {
        None
    };

    let started_file = Instant::now();
    // Bug 4: VerifyMode::Sample is now an actual per-file decision.
    let should_verify = cfg.verify.should_verify(index);
    // Bug 6: thread a progress callback so the per-file bar updates
    // live, not just at completion.
    let current_file_name = entry
        .source
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| entry.source.display().to_string());
    let ev_for_cb = events.clone();
    let bytes_done_for_cb = Arc::clone(bytes_done);
    let files_done_for_cb = Arc::clone(files_done);
    let total_bytes_for_cb = total_bytes;
    let started_for_cb = started;
    let mut on_progress = |written_this_file: u64| {
        // Note: bytes_done is only updated at file END (after this
        // callback returns), so we pass the in-flight "written_this_file"
        // through current_file_written; the renderer combines the two.
        let _ = ev_for_cb.send(Event::Progress(snapshot_progress_inflight(
            &bytes_done_for_cb,
            &files_done_for_cb,
            files_total,
            total_bytes_for_cb,
            started_for_cb,
            current_file_name.clone(),
            written_this_file,
            entry.size,
        )));
    };

    let outcome = match mode {
        TransferMode::Copy => copy_file_with_progress(
            &entry.source,
            &target,
            cfg.buffer_size,
            cfg.try_reflink,
            should_verify,
            &mut on_progress,
        )?,
        TransferMode::Move => move_file(
            &entry.source,
            &target,
            cfg.buffer_size,
            cfg.try_reflink,
            should_verify,
        )?,
    };
    let elapsed = started_file.elapsed();

    if let Some(snap) = snap {
        let _ = snap.restore(&target);
    }
    if cfg.preserve_xattrs {
        let _ = copy_xattrs(&entry.source, &target);
    }

    bytes_done.fetch_add(entry.size, Ordering::SeqCst);
    files_done.fetch_add(1, Ordering::SeqCst);
    let _ = events.send(Event::FileCompleted {
        index: files_done.load(Ordering::SeqCst) - 1,
        outcome: match outcome {
            CopyOutcome::Copied | CopyOutcome::AlreadyCurrent => match mode {
                TransferMode::Copy => FileOutcome::Copied,
                TransferMode::Move => FileOutcome::Moved,
            },
            CopyOutcome::Resumed => FileOutcome::Resumed,
            CopyOutcome::Reflinked => FileOutcome::Reflinked,
        },
        source: entry.source.clone(),
        destination: target.clone(),
        bytes: entry.size,
        elapsed,
    });
    let _ = events.send(Event::Progress(snapshot_progress(
        bytes_done,
        files_done,
        files_total,
        total_bytes,
        started,
    )));
    Ok(())
}

fn snapshot_progress(
    bytes_done: &Arc<AtomicU64>,
    files_done: &Arc<AtomicU64>,
    files_total: u64,
    total_bytes: u64,
    started: Instant,
) -> Progress {
    let bytes = bytes_done.load(Ordering::SeqCst);
    let files = files_done.load(Ordering::SeqCst);
    let elapsed = started.elapsed();
    let speed = if elapsed.as_secs_f64() > 0.0 {
        bytes as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };
    let remaining = total_bytes.saturating_sub(bytes);
    let eta = if speed > 0.0 {
        Some(Duration::from_secs_f64(remaining as f64 / speed))
    } else {
        None
    };
    Progress {
        bytes_done: bytes,
        bytes_total: total_bytes,
        files_done: files,
        files_total,
        current_speed_bps: speed,
        eta,
        current_file: None,
        current_file_written: None,
        current_file_total: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn snapshot_progress_inflight(
    bytes_done: &Arc<AtomicU64>,
    files_done: &Arc<AtomicU64>,
    files_total: u64,
    total_bytes: u64,
    started: Instant,
    current_file: String,
    current_file_written: u64,
    current_file_total: u64,
) -> Progress {
    let mut p = snapshot_progress(bytes_done, files_done, files_total, total_bytes, started);
    p.current_file = Some(current_file);
    p.current_file_written = Some(current_file_written);
    p.current_file_total = Some(current_file_total);
    p
}

fn canonical_root(src: &Path) -> EngineResult<PathBuf> {
    Ok(std::fs::canonicalize(src)?)
}

fn compute_dst(item: &ScannedItem, src_root: &Path, dest_root: &Path) -> Option<PathBuf> {
    let src_root = std::fs::canonicalize(src_root).ok()?;
    let rel = item.source.strip_prefix(&src_root).ok()?;
    Some(dest_root.join(rel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::FileOutcome;
    use std::io::Write;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    fn write_random(path: &Path, size: usize) {
        let mut f = std::fs::File::create(path).unwrap();
        let chunk: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
        let mut left = size;
        while left > 0 {
            let n = left.min(chunk.len());
            f.write_all(&chunk[..n]).unwrap();
            left -= n;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn engine_copies_recursively() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("sub/deeper")).unwrap();
        write_random(&src.path().join("a.bin"), 8192);
        write_random(&src.path().join("sub/b.bin"), 16384);
        write_random(&src.path().join("sub/deeper/c.bin"), 4096);

        let cfg = Config {
            buffer_size: 4096,
            parallelism: 2,
            verify: crate::config::VerifyMode::All,
            conflict_policy: crate::policy::ConflictPolicy::Overwrite,
            // Force a regular copy so the FileOutcome is deterministic
            // across filesystems (CI runners may support reflink).
            try_reflink: false,
            ..Config::default()
        };
        let engine = Engine::new(cfg).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let dst_root = dst.path().to_path_buf();
        let src_root = src.path().to_path_buf();
        let h = tokio::spawn(async move {
            engine
                .run(vec![src_root], dst_root, TransferMode::Copy, tx)
                .await
        });

        let mut completed = 0;
        let mut done_seen = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                Event::FileCompleted { outcome, .. } => {
                    assert_eq!(outcome, FileOutcome::Copied);
                    completed += 1;
                }
                Event::Done { success, .. } => {
                    assert!(success);
                    done_seen = true;
                }
                _ => {}
            }
        }
        h.await.unwrap().unwrap();
        assert_eq!(completed, 3);
        assert!(done_seen);

        // Verify the actual files exist and have the right content.
        assert_eq!(
            std::fs::read(dst.path().join("a.bin")).unwrap(),
            std::fs::read(src.path().join("a.bin")).unwrap()
        );
        assert_eq!(
            std::fs::read(dst.path().join("sub/b.bin")).unwrap(),
            std::fs::read(src.path().join("sub/b.bin")).unwrap()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn engine_moves_and_cleans_up_source() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        write_random(&src.path().join("a.bin"), 2048);

        let cfg = Config {
            buffer_size: 1024,
            parallelism: 1,
            conflict_policy: crate::policy::ConflictPolicy::Overwrite,
            try_reflink: false,
            ..Config::default()
        };
        let engine = Engine::new(cfg).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let dst_root = dst.path().to_path_buf();
        let src_root = src.path().to_path_buf();
        let h = tokio::spawn(async move {
            engine
                .run(vec![src_root], dst_root, TransferMode::Move, tx)
                .await
        });

        while let Some(ev) = rx.recv().await {
            if matches!(ev, Event::Done { .. }) {
                break;
            }
        }
        h.await.unwrap().unwrap();

        assert!(!src.path().join("a.bin").exists());
        assert!(dst.path().join("a.bin").exists());
    }
}
