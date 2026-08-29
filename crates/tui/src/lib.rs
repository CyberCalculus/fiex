//! `fiex-tui` — ratatui frontend for the fiex engine.
//!
//! Architecture: a pure consumer of the engine's `Event` stream. The
//! transfer logic lives in `fiex-engine`; the TUI is just a renderer +
//! keyboard handler. That makes the engine fully testable headlessly and
//! keeps the UI reactive — no business logic in here.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod app;
pub mod browser;
pub mod dashboard;
pub mod input;
pub mod palette;
pub mod render;
pub mod sparkline;
pub mod theme;

pub use app::{App, AppMode};
pub use theme::Theme;
