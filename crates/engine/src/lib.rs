//! `fiex-engine` — async, secure file transfer engine for fiex.
//!
//! The engine is UI-agnostic. It walks the source tree, plans operations,
//! performs atomic, checksum-verified copies (or moves), and emits a stream
//! of [`Event`]s over a channel that the CLI's progress renderer consumes.
//!
//! Highlights:
//!  - Streaming producer/consumer pipeline (no full file list before transfer)
//!  - `.tmp` sibling + atomic rename — never half-written files on crash
//!  - Resume by re-using a `.tmp` (verifies the kept prefix against the source
//!    before continuing, so a corrupted temp triggers a clean restart)
//!  - BLAKE3 checksums for integrity, streamed through the same I/O pass
//!  - Cross-filesystem CoW via `copy_file_range` / `ficlone` on Linux + Android
//!    with a buffered fallback
//!  - Path canonicalization + symlink confinement to prevent traversal
//!
//! The engine has zero dependencies on any TUI crate, so it can be
//! unit-tested headlessly.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]

pub mod checksum;
pub mod config;
pub mod engine;
pub mod error;
pub mod event;
pub mod metadata;
pub mod policy;
pub mod scan;
pub mod transfer;

pub use config::{Config, VerifyMode};
pub use engine::{Engine, EngineHandle, Plan, TransferMode};
pub use error::{EngineError, EngineResult};
pub use event::{Event, FileOutcome, LogLevel, Progress};
pub use policy::{ConflictPolicy, SymlinkPolicy};
