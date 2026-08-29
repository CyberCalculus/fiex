//! High-level engine: scan → plan → transfer → emit events.
//!
//! The engine is constructed once with a [`Config`] and then driven via
//! [`Engine::run`]. It does not own a UI; it pushes events through the
//! `events` channel the caller passed in.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::error::{EngineError, EngineResult};
use crate::event::{Event, FileOutcome, LogLevel, Progress};
use crate::metadata::{copy_xattrs, MetadataSnapshot};
use crate::policy::{PromptDecision, ResolvedTarget, SymlinkPolicy};
use crate::scan::{scan_tree, ItemKind, ScanOptions, ScannedItem};
use crate::transfer::{copy_file_with_progress, copy_symlink, move_file, CopyOutcome};

/// Callback used to resolve a destination conflict when the
/// configured [`ConflictPolicy`] is [`ConflictPolicy::Prompt`].
///
/// The callback is invoked synchronously inside a worker thread, so
/// it must not block on anything other than the user (no async,
/// no `tokio::sync::Mutex`). The CLI's default impl reads a line
/// from stdin; the engine's default impl skips the conflict with
/// a log line.
pub type PromptCallback = Arc<dyn Fn(&Path, &Path) -> PromptDecision + Send + Sync>;

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

    /// Build the default non-interactive prompt callback: any
    /// `Prompt` conflict is logged and skipped. Use this when stdin
    /// isn't a TTY, or when the user passed `--conflict` something
    /// other than `prompt`.
    pub fn default_prompt_skip() -> PromptCallback {
        Arc::new(|_src, _dst| PromptDecision::Skip)
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
    ///
    /// `prompt` is invoked for every destination conflict when the
    /// configured [`ConflictPolicy`] is [`ConflictPolicy::Prompt`].
    /// Pass `Arc::new(|_, _| PromptDecision::Skip)` (the
    /// `Engine::default_prompt_skip` helper) for non-interactive
    /// runs.
    pub async fn run(
        self,
        sources: Vec<PathBuf>,
        dest_root: PathBuf,
        mode: TransferMode,
        events: mpsc::UnboundedSender<Event>,
        prompt: PromptCallback,
    ) -> EngineResult<()> {
        // Move fast-path: a single source on the same filesystem as
        // the destination can be moved by a single rename(2) — same
        // semantics as GNU `mv` for two-arg same-FS moves. The
        // per-file loop is only needed when the rename is impossible
        // (cross-FS, multiple sources, or dest has conflicting
        // content that policy needs to disambiguate).
        if matches!(mode, TransferMode::Move) && sources.len() == 1 {
            if let Some(outcome) =
                try_whole_tree_rename(&sources[0], &dest_root, &self.cancel, &events)
            {
                let success = !matches!(outcome, Err(EngineError::Cancelled));
                let _ = events.send(Event::Done { success, errors: 0 });
                return outcome;
            }
        }

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
        // 0 = no "all" yet, 1 = the user picked "all" → every remaining
        // Prompt conflict becomes Overwrite. Set by the prompt callback
        // through this shared atomic; read by every worker before asking.
        let prompt_all = Arc::new(AtomicI8::new(0));
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
            let prompt = prompt.clone();
            let prompt_all = prompt_all.clone();
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
                        &prompt,
                        &prompt_all,
                    ) {
                        errors.fetch_add(1, Ordering::SeqCst);
                        let _ = ev.send(Event::FileError {
                            source: entry.source.clone(),
                            destination: entry.destination.clone(),
                            message: e.to_string(),
                        });
                        // `Cancelled` is a user-initiated abort — propagate
                        // it to the engine-wide cancel flag so the rest
                        // of the workers bail out, and the caller sees
                        // `EngineError::Cancelled` instead of a silent
                        // "completed with errors" success=false.
                        if matches!(e, EngineError::Cancelled) {
                            cancel.store(true, Ordering::SeqCst);
                        }
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

        // In Move mode, per-file move_file() leaves empty source
        // directories behind (the workers only touch files). Walk the
        // source tree bottom-up and remove any directory that is now
        // empty. Files that the engine couldn't move (errors) are
        // still inside their parent, so the directory stays.
        if matches!(mode, TransferMode::Move) {
            prune_empty_source_dirs(&dirs, &self.cancel);
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

/// Walk the source directory entries bottom-up and `remove_dir` each
/// one that is now empty. Directories that still contain files (e.g.
/// the move failed for some of their children) are left in place —
/// `remove_dir` returns ENOTEMPTY in that case and we silently
/// swallow it.
fn prune_empty_source_dirs(dirs: &[PlanEntry], cancel: &Arc<AtomicBool>) {
    let mut sorted: Vec<&PlanEntry> = dirs.iter().collect();
    // Deepest paths first: longer path = deeper directory. Same-depth
    // entries are removed in any order — they don't contain each other.
    sorted.sort_by_key(|d| std::cmp::Reverse(d.source.as_os_str().len()));
    for d in sorted {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let _ = std::fs::remove_dir(&d.source);
    }
}

/// Move fast-path: try to satisfy a two-argument `fiex -m src dst` with
/// a single `rename(2)`, like GNU `mv` would. Emits the same
/// `Event::FileStarted` / `FileCompleted` / `FileError` stream the
/// per-file loop would have emitted, so the renderer still shows
/// progress lines.
///
/// Returns `Some(Ok(()))` on success, `Some(Err(e))` on a real error
/// (caller bails), or `None` if the fast path doesn't apply and the
/// per-file loop should take over.
fn try_whole_tree_rename(
    source: &Path,
    dest_root: &Path,
    cancel: &Arc<AtomicBool>,
    events: &mpsc::UnboundedSender<Event>,
) -> Option<EngineResult<()>> {
    use std::os::unix::fs::MetadataExt;

    // The fast path only applies when both sides exist as
    // directories on the same filesystem. We canonicalize the
    // source so we have a real inode; the destination is left as
    // the user wrote it.
    let canon_src = match std::fs::canonicalize(source) {
        Ok(p) => p,
        Err(_) => return None, // Source doesn't exist; let the per-file path report it.
    };
    let src_meta = match std::fs::symlink_metadata(&canon_src) {
        Ok(m) => m,
        Err(_) => return None,
    };
    if !src_meta.is_dir() {
        return None; // Single-file move is handled by move_file.
    }
    // The user might have passed a trailing-slashed dest; normalize.
    let dest_path = dest_root.to_path_buf();

    // GNU `mv` two-arg semantics: if `dest` is an existing directory,
    // move `src` *into* it (i.e. dest/src). Otherwise rename `src`
    // to `dest`. This is the rule — match it.
    let final_dest = if dest_path.exists() {
        let dm = match std::fs::symlink_metadata(&dest_path) {
            Ok(m) => m,
            Err(_) => return None,
        };
        if !dm.is_dir() {
            // mv refuses to move a directory onto a non-directory.
            return None;
        }
        // Same FS check.
        if dm.dev() != src_meta.dev() {
            return None;
        }
        let leaf = canon_src.file_name().unwrap_or(canon_src.as_os_str());
        dest_path.join(leaf)
    } else {
        if dest_path
            .parent()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.dev() != src_meta.dev())
            .unwrap_or(false)
        {
            return None;
        }
        dest_path.clone()
    };

    // Walk the source to emit the same progress events the per-file
    // loop would. We only need to count and name files; the actual
    // I/O is a single rename(2).
    let plan = match build_simple_plan(&canon_src, &final_dest) {
        Ok(p) => p,
        Err(_) => return None,
    };

    events
        .send(Event::Started {
            files_total: plan.files_total(),
            bytes_total: plan.bytes_total(),
        })
        .ok();

    // Emit a FileStarted + FileCompleted for every entry so the
    // renderer logs each one. The actual move is a single rename;
    // these events are advisory.
    for (i, entry) in plan.entries.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Some(Err(EngineError::Cancelled));
        }
        if !matches!(entry.kind, ItemKind::File) {
            continue;
        }
        let _ = events.send(Event::FileStarted {
            index: i as u64,
            source: entry.source.clone(),
            destination: entry.destination.clone(),
            bytes: entry.size,
        });
        let _ = events.send(Event::FileCompleted {
            index: i as u64,
            outcome: FileOutcome::Moved,
            source: entry.source.clone(),
            destination: entry.destination.clone(),
            bytes: entry.size,
            elapsed: Duration::from_millis(0),
        });
    }

    // The one syscall that does all of the work.
    if let Err(e) = std::fs::rename(&canon_src, &final_dest) {
        // Bail with an error so the per-file path isn't tried on a
        // half-renamed tree.
        let _ = events.send(Event::FileError {
            source: canon_src.clone(),
            destination: final_dest.clone(),
            message: e.to_string(),
        });
        return Some(Err(EngineError::io(&canon_src, e)));
    }

    Some(Ok(()))
}

