//! Typed events emitted by the engine over the TUI channel.
//!
//! These are the only thing the TUI ever sees from the engine — the renderer
//! stays purely reactive.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Severity for log lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Per-file transfer outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileOutcome {
    Copied,
    Moved,
    Skipped,
    Resumed,
    Reflinked,
}

/// Aggregate progress snapshot — emitted frequently while a transfer is
/// active so the dashboard can render a live throughput / ETA.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub current_speed_bps: f64,
    pub eta: Option<Duration>,
}

impl Progress {
    pub fn fraction(&self) -> f64 {
        if self.bytes_total == 0 {
            1.0
        } else {
            (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0)
        }
    }
}

/// The full event enum the engine pushes to the TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// Engine started, here's how many files / bytes it expects to handle.
    Started { files_total: u64, bytes_total: u64 },

    /// A file is about to be transferred. Emitted exactly once per file.
    FileStarted {
        index: u64,
        source: PathBuf,
        destination: PathBuf,
        bytes: u64,
    },

    /// Progress tick (bytes_done changed within a file). The TUI coalesces
    /// these — it does not have to redraw on every one.
    Progress(Progress),

    /// A file finished. `bytes` is the actual size transferred (post-rename
    /// and metadata-restored). For resumed transfers it is the total size
    /// of the file, not just the bytes copied in this session.
    FileCompleted {
        index: u64,
        outcome: FileOutcome,
        source: PathBuf,
        destination: PathBuf,
        bytes: u64,
        elapsed: Duration,
    },

    /// A non-fatal error happened for a particular file. The engine keeps
    /// going with the rest of the plan.
    FileError {
        source: PathBuf,
        destination: PathBuf,
        message: String,
    },

    /// A log line for the status pane.
    Log { level: LogLevel, message: String },

    /// Engine finished — `success` is false if any non-fatal errors were
    /// collected. `errors` is the count of files that failed.
    Done { success: bool, errors: u64 },
}
