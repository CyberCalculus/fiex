//! Dual-pane Norton-Commander style file browser.
//!
//! Each pane lists the contents of one directory. Navigation is hjkl +
//! arrow keys, `Tab` swaps focus, `Enter` descends into a directory, and
//! `Space` toggles the current entry as a "selected" item to act on.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct BrowserPane {
    pub current_dir: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    /// Vertical scroll offset (for tall listings).
    pub offset: usize,
    /// Items the user has selected for the operation.
    pub selected: std::collections::HashSet<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
    Parent,
}

impl BrowserPane {
    pub fn new(dir: PathBuf) -> Self {
        let mut s = Self {
            current_dir: dir,
            entries: Vec::new(),
            cursor: 0,
            offset: 0,
            selected: Default::default(),
        };
        s.refresh();
        s
    }

    pub fn refresh(&mut self) {
        let mut entries = Vec::new();
        // Always allow going up.
        if self.current_dir.parent().is_some() {
            entries.push(Entry {
                name: "..".into(),
                path: self.current_dir.join(".."),
                kind: EntryKind::Parent,
                size: 0,
            });
        }
        if let Ok(read) = std::fs::read_dir(&self.current_dir) {
            let mut v: Vec<Entry> = read
                .filter_map(|e| e.ok())
                .map(|e| {
                    let md = e.metadata().ok();
                    let ft = md.as_ref().map(|m| m.file_type());
                    let kind = match ft {
                        Some(t) if t.is_dir() => EntryKind::Dir,
                        Some(t) if t.is_symlink() => EntryKind::Symlink,
                        _ => EntryKind::File,
                    };
                    Entry {
                        name: e.file_name().to_string_lossy().into_owned(),
                        path: e.path(),
                        kind,
                        size: md.map(|m| m.len()).unwrap_or(0),
                    }
                })
                .collect();
            v.sort_by(|a, b| match (a.kind, b.kind) {
                (EntryKind::Parent, _) => std::cmp::Ordering::Less,
                (_, EntryKind::Parent) => std::cmp::Ordering::Greater,
                (EntryKind::Dir, EntryKind::File) => std::cmp::Ordering::Less,
                (EntryKind::File, EntryKind::Dir) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });
            entries.extend(v);
        }
        self.entries = entries;
        if self.cursor >= self.entries.len() {
            self.cursor = self.entries.len().saturating_sub(1);
        }
    }

    pub fn move_cursor(&mut self, delta: i64) {
        let len = self.entries.len() as i64;
        if len == 0 {
            return;
        }
        let mut cur = self.cursor as i64 + delta;
        if cur < 0 {
            cur = 0;
        }
        if cur >= len {
            cur = len - 1;
        }
        self.cursor = cur as usize;
    }

    pub fn jump_top(&mut self) {
        self.cursor = 0;
    }

    pub fn jump_bottom(&mut self) {
        if !self.entries.is_empty() {
            self.cursor = self.entries.len() - 1;
        }
    }

    pub fn activate(&mut self) -> Option<PathBuf> {
        let entry = self.entries.get(self.cursor)?.clone();
        match entry.kind {
            EntryKind::Dir => {
                self.current_dir = entry.path;
                self.refresh();
                Some(self.current_dir.clone())
            }
            EntryKind::File | EntryKind::Symlink => Some(entry.path),
            EntryKind::Parent => {
                if let Some(p) = self.current_dir.parent() {
                    self.current_dir = p.to_path_buf();
                    self.refresh();
                    Some(self.current_dir.clone())
                } else {
                    None
                }
            }
        }
    }

    pub fn toggle_select(&mut self) {
        if let Some(entry) = self.entries.get(self.cursor) {
            if matches!(entry.kind, EntryKind::Parent) {
                return;
            }
            if !self.selected.remove(&entry.path) {
                self.selected.insert(entry.path.clone());
            }
        }
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    #[allow(dead_code)]
    pub fn fuzzy_filter(
        &self,
        pattern: &str,
        _matcher: &mut nucleo_matcher::Matcher,
    ) -> Vec<usize> {
        if pattern.is_empty() {
            return (0..self.entries.len()).collect();
        }
        // Simple substring fuzzy filter: score by 1 if any char of the
        // pattern appears in order, else 0. Higher = better.
        let pat = pattern.to_lowercase();
        let mut indices: Vec<(usize, usize)> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            let name = e.name.to_lowercase();
            let score = if name.contains(&pat) {
                2
            } else if pat.chars().all(|c| name.contains(c)) {
                1
            } else {
                0
            };
            if score > 0 {
                indices.push((score, i));
            }
        }
        indices.sort_by(|a, b| b.0.cmp(&a.0));
        indices.into_iter().map(|(_, i)| i).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn browser_lists_dir_contents() {
        let d = tempdir().unwrap();
        fs::create_dir_all(d.path().join("sub")).unwrap();
        fs::write(d.path().join("a.txt"), b"x").unwrap();
        let mut p = BrowserPane::new(d.path().to_path_buf());
        p.refresh();
        // We expect: "..", "sub" (dir), "a.txt" (file) — sorted.
        assert!(p.entries.iter().any(|e| e.name == "a.txt"));
        assert!(p
            .entries
            .iter()
            .any(|e| e.name == "sub" && e.kind == EntryKind::Dir));
    }

    #[test]
    fn browser_cursor_clamped() {
        let d = tempdir().unwrap();
        let mut p = BrowserPane::new(d.path().to_path_buf());
        p.move_cursor(10);
        assert_eq!(p.cursor, 0);
        p.jump_bottom();
        assert!(p.cursor < p.entries.len());
    }
}
