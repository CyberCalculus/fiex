//! Application state and event loop glue.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fiex_engine::EngineHandle;
use ratatui::layout::Rect;
use tokio::sync::mpsc;

use crate::browser::{BrowserPane, PaneFocus};
use crate::dashboard::Dashboard;
use crate::input::Command;
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Browsing,
    /// Transfer in progress.
    Transferring,
    /// Command palette open.
    Palette,
    /// Quit requested.
    Quitting,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub ts: chrono::DateTime<chrono::Local>,
    pub level: fiex_engine::LogLevel,
    pub message: String,
}

const LOG_CAP: usize = 5000;
const REDRAW_CAP: Duration = Duration::from_millis(16); // ~60fps

pub struct App {
    pub theme: Theme,
    pub left: BrowserPane,
    pub right: BrowserPane,
    pub focus: PaneFocus,
    pub mode: AppMode,
    pub dashboard: Dashboard,
    pub logs: VecDeque<LogLine>,
    pub log_scroll: usize,
    pub palette_query: String,
    pub engine_handle: Option<EngineHandle>,
    pub area: Rect,
    pub dirty: bool,
    pub last_redraw: Instant,
}

impl App {
    pub fn new(theme: Theme, left_dir: PathBuf, right_dir: PathBuf) -> Self {
        Self {
            theme,
            left: BrowserPane::new(left_dir),
            right: BrowserPane::new(right_dir),
            focus: PaneFocus::Left,
            mode: AppMode::Browsing,
            dashboard: Dashboard::default(),
            logs: VecDeque::with_capacity(LOG_CAP),
            log_scroll: 0,
            palette_query: String::new(),
            engine_handle: None,
            area: Rect::default(),
            dirty: true,
            last_redraw: Instant::now(),
        }
    }

    pub fn focused_pane(&self) -> &BrowserPane {
        match self.focus {
            PaneFocus::Left => &self.left,
            PaneFocus::Right => &self.right,
        }
    }

    pub fn focused_pane_mut(&mut self) -> &mut BrowserPane {
        match self.focus {
            PaneFocus::Left => &mut self.left,
            PaneFocus::Right => &mut self.right,
        }
    }

    pub fn push_log(&mut self, level: fiex_engine::LogLevel, message: String) {
        if self.logs.len() == LOG_CAP {
            self.logs.pop_front();
        }
        self.logs.push_back(LogLine {
            ts: chrono::Local::now(),
            level,
            message,
        });
        self.dirty = true;
    }

    pub fn apply_event(&mut self, ev: fiex_engine::Event) {
        match &ev {
            fiex_engine::Event::Log { level, message } => {
                self.push_log(*level, message.clone());
            }
            _ => {}
        }
        self.dashboard.apply(&ev);
        self.dirty = true;
    }

    pub fn handle_command(&mut self, cmd: Command) {
        match (self.mode, cmd) {
            (AppMode::Palette, Command::PaletteInsert(c)) => {
                self.palette_query.push(c);
                self.dirty = true;
            }
            (AppMode::Palette, Command::PaletteBackspace) => {
                self.palette_query.pop();
                self.dirty = true;
            }
            (AppMode::Palette, Command::SubmitPalette) => {
                // For now: accept the query as a path and cd into it on the
                // focused pane. A future iteration can hook this up to a
                // proper fuzzy file jump.
                let p = self.palette_query.trim();
                if !p.is_empty() {
                    let path = std::path::PathBuf::from(p);
                    if path.is_dir() {
                        let pane = self.focused_pane_mut();
                        pane.current_dir = path;
                        pane.refresh();
                    }
                }
                self.palette_query.clear();
                self.mode = AppMode::Browsing;
                self.dirty = true;
            }
            (AppMode::Palette, Command::CancelPalette) => {
                self.palette_query.clear();
                self.mode = AppMode::Browsing;
                self.dirty = true;
            }
            (_, Command::OpenCommandPalette) => {
                self.mode = AppMode::Palette;
                self.palette_query.clear();
                self.dirty = true;
            }
            (_, Command::Quit) => {
                self.mode = AppMode::Quitting;
                if let Some(h) = &self.engine_handle {
                    h.cancel();
                }
            }
            (AppMode::Browsing, Command::MoveUp) => {
                self.focused_pane_mut().move_cursor(-1);
                self.dirty = true;
            }
            (AppMode::Browsing, Command::MoveDown) => {
                self.focused_pane_mut().move_cursor(1);
                self.dirty = true;
            }
            (AppMode::Browsing, Command::MoveTop) => {
                self.focused_pane_mut().jump_top();
                self.dirty = true;
            }
            (AppMode::Browsing, Command::MoveBottom) => {
                self.focused_pane_mut().jump_bottom();
                self.dirty = true;
            }
            (AppMode::Browsing, Command::Activate) => {
                self.focused_pane_mut().activate();
                self.dirty = true;
            }
            (AppMode::Browsing, Command::Parent) => {
                let pane = self.focused_pane_mut();
                if let Some(p) = pane.current_dir.parent() {
                    pane.current_dir = p.to_path_buf();
                    pane.refresh();
                }
                self.dirty = true;
            }
            (AppMode::Browsing, Command::ToggleSelect) => {
                self.focused_pane_mut().toggle_select();
                self.dirty = true;
            }
            (AppMode::Browsing, Command::ClearSelection) => {
                self.left.clear_selection();
                self.right.clear_selection();
                self.dirty = true;
            }
            (AppMode::Browsing, Command::FocusLeft) => {
                self.focus = PaneFocus::Left;
                self.dirty = true;
            }
            (AppMode::Browsing, Command::FocusRight) => {
                self.focus = PaneFocus::Right;
                self.dirty = true;
            }
            _ => {}
        }
    }

    pub fn should_redraw(&self) -> bool {
        self.dirty && self.last_redraw.elapsed() >= REDRAW_CAP
    }

    pub fn mark_redrawn(&mut self) {
        self.dirty = false;
        self.last_redraw = Instant::now();
    }

    pub fn set_area(&mut self, area: Rect) {
        if area != self.area {
            self.area = area;
            self.dirty = true;
        }
    }

    /// Spawn the engine runner in a tokio task and wire events into `tx`.
    pub fn spawn_transfer(
        &mut self,
        sources: Vec<PathBuf>,
        dest: PathBuf,
        mode: fiex_engine::TransferMode,
        config: fiex_engine::Config,
        tx: mpsc::UnboundedSender<fiex_engine::Event>,
    ) -> EngineHandle {
        let engine = fiex_engine::Engine::new(config).expect("validated config");
        let handle = engine.handle();
        self.engine_handle = Some(handle.clone());
        self.mode = AppMode::Transferring;
        tokio::spawn(async move {
            let _ = engine.run(sources, dest, mode, tx).await;
        });
        handle
    }
}
