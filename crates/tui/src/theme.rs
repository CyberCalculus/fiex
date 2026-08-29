//! Color theme for the TUI. Inspired by Catppuccin Mocha / Tokyo Night —
//! calm, low-saturation backgrounds with bright accent colors.

use ratatui::style::Color;

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub accent_alt: Color,
    pub success: Color,
    pub warn: Color,
    pub error: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub border: Color,
    pub border_focus: Color,
    pub progress_fill: Color,
    pub progress_track: Color,
    pub sparkline: Color,
    pub log_info: Color,
    pub log_warn: Color,
    pub log_error: Color,
}

impl Theme {
    pub fn catppuccin_mocha() -> Self {
        Self {
            name: "catppuccin-mocha".into(),
            bg: Color::Rgb(30, 30, 46),
            fg: Color::Rgb(205, 214, 244),
            dim: Color::Rgb(127, 132, 156),
            accent: Color::Rgb(137, 180, 250),
            accent_alt: Color::Rgb(245, 194, 231),
            success: Color::Rgb(166, 227, 161),
            warn: Color::Rgb(250, 179, 135),
            error: Color::Rgb(243, 139, 168),
            selection_bg: Color::Rgb(69, 71, 90),
            selection_fg: Color::Rgb(205, 214, 244),
            border: Color::Rgb(69, 71, 90),
            border_focus: Color::Rgb(137, 180, 250),
            progress_fill: Color::Rgb(137, 180, 250),
            progress_track: Color::Rgb(49, 50, 68),
            sparkline: Color::Rgb(180, 190, 254),
            log_info: Color::Rgb(180, 190, 254),
            log_warn: Color::Rgb(250, 179, 135),
            log_error: Color::Rgb(243, 139, 168),
        }
    }

    pub fn tokyo_night() -> Self {
        Self {
            name: "tokyo-night".into(),
            bg: Color::Rgb(26, 27, 38),
            fg: Color::Rgb(192, 202, 245),
            dim: Color::Rgb(86, 95, 137),
            accent: Color::Rgb(122, 162, 247),
            accent_alt: Color::Rgb(187, 154, 247),
            success: Color::Rgb(158, 206, 106),
            warn: Color::Rgb(224, 175, 104),
            error: Color::Rgb(247, 118, 142),
            selection_bg: Color::Rgb(41, 46, 66),
            selection_fg: Color::Rgb(192, 202, 245),
            border: Color::Rgb(41, 46, 66),
            border_focus: Color::Rgb(122, 162, 247),
            progress_fill: Color::Rgb(122, 162, 247),
            progress_track: Color::Rgb(33, 35, 49),
            sparkline: Color::Rgb(125, 207, 255),
            log_info: Color::Rgb(125, 207, 255),
            log_warn: Color::Rgb(224, 175, 104),
            log_error: Color::Rgb(247, 118, 142),
        }
    }

    pub fn by_name(name: &str) -> Self {
        match name {
            "tokyo-night" => Self::tokyo_night(),
            _ => Self::catppuccin_mocha(),
        }
    }
}
