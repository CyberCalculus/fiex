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
}

/// When to run a BLAKE3 verification pass.
///
/// `All` is the default: every file is checksummed as it copies (zero
/// extra I/O passes — see `checksum::HashingWriter`).
/// `None` disables verification entirely.
/// `Sample { pct }` verifies only the given percentage of files
/// (uniform random, per file). `pct` is in `[0, 100]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyMode {
    /// Never run an extra verification pass beyond the streaming one.
    None,
    /// Verify every file as it is copied.
    #[default]
    All,
    /// Verify a random `pct` percent of files (0–100).
    Sample { pct: u8 },
}

impl VerifyMode {
    /// Should this file (identified by a 0-based plan index) be verified?
    /// `None` → never, `All` → always, `Sample { pct }` → uniform random
    /// per index. The RNG is deterministic per-thread (XorShift seeded
    /// from a process-wide counter + a salt based on the index) so tests
    /// can pin the exact sample set by setting the same seed twice.
    pub fn should_verify(&self, index: u64) -> bool {
        match *self {
            VerifyMode::None => false,
            VerifyMode::All => true,
            VerifyMode::Sample { pct } => {
                if pct == 0 {
                    return false;
                }
                if pct >= 100 {
                    return true;
                }
                // XorShift64 seeded with a per-process counter plus the
                // file index — uniform over [0, 100). We compare to `pct`
                // so the expected sampled fraction is `pct / 100`.
                let seed = (process_counter() ^ index.wrapping_mul(0x9E3779B97F4A7C15))
                    .wrapping_add(0xBF58476D1CE4E5B9);
                let mut s = seed;
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s % 100) < pct as u64
            }
        }
    }
}

fn process_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    C.fetch_add(1, Ordering::Relaxed)
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
    fn verify_mode_sample_rates() {
        use crate::config::VerifyMode;
        let none = VerifyMode::None;
        let all = VerifyMode::All;
        let sample_0 = VerifyMode::Sample { pct: 0 };
        let sample_50 = VerifyMode::Sample { pct: 50 };
        let sample_100 = VerifyMode::Sample { pct: 100 };

        for i in 0..10 {
            assert!(!none.should_verify(i));
            assert!(all.should_verify(i));
            assert!(!sample_0.should_verify(i));
            assert!(sample_100.should_verify(i));
        }

        // 50% over a 200-file run: between 60 and 140 verifies expected
        // (rough; the actual distribution depends on the seeded XorShift).
        let count = (0..200).filter(|i| sample_50.should_verify(*i)).count();
        assert!(count > 60 && count < 140, "got {count}");
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
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.buffer_size, 524288);
        assert_eq!(cfg.parallelism, 8);
        assert_eq!(cfg.conflict_policy, ConflictPolicy::Overwrite);
        assert_eq!(cfg.symlink_policy, SymlinkPolicy::Follow);
    }
}
