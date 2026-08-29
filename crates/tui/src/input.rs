//! Keyboard input → app commands.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    MoveUp,
    MoveDown,
    MoveTop,
    MoveBottom,
    PageUp,
    PageDown,
    Activate,
    Parent,
    ToggleSelect,
    ClearSelection,
    FocusLeft,
    FocusRight,
    OpenCommandPalette,
    SubmitPalette,
    CancelPalette,
    PaletteInsert(char),
    PaletteBackspace,
    RunTransfer,
    CancelTransfer,
    Quit,
    ShowLog,
    Noop,
}

pub fn map_key(ev: KeyEvent) -> Command {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    match ev.code {
        KeyCode::Char('q') if ctrl => Command::Quit,
        KeyCode::Esc => Command::CancelPalette,
        KeyCode::Char('c') if ctrl => Command::CancelTransfer,
        KeyCode::Char('j') | KeyCode::Down => Command::MoveDown,
        KeyCode::Char('k') | KeyCode::Up => Command::MoveUp,
        KeyCode::Char('h') | KeyCode::Left => Command::Parent,
        KeyCode::Char('l') | KeyCode::Right => Command::Activate,
        KeyCode::Char('g') => Command::MoveTop,
        KeyCode::Char('G') => Command::MoveBottom,
        KeyCode::PageUp => Command::PageUp,
        KeyCode::PageDown => Command::PageDown,
        KeyCode::Enter => Command::Activate,
        KeyCode::Tab => Command::FocusRight,
        KeyCode::BackTab => Command::FocusLeft,
        KeyCode::Char(' ') => Command::ToggleSelect,
        KeyCode::Char('a') if ctrl => Command::ClearSelection,
        KeyCode::Char('p') if ctrl => Command::OpenCommandPalette,
        KeyCode::Char(':') => Command::OpenCommandPalette,
        KeyCode::Char('r') if ctrl => Command::RunTransfer,
        KeyCode::Char(c) => Command::PaletteInsert(c),
        KeyCode::Backspace => Command::PaletteBackspace,
        _ => Command::Noop,
    }
}