/// Lightweight plan: walk `canon_src` and produce `(source, destination)`
/// pairs without re-running the full scanner. Used only by the move
/// fast-path.
fn build_simple_plan(canon_src: &Path, final_dest: &Path) -> EngineResult<Plan> {
    let mut entries = Vec::new();
    fn walk(src: &Path, dst: &Path, out: &mut Vec<PlanEntry>) -> EngineResult<()> {
        let md = std::fs::symlink_metadata(src).map_err(|e| EngineError::io(src, e))?;
        let ft = md.file_type();
        if ft.is_dir() {
            out.push(PlanEntry {
                source: src.to_path_buf(),
                destination: dst.to_path_buf(),
                kind: ItemKind::Dir,
                size: 0,
            });
            for entry in std::fs::read_dir(src).map_err(|e| EngineError::io(src, e))? {
                let entry = entry.map_err(|e| EngineError::io(src, e))?;
                let child_src = entry.path();
                let child_dst = dst.join(entry.file_name());
                walk(&child_src, &child_dst, out)?;
            }
        } else if ft.is_file() {
            out.push(PlanEntry {
                source: src.to_path_buf(),
                destination: dst.to_path_buf(),
                kind: ItemKind::File,
                size: md.len(),
            });
        } else if ft.is_symlink() {
            out.push(PlanEntry {
                source: src.to_path_buf(),
                destination: dst.to_path_buf(),
                kind: ItemKind::Symlink,
                size: 0,
            });
        }
        Ok(())
    }
    walk(canon_src, final_dest, &mut entries)?;
    Ok(Plan { entries })
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
    prompt: &PromptCallback,
    prompt_all: &Arc<AtomicI8>,
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
        ResolvedTarget::Prompt {
            source,
            destination,
        } => {
            // If the user already answered "all" earlier in this run,
            // every remaining conflict becomes an Overwrite. No more
            // prompts.
            let decision = if prompt_all.load(Ordering::SeqCst) != 0 {
                PromptDecision::Overwrite
            } else {
                prompt(&source, &destination)
            };
            match decision {
                PromptDecision::Overwrite => destination,
                PromptDecision::All => {
                    prompt_all.store(1, Ordering::SeqCst);
                    destination
                }
                PromptDecision::Skip => {
                    let _ = events.send(Event::Log {
                        level: LogLevel::Info,
                        message: format!("skip {}", source.display()),
                    });
                    return Ok(());
                }
                PromptDecision::Cancel => {
                    // Flip the engine-wide cancel flag so the workers
                    // and the producer bail out.
                    let cancel = events
                        .send(Event::Log {
                            level: LogLevel::Warn,
                            message: "cancelled by user at prompt".into(),
                        })
                        .ok();
                    let _ = cancel;
                    return Err(EngineError::Cancelled);
                }
            }
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
                .run(
                    vec![src_root],
                    dst_root,
                    TransferMode::Copy,
                    tx,
                    Engine::default_prompt_skip(),
                )
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
                .run(
                    vec![src_root],
                    dst_root,
                    TransferMode::Move,
                    tx,
                    Engine::default_prompt_skip(),
                )
                .await
        });

        while let Some(ev) = rx.recv().await {
            if matches!(ev, Event::Done { .. }) {
                break;
            }
        }
        h.await.unwrap().unwrap();

        // GNU mv semantics: when the destination is an existing
        // directory, the source is moved *under* it. So `a.bin` ends
        // up at dst/<src_basename>/a.bin.
        let src_basename = src.path().file_name().unwrap();
        assert!(!src.path().join("a.bin").exists());
        assert!(dst.path().join(src_basename).join("a.bin").exists());
    }

    /// Bug fix regression: a Move run over a nested source tree must
    /// leave the user with no source files AND no leftover empty
    /// source directories. Previously per-file `move_file` only
    /// renamed each file, so `src/sub/` stayed as an empty directory
    /// after the run.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn move_prunes_empty_source_directories() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        // Build a nested source tree.
        std::fs::create_dir_all(src.path().join("sub/deeper")).unwrap();
        write_random(&src.path().join("a.bin"), 1024);
        write_random(&src.path().join("sub/b.bin"), 2048);
        write_random(&src.path().join("sub/deeper/c.bin"), 512);

        let cfg = Config {
            buffer_size: 1024,
            parallelism: 2,
            verify: crate::config::VerifyMode::All,
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
                .run(
                    vec![src_root.clone()],
                    dst_root,
                    TransferMode::Move,
                    tx,
                    Engine::default_prompt_skip(),
                )
                .await
        });

        while let Some(ev) = rx.recv().await {
            if matches!(ev, Event::Done { .. }) {
                break;
            }
        }
        h.await.unwrap().unwrap();

        // GNU mv semantics: source goes *under* the destination.
        let moved_root = dst.path().join(src.path().file_name().unwrap());
        // No files left anywhere in the source.
        for p in [
            src.path().join("a.bin"),
            src.path().join("sub/b.bin"),
            src.path().join("sub/deeper/c.bin"),
        ] {
            assert!(!p.exists(), "source file should be gone: {p:?}");
        }
        // Every source directory is removed too (deepest first,
        // so each `remove_dir` sees an empty parent).
        for p in [
            src.path().join("sub/deeper"),
            src.path().join("sub"),
            src.path().join("a.bin").parent().unwrap().to_path_buf(),
        ] {
            assert!(!p.exists(), "empty source dir should be pruned: {p:?}");
        }
        // The dst tree is intact (under moved_root).
        assert!(moved_root.join("a.bin").exists());
        assert!(moved_root.join("sub/b.bin").exists());
        assert!(moved_root.join("sub/deeper/c.bin").exists());
    }

    /// GNU `mv` parity: a Move run with a single source directory on
    /// the same filesystem should be a single `rename(2)`. Verify the
    /// destination's files are the *same inodes* as the source's
    /// (rename preserves them) and the source is gone entirely.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn move_uses_whole_tree_rename_on_same_fs() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("sub")).unwrap();
        write_random(&src.path().join("a.bin"), 1024);
        write_random(&src.path().join("sub/b.bin"), 2048);

        let inodes_before: Vec<(std::path::PathBuf, u64)> = walk_inodes(src.path());

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
                .run(
                    vec![src_root.clone()],
                    dst_root,
                    TransferMode::Move,
                    tx,
                    Engine::default_prompt_skip(),
                )
                .await
        });
        while let Some(ev) = rx.recv().await {
            if matches!(ev, Event::Done { .. }) {
                break;
            }
        }
        h.await.unwrap().unwrap();

        // The src/ root is gone (rename moved the whole tree).
        assert!(!src.path().exists(), "source root should be gone");
        // The destination is a single renamed tree, not a copy.
        let inodes_after = walk_inodes(dst.path());
        assert_eq!(inodes_before.len(), inodes_after.len());
        for (i, (path_before, ino_before)) in inodes_before.iter().enumerate() {
            let (path_after, ino_after) = &inodes_after[i];
            assert_eq!(
                ino_before, ino_after,
                "inode mismatch for {path_before:?} vs {path_after:?}"
            );
        }
    }

    /// When the policy is `Prompt` and the user answers `Overwrite`
    /// for every conflict, the engine should overwrite an existing
    /// destination file with the source content.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prompt_callback_overwrite_resolves_conflict() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        write_random(&src.path().join("a.bin"), 2048);
        // Destination already has a same-named file with different
        // content — this is the conflict the prompt resolves.
        std::fs::write(dst.path().join("a.bin"), vec![0xAAu8; 4096]).unwrap();

        let cfg = Config {
            buffer_size: 1024,
            parallelism: 1,
            verify: crate::config::VerifyMode::All,
            conflict_policy: crate::policy::ConflictPolicy::Prompt,
            try_reflink: false,
            ..Config::default()
        };
        let engine = Engine::new(cfg).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let src_root = src.path().to_path_buf();
        let dst_root = dst.path().to_path_buf();
        // Always say "overwrite" via the prompt callback.
        let prompt: PromptCallback = std::sync::Arc::new(|_src, _dst| PromptDecision::Overwrite);
        let h = tokio::spawn(async move {
            engine
                .run(vec![src_root], dst_root, TransferMode::Copy, tx, prompt)
                .await
        });
        while let Some(ev) = rx.recv().await {
            if matches!(ev, Event::Done { .. }) {
                break;
            }
        }
        h.await.unwrap().unwrap();

        // The destination now matches the source.
        let src_bytes = std::fs::read(src.path().join("a.bin")).unwrap();
        let dst_bytes = std::fs::read(dst.path().join("a.bin")).unwrap();
        assert_eq!(src_bytes, dst_bytes);
        assert_eq!(dst_bytes.len(), 2048);
    }

    /// When the prompt callback returns `Skip`, the destination is
    /// left alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prompt_callback_skip_leaves_destination_alone() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        write_random(&src.path().join("a.bin"), 2048);
        let original = vec![0xAAu8; 4096];
        std::fs::write(dst.path().join("a.bin"), &original).unwrap();

        let cfg = Config {
            buffer_size: 1024,
            parallelism: 1,
            conflict_policy: crate::policy::ConflictPolicy::Prompt,
            try_reflink: false,
            ..Config::default()
        };
        let engine = Engine::new(cfg).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let src_root = src.path().to_path_buf();
        let dst_root = dst.path().to_path_buf();
        let prompt: PromptCallback = std::sync::Arc::new(|_src, _dst| PromptDecision::Skip);
        let h = tokio::spawn(async move {
            engine
                .run(vec![src_root], dst_root, TransferMode::Copy, tx, prompt)
                .await
        });
        while let Some(ev) = rx.recv().await {
            if matches!(ev, Event::Done { .. }) {
                break;
            }
        }
        h.await.unwrap().unwrap();

        let dst_bytes = std::fs::read(dst.path().join("a.bin")).unwrap();
        assert_eq!(
            dst_bytes, original,
            "destination must not be touched on skip"
        );
    }

    /// When the prompt callback returns `Cancel`, the engine returns
    /// `EngineError::Cancelled` and the rest of the plan is dropped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prompt_callback_cancel_aborts_run() {
        let src = tempdir().unwrap();
        let dst = tempdir().unwrap();
        write_random(&src.path().join("a.bin"), 2048);
        write_random(&src.path().join("b.bin"), 2048);
        std::fs::write(dst.path().join("a.bin"), vec![0xAAu8; 4096]).unwrap();

        let cfg = Config {
            buffer_size: 1024,
            parallelism: 1,
            conflict_policy: crate::policy::ConflictPolicy::Prompt,
            try_reflink: false,
            ..Config::default()
        };
        let engine = Engine::new(cfg).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let src_root = src.path().to_path_buf();
        let dst_root = dst.path().to_path_buf();
        let prompt: PromptCallback = std::sync::Arc::new(|_src, _dst| PromptDecision::Cancel);
        let result = engine
            .run(vec![src_root], dst_root, TransferMode::Copy, tx, prompt)
            .await;
        assert!(matches!(result, Err(EngineError::Cancelled)));
    }

    /// Collect `(relative_path, inode)` pairs for every regular file
    /// under `root`, sorted by relative path so the comparison in
    /// `move_uses_whole_tree_rename_on_same_fs` is stable.
    fn walk_inodes(root: &Path) -> Vec<(std::path::PathBuf, u64)> {
        use std::os::unix::fs::MetadataExt;
        let mut out = Vec::new();
        fn visit(
            root: &Path,
            cur: &Path,
            out: &mut Vec<(std::path::PathBuf, u64)>,
        ) -> std::io::Result<()> {
            for entry in std::fs::read_dir(cur)? {
                let entry = entry?;
                let p = entry.path();
                let md = entry.metadata()?;
                if md.is_file() {
                    let rel = p.strip_prefix(root).unwrap().to_path_buf();
                    out.push((rel, md.ino()));
                } else if md.is_dir() {
                    visit(root, &p, out)?;
                }
            }
            Ok(())
        }
        visit(root, root, &mut out).unwrap();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}
