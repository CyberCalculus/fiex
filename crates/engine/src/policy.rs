//! Conflict resolution and symlink handling policies.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictPolicy {
    /// Always replace the destination.
    Overwrite,
    /// Keep the destination, leave the source as-is.
    Skip,
    /// Move the existing destination aside (suffix `.fiex-old.<n>`) then
    /// proceed with the new copy.
    RenameOld,
    /// Pick a unique name with a numeric suffix (e.g. `file (1).bin`).
    RenameNew,
    /// Ask the user. In headless mode this is treated as `Skip` with a log
    /// line; the TUI handles the real prompts.
    Prompt,
}

impl Default for ConflictPolicy {
    fn default() -> Self {
        Self::Prompt
    }
}

impl ConflictPolicy {
    pub fn resolve(self, src: &Path, dst: &Path) -> ResolvedTarget {
        match self {
            Self::Overwrite => ResolvedTarget::Overwrite {
                target: dst.to_path_buf(),
            },
            Self::Skip => ResolvedTarget::Skip,
            Self::RenameOld => ResolvedTarget::RenameOld {
                target: dst.to_path_buf(),
            },
            Self::RenameNew => ResolvedTarget::RenameNew {
                target: unique_path(dst),
            },
            Self::Prompt => ResolvedTarget::Prompt {
                source: src.to_path_buf(),
                destination: dst.to_path_buf(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    Overwrite {
        target: std::path::PathBuf,
    },
    Skip,
    RenameOld {
        target: std::path::PathBuf,
    },
    RenameNew {
        target: std::path::PathBuf,
    },
    Prompt {
        source: std::path::PathBuf,
        destination: std::path::PathBuf,
    },
}

/// Append a numeric suffix until the path does not exist. Caller should
/// still handle the race where a file is created in between this and the
/// rename.
pub fn unique_path(dst: &Path) -> std::path::PathBuf {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    let stem = dst
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = dst.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 1..u32::MAX {
        let candidate = match &ext {
            Some(e) if !e.is_empty() => parent.join(format!("{stem} ({n}).{e}")),
            _ => parent.join(format!("{stem} ({n})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    // Pathological fallback
    parent.join(format!("{stem}.fiex-exhausted"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymlinkPolicy {
    /// Re-create the symlink at the destination.
    Preserve,
    /// Replace the symlink with the file/dir it points to.
    Follow,
    /// Skip symlinks entirely.
    Skip,
}

impl Default for SymlinkPolicy {
    fn default() -> Self {
        Self::Preserve
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn unique_path_appends_counter() {
        let p = PathBuf::from("/tmp/data.bin");
        let first = unique_path(&p);
        assert_eq!(first, PathBuf::from("/tmp/data (1).bin"));
    }

    #[test]
    fn unique_path_handles_no_extension() {
        let p = PathBuf::from("/tmp/Makefile");
        let first = unique_path(&p);
        assert_eq!(first, PathBuf::from("/tmp/Makefile (1)"));
    }

    #[test]
    fn conflict_policy_default_is_prompt() {
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::Prompt);
    }

    #[test]
    fn conflict_policy_overwrite_targets_dst() {
        let src = PathBuf::from("/a");
        let dst = PathBuf::from("/b");
        assert_eq!(
            ConflictPolicy::Overwrite.resolve(&src, &dst),
            ResolvedTarget::Overwrite { target: dst }
        );
    }

    #[test]
    fn conflict_policy_skip_is_skip() {
        let src = PathBuf::from("/a");
        let dst = PathBuf::from("/b");
        assert_eq!(
            ConflictPolicy::Skip.resolve(&src, &dst),
            ResolvedTarget::Skip
        );
    }
}
