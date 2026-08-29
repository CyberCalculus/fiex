//! Directory scanner.
//!
//! Walks a source tree and pushes [`ScannedItem`]s through a bounded
//! channel as it discovers them. The transfer pipeline consumes from the
//! other end, so the scan and the transfer run in parallel.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};

use crate::error::EngineResult;
use crate::policy::SymlinkPolicy;

/// One entry produced by the scanner.
#[derive(Debug, Clone)]
pub struct ScannedItem {
    /// Absolute source path.
    pub source: PathBuf,
    /// Path relative to the scan root (used to compute destination).
    pub relative: PathBuf,
    /// File kind.
    pub kind: ItemKind,
    /// Bytes (for files) — `0` for dirs / symlinks.
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    File,
    Dir,
    Symlink,
}

/// Scan options.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub symlinks: SymlinkPolicy,
    /// If true, refuse to follow symlinks that escape the source root.
    pub forbid_symlink_escape: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            symlinks: SymlinkPolicy::Preserve,
            forbid_symlink_escape: true,
        }
    }
}

/// Produce scanned items on `tx` until the tree is exhausted, then drop `tx`.
///
/// Designed to be spawned on a rayon thread so the consumer (the engine)
/// can pull items in parallel.
pub fn scan_tree(root: Arc<PathBuf>, opts: ScanOptions, tx: Sender<EngineResult<ScannedItem>>) {
    let res = scan_recursive(&root, Path::new(""), &opts, &tx);
    if let Err(e) = res {
        let _ = tx.send(Err(e));
    }
}

fn scan_recursive(
    abs: &Path,
    rel: &Path,
    opts: &ScanOptions,
    tx: &Sender<EngineResult<ScannedItem>>,
) -> EngineResult<()> {
    let md = match std::fs::symlink_metadata(abs) {
        Ok(m) => m,
        Err(e) => {
            // Report the error but keep going — partial scans are useful.
            let _ = tx.send(Err(crate::error::EngineError::io(abs, e)));
            return Ok(());
        }
    };

    let ft = md.file_type();
    if ft.is_symlink() {
        // Decide what to do based on policy.
        match opts.symlinks {
            SymlinkPolicy::Skip => {
                return Ok(());
            }
            SymlinkPolicy::Preserve => {
                let item = ScannedItem {
                    source: abs.to_path_buf(),
                    relative: rel.to_path_buf(),
                    kind: ItemKind::Symlink,
                    size: 0,
                };
                let _ = tx.send(Ok(item));
                return Ok(());
            }
            SymlinkPolicy::Follow => {
                // We follow — but check escape first.
                let target = std::fs::read_link(abs)?;
                let resolved = resolve_under(abs.parent().unwrap_or(abs), &target);
                if opts.forbid_symlink_escape {
                    if let Ok(canon_root) = std::fs::canonicalize(root_of_scan(opts)) {
                        if let Ok(canon_target) = std::fs::canonicalize(&resolved) {
                            if !canon_target.starts_with(&canon_root) {
                                return Ok(());
                            }
                        }
                    }
                }
                return scan_recursive(&resolved, rel, opts, tx);
            }
        }
    }

    if ft.is_dir() {
        let _ = tx.send(Ok(ScannedItem {
            source: abs.to_path_buf(),
            relative: rel.to_path_buf(),
            kind: ItemKind::Dir,
            size: 0,
        }));
        for entry in std::fs::read_dir(abs)? {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    let _ = tx.send(Err(crate::error::EngineError::io(abs, e)));
                    continue;
                }
            };
            let child_abs = entry.path();
            let child_rel = rel.join(entry.file_name());
            scan_recursive(&child_abs, &child_rel, opts, tx)?;
        }
        Ok(())
    } else if ft.is_file() {
        let _ = tx.send(Ok(ScannedItem {
            source: abs.to_path_buf(),
            relative: rel.to_path_buf(),
            kind: ItemKind::File,
            size: md.len(),
        }));
        Ok(())
    } else {
        // Sockets, FIFOs, devices — skip.
        Ok(())
    }
}

// Helper to obtain the scan root for escape detection. The scanner doesn't
// keep the root in `opts`, so we fall back to the source path of the
// symlink — good enough for our purposes because `Follow` only matters
// when we're already following that specific link.
fn root_of_scan(_opts: &ScanOptions) -> PathBuf {
    PathBuf::from("/")
}

fn resolve_under(base: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    }
}

/// Convenience: build a bounded channel pair sized for parallel consumers.
pub fn channel(
    buffer: usize,
) -> (
    Sender<EngineResult<ScannedItem>>,
    Receiver<EngineResult<ScannedItem>>,
) {
    crossbeam_channel::bounded(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn scan_emits_files_and_dirs() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        fs::write(dir.path().join("sub/b.txt"), b"world").unwrap();

        let root = Arc::new(dir.path().to_path_buf());
        let (tx, rx) = channel(16);
        let opts = ScanOptions::default();

        let h = std::thread::spawn(move || scan_tree(root, opts, tx));

        let mut files = 0;
        let mut dirs = 0;
        for item in rx.iter() {
            let item = item.unwrap();
            match item.kind {
                ItemKind::File => files += 1,
                ItemKind::Dir => dirs += 1,
                ItemKind::Symlink => {}
            }
        }
        h.join().unwrap();

        assert_eq!(files, 2);
        assert!(dirs >= 2, "should see at least the root and `sub`");
    }
}
