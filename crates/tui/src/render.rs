//! Pure rendering: takes the app state and draws a frame.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Sparkline,
};
use ratatui::Frame;

use crate::app::{App, AppMode};
use crate::browser::{EntryKind, PaneFocus};
use crate::theme::Theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.set_area(area);

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // top bar
            Constraint::Min(8),     // browser (dual pane)
            Constraint::Length(8),  // dashboard
            Constraint::Min(4),     // log
            Constraint::Length(1),  // status / hints
        ])
        .split(area);

    draw_top_bar(f, app, outer[0]);
    draw_browser(f, app, outer[1]);
    draw_dashboard(f, app, outer[2]);
    draw_log(f, app, outer[3]);
    draw_status(f, app, outer[4]);

    if matches!(app.mode, AppMode::Palette) {
        draw_palette_overlay(f, app, area);
    }
}

fn draw_top_bar(f: &mut Frame, app: &App, area: Rect) {
    let title = Line::from(vec![
        Span::styled(" fiex ", Style::default().bg(app.theme.accent).fg(app.theme.bg)),
        Span::styled("  file-exchange ", Style::default().fg(app.theme.dim)),
    ]);
    let mode = match app.mode {
        AppMode::Browsing => "BROWSE",
        AppMode::Transferring => "TRANSFER",
        AppMode::Palette => "PALETTE",
        AppMode::Quitting => "QUIT",
    };
    let info = Line::from(vec![
        Span::styled(
            format!("  {} ", mode),
            Style::default().fg(app.theme.accent_alt),
        ),
        Span::styled(
            format!("{:?} ", app.focus),
            Style::default().fg(app.theme.dim),
        ),
    ]);
    let p = Paragraph::new(Line::from(vec![title, info]))
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(app.theme.border)),
        );
    f.render_widget(p, area);
}

fn draw_browser(f: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    draw_pane(f, app, PaneFocus::Left, chunks[0]);
    draw_pane(f, app, PaneFocus::Right, chunks[1]);
}

fn draw_pane(f: &mut Frame, app: &mut App, focus: PaneFocus, area: Rect) {
    let focused = app.focus == focus;
    let pane = match focus {
        PaneFocus::Left => &app.left,
        PaneFocus::Right => &app.right,
    };
    let border_color = if focused {
        app.theme.border_focus
    } else {
        app.theme.border
    };
    let title = format!(
        " {} ",
        pane.current_dir.display()
    );
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(app.theme.fg)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let entries = &pane.entries;
    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let is_cursor = i == pane.cursor;
            let (icon, color) = match e.kind {
                EntryKind::Dir => ("▸ ", app.theme.accent),
                EntryKind::Symlink => ("⤷ ", app.theme.accent_alt),
                EntryKind::File => ("  ", app.theme.fg),
                EntryKind::Parent => ("↑ ", app.theme.dim),
            };
            let selected = pane.selected.contains(&e.path);
            let line = Line::from(vec![
                Span::styled(icon, Style::default().fg(color)),
                Span::styled(
                    format!("{:<32}", truncate(&e.name, 32)),
                    Style::default().fg(if selected { app.theme.warn } else { app.theme.fg }),
                ),
                Span::styled(
                    if matches!(e.kind, EntryKind::File) {
                        human_bytes(e.size)
                    } else {
                        String::new()
                    },
                    Style::default().fg(app.theme.dim),
                ),
            ]);
            let style = if is_cursor {
                Style::default()
                    .bg(app.theme.selection_bg)
                    .fg(app.theme.selection_fg)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(pane.cursor));
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(app.theme.selection_bg)
                .fg(app.theme.selection_fg),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_dashboard(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(40), // bars
            Constraint::Min(30),    // sparkline + meta
        ])
        .margin(1)
        .split(area);

    let block = Block::default()
        .title(Span::styled(
            " Dashboard ",
            Style::default().fg(app.theme.fg),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let bars = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // overall label
            Constraint::Length(2), // overall gauge
            Constraint::Length(1), // current file label
            Constraint::Length(2), // current file gauge
            Constraint::Min(0),
        ])
        .split(inner);

    // Overall gauge
    let overall_pct = (app.dashboard.overall_fraction() * 100.0) as u16;
    let overall = Gauge::default()
        .gauge_style(
            Style::default()
                .fg(app.theme.progress_fill)
                .bg(app.theme.progress_track),
        )
        .label(format!(
            "{} / {}  ({}%)",
            human_bytes(app.dashboard.bytes_done),
            human_bytes(app.dashboard.bytes_total),
            overall_pct
        ))
        .ratio(app.dashboard.overall_fraction());
    f.render_widget(overall, bars[1]);

    // Per-file gauge
    let label = match &app.dashboard.current_file {
        Some(cf) => format!("{} ({})", truncate(&cf.name, 40), human_bytes(cf.bytes)),
        None => String::from("(idle)"),
    };
    let file_block = Block::default()
        .title(Span::styled(label, Style::default().fg(app.theme.fg)))
        .borders(Borders::TOP)
        .border_style(Style::default().fg(app.theme.border));
    f.render_widget(file_block, bars[2]);
    let file_gauge = Gauge::default()
        .gauge_style(
            Style::default()
                .fg(app.theme.accent_alt)
                .bg(app.theme.progress_track),
        )
        .ratio(app.dashboard.file_fraction());
    f.render_widget(file_gauge, bars[3]);

    // Right: sparkline + meta
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(2)])
        .split(chunks[1]);

    let sparkline_data: Vec<u64> = app
        .dashboard
        .sparkline
        .samples()
        .iter()
        .map(|v| (*v * 100.0) as u64)
        .collect();
    let sparkline = Sparkline::default()
        .block(
            Block::default()
                .title(Span::styled(
                    " Throughput (MB/s) ",
                    Style::default().fg(app.theme.fg),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.border)),
        )
        .data(&sparkline_data)
        .style(Style::default().fg(app.theme.sparkline));
    f.render_widget(sparkline, right[0]);

    let meta = format!(
        "Speed: {}/s    Files: {}/{}    ETA: {}\nTheme: {}",
        human_bytes(app.dashboard.current_speed_bps as u64),
        app.dashboard.files_done,
        app.dashboard.files_total,
        app.dashboard
            .eta
            .map(human_duration)
            .unwrap_or_else(|| "--".into()),
        app.theme.name,
    );
    let meta_p = Paragraph::new(meta)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.border)),
        )
        .style(Style::default().fg(app.theme.fg));
    f.render_widget(meta_p, right[1]);
}

