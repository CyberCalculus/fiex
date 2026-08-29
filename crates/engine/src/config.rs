//! Engine configuration. Loads from a TOML file, layered on top of defaults.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{EngineError, EngineResult};
use crate::policy::{ConflictPolicy, SymlinkPolicy};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// I/O buffer size in bytes.
    pub buffer_size: usize,

    /// Maximum number of parallel file transfers.
    pub parallelism: usize,

    /// How to handle name collisions at the destination.
    pub conflict_policy: ConflictPolicy,

    /// How to handle symlinks encountered in the source tree.
    pub symlink_policy: SymlinkPolicy,

    /// Verify destination by hashing with BLAKE3 after copy.
    pub verify: VerifyMode,

    /// Preserve POSIX permissions, mtime, atime.
    pub preserve_metadata: bool,

    /// Preserve extended attributes (xattrs). On Linux only.
    pub preserve_xattrs: bool,

    /// Allow following symlinks that escape the source tree. Off by default
    /// to prevent exfiltration / overwrite attacks.
    pub allow_symlink_escape: bool,

    /// Use cross-filesystem CoW (reflink) when possible. Falls back to a
    /// buffered copy if the syscall fails.
    pub try_reflink: bool,

    /// Color theme name (TUI-only hint, free-form).
    pub theme: String,
}

/// When to run a BLAKE3 verification pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyMode {
    /// Never run an extra verification pass beyond the streaming one.
    None,
    /// Verify every file after the copy completes.
    #[default]
    All,
    /// Verify a random sample of files (pct is 0-100).
    Sample,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            buffer_size: 256 * 1024,
            parallelism: num_cpus(),
            conflict_policy: ConflictPolicy::Prompt,
            symlink_policy: SymlinkPolicy::Preserve,
            verify: VerifyMode::All,
            preserve_metadata: true,
            preserve_xattrs: false,
            allow_symlink_escape: false,
            try_reflink: true,
            theme: "catppuccin-mocha".to_string(),
        }
    }
}

impl Config {
    /// Load config from a TOML file. Missing file is OK (defaults).
    pub fn load_from(path: &Path) -> EngineResult<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| EngineError::io(path, e))?;
        let cfg: Self = toml::from_str(&text)
            .map_err(|e| EngineError::Config(format!("{}: {}", path.display(), e)))?;
        Ok(cfg)
    }

    /// Validate a config in isolation.
    pub fn validate(&self) -> EngineResult<()> {
        if self.buffer_size == 0 {
            return Err(EngineError::Config("buffer_size must be > 0".into()));
        }
        if self.parallelism == 0 {
            return Err(EngineError::Config("parallelism must be > 0".into()));
        }
        Ok(())
    }
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        let cfg = Config::default();
        cfg.validate().expect("default config should validate");
    }

    #[test]
    fn zero_buffer_is_rejected() {
        let cfg = Config {
            buffer_size: 0,
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn missing_toml_returns_defaults() {
        let p = std::env::temp_dir().join("fiex-does-not-exist-12345.toml");
        let _ = std::fs::remove_file(&p);
        let cfg = Config::load_from(&p).unwrap();
        assert_eq!(cfg.buffer_size, Config::default().buffer_size);
    }

    #[test]
    fn parses_real_toml() {
        let toml = r#"
            buffer_size = 524288
            parallelism = 8
            conflict_policy = "overwrite"
            symlink_policy = "follow"
            verify = "all"
            preserve_metadata = true
            preserve_xattrs = true
            allow_symlink_escape = false
            try_reflink = true
            theme = "tokyo-night"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.buffer_size, 524288);
        assert_eq!(cfg.parallelism, 8);
        assert_eq!(cfg.conflict_policy, ConflictPolicy::Overwrite);
        assert_eq!(cfg.symlink_policy, SymlinkPolicy::Follow);
        assert_eq!(cfg.theme, "tokyo-night");
    }
}
