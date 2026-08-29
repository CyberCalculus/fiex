//! Conflict resolution and symlink handling policies.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
    /// Ask the user. The CLI's interactive runner prompts on a TTY and
    /// otherwise treats it as `Skip` (with a log line) so a piped run
    /// never blocks.
    Prompt,
}

impl Default for ConflictPolicy {
    fn default() -> Self {
        // Default to `Overwrite` so a fresh `Config` and a
        // serde-loaded `Config` agree: missing keys don't quietly
        // make the engine a no-op. Users who want a prompt have to
        // opt in via `--conflict prompt` or the `conflict_policy`
        // field.
        Self::Overwrite
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

/// What the user answered when asked about a destination conflict.
///
/// The engine's `run` accepts an optional prompt callback; if the
/// user picks `All`, the engine flips the in-flight run to
/// "overwrite everything remaining" so it doesn't keep asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDecision {
    Overwrite,
    Skip,
    /// Treat every remaining conflict in this run as Overwrite.
    All,
    /// Cancel the run.
    Cancel,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SymlinkPolicy {
    /// Re-create the symlink at the destination.
    #[default]
    Preserve,
    /// Replace the symlink with the file/dir it points to.
    Follow,
    /// Skip symlinks entirely.
    Skip,
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
    fn conflict_policy_default_is_overwrite() {
        // Default flipped from `Prompt` to `Overwrite` in v0.2.3 so
        // a fresh `Config` and a serde-loaded one with a missing
        // `conflict_policy` key agree (and the run actually copies).
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::Overwrite);
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
