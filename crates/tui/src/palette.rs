//! Re-exports for the TUI palette of widgets (bars, blocks, lines).

use ratatui::style::{Color, Style};

pub fn style_fg(color: Color) -> Style {
    Style::default().fg(color)
}

pub fn style_bg_fg(bg: Color, fg: Color) -> Style {
    Style::default().bg(bg).fg(fg)
}