fn draw_log(f: &mut Frame, app: &App, area: Rect) {
    let visible = area.height.saturating_sub(2) as usize;
    let start = app.logs.len().saturating_sub(visible + app.log_scroll);
    let lines: Vec<Line> = app
        .logs
        .iter()
        .skip(start)
        .take(visible)
        .map(|l| {
            let color = match l.level {
                fiex_engine::LogLevel::Info => app.theme.log_info,
                fiex_engine::LogLevel::Warn => app.theme.log_warn,
                fiex_engine::LogLevel::Error => app.theme.log_error,
            };
            Line::from(vec![
                Span::styled(
                    format!("{} ", l.ts.format("%H:%M:%S")),
                    Style::default().fg(app.theme.dim),
                ),
                Span::styled(
                    format!("[{}] ", level_str(l.level)),
                    Style::default().fg(color),
                ),
                Span::styled(&l.message, Style::default().fg(app.theme.fg)),
            ])
        })
        .collect();
    let block = Block::default()
        .title(Span::styled(
            " Log ",
            Style::default().fg(app.theme.fg),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border));
    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let text = match app.mode {
        AppMode::Browsing => "  hjkl/arrows  navigate  ·  space  select  ·  tab  switch  ·  :  palette  ·  Ctrl-r  run  ·  Ctrl-c  cancel  ·  Ctrl-q  quit",
        AppMode::Transferring => "  transfer in progress…  Ctrl-c  cancel",
        AppMode::Palette => "  type to filter  ·  Enter  accept  ·  Esc  cancel",
        AppMode::Quitting => "  shutting down…",
    };
    let p = Paragraph::new(text)
        .style(Style::default().fg(app.theme.dim))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(app.theme.border)),
        );
    f.render_widget(p, area);
}

fn draw_palette_overlay(f: &mut Frame, app: &App, area: Rect) {
    let h: u16 = 3;
    let w = area.width.saturating_sub(8).min(80);
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);
    f.render_widget(Clear, rect);
    let p = Paragraph::new(format!(" :{}", app.palette_query))
        .style(Style::default().fg(app.theme.fg))
        .block(
            Block::default()
                .title(Span::styled(" Command Palette ", Style::default().fg(app.theme.accent)))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.accent)),
        );
    f.render_widget(p, rect);
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "K", "M", "G", "T", "P"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m{}s", s / 60, s % 60)
    } else {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    }
}

fn level_str(l: fiex_engine::LogLevel) -> &'static str {
    match l {
        fiex_engine::LogLevel::Info => "INFO",
        fiex_engine::LogLevel::Warn => "WARN",
        fiex_engine::LogLevel::Error => "ERR ",
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[allow(dead_code)]
pub fn current_theme_summary(theme: &Theme) -> String {
    theme.name.clone()
}
