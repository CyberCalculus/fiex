//! `fiex` CLI — top-level entry point. Parses args, loads config, runs
//! the engine. The renderer emits the same linear `rich`-style output on
//! every terminal and falls back to plain log lines when stderr isn't a
//! TTY or `NO_COLOR` is set.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, ValueHint};
use fiex_engine::{Config, ConflictPolicy, SymlinkPolicy, TransferMode, VerifyMode};

mod headless;
mod progress;

#[derive(Parser, Debug)]
#[command(
    name = "fiex",
    version,
    about = "Fast, secure file mover/copier with BLAKE3 integrity checks",
    long_about = "fiex copies and moves files recursively with linear (rich-style) \
                  progress bars, BLAKE3 integrity checks, atomic .tmp + rename \
                  semantics, resume-on-the-same-prefix, and cross-filesystem CoW \
                  reflink when available."
)]
struct Cli {
    /// Source paths (file or directory). Repeatable.
    #[arg(value_hint = ValueHint::FilePath, required = true)]
    sources: Vec<PathBuf>,

    /// Destination directory.
    #[arg(value_hint = ValueHint::DirPath)]
    dest: PathBuf,

    /// Move instead of copy (deletes source on success).
    #[arg(short, long = "move")]
    move_: bool,

    /// Path to a TOML config file (defaults to $XDG_CONFIG_HOME/fiex/config.toml).
    #[arg(long, value_hint = ValueHint::FilePath)]
    config: Option<PathBuf>,

    /// I/O buffer size in bytes.
    #[arg(long)]
    buffer_size: Option<usize>,

    /// Parallel file transfers.
    #[arg(long)]
    parallelism: Option<usize>,

    /// Conflict policy: overwrite | skip | rename-old | rename-new | prompt.
    #[arg(long)]
    conflict: Option<String>,

    /// Symlink policy: preserve | follow | skip.
    #[arg(long)]
    symlinks: Option<String>,

    /// Verify mode: none | all | sample=PCT (0-100).
    #[arg(long, value_name = "MODE")]
    verify: Option<String>,

    /// Try cross-FS CoW reflink before falling back to buffered copy.
    #[arg(long)]
    try_reflink: Option<bool>,

    /// Preserve POSIX permissions + mtime/atime.
    #[arg(long)]
    preserve_metadata: Option<bool>,

    /// Preserve xattrs (Linux).
    #[arg(long)]
    preserve_xattrs: Option<bool>,

    /// Allow following symlinks that escape the source tree (off by default).
    #[arg(long)]
    allow_symlink_escape: Option<bool>,

    /// Force plain text progress lines instead of bars (also implied by
    /// non-TTY output or `NO_COLOR=1`).
    #[arg(long)]
    no_progress: bool,
}

fn main() -> ExitCode {
    init_tracing();

    let cli = Cli::parse();
    let config_path = cli
        .config
        .clone()
        .or_else(default_config_path)
        .unwrap_or_else(|| PathBuf::from("fiex.toml"));
    let mut cfg = match Config::load_from(&config_path)
        .with_context(|| format!("loading config from {}", config_path.display()))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fiex: {e:#}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = apply_cli_overrides(&cli, &mut cfg) {
        eprintln!("fiex: {e:#}");
        return ExitCode::from(2);
    }
    if let Err(e) = cfg.validate() {
        eprintln!("fiex: {e}");
        return ExitCode::from(2);
    }

    let mode = if cli.move_ {
        TransferMode::Move
    } else {
        TransferMode::Copy
    };

    let no_progress = cli.no_progress;
    let cfg_for_run = cfg.clone();
    let sources = cli.sources.clone();
    let dest = cli.dest.clone();

    let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        rt.block_on(
            async move { headless::run(cfg_for_run, sources, dest, mode, no_progress).await },
        )
    })) {
        Ok(Ok(code)) => code,
        Ok(Err(e)) => {
            eprintln!("fiex: {e:#}");
            1
        }
        Err(_) => {
            eprintln!("fiex: internal panic");
            1
        }
    };

    ExitCode::from(result as u8)
}

fn apply_cli_overrides(cli: &Cli, cfg: &mut Config) -> Result<()> {
    use anyhow::anyhow;
    if let Some(v) = cli.buffer_size {
        cfg.buffer_size = v;
    }
    if let Some(v) = cli.parallelism {
        cfg.parallelism = v;
    }
    if let Some(ref v) = cli.conflict {
        cfg.conflict_policy = match v.as_str() {
            "overwrite" => ConflictPolicy::Overwrite,
            "skip" => ConflictPolicy::Skip,
            "rename-old" => ConflictPolicy::RenameOld,
            "rename-new" => ConflictPolicy::RenameNew,
            "prompt" => ConflictPolicy::Prompt,
            other => return Err(anyhow!("unknown --conflict value: {other}")),
        };
    }
    if let Some(ref v) = cli.symlinks {
        cfg.symlink_policy = match v.as_str() {
            "preserve" => SymlinkPolicy::Preserve,
            "follow" => SymlinkPolicy::Follow,
            "skip" => SymlinkPolicy::Skip,
            other => return Err(anyhow!("unknown --symlinks value: {other}")),
        };
    }
    if let Some(ref v) = cli.verify {
        cfg.verify = parse_verify(v)
            .ok_or_else(|| anyhow!("unknown --verify value: {v} (use none|all|sample=PCT)"))?;
    }
    if let Some(v) = cli.try_reflink {
        cfg.try_reflink = v;
    }
    if let Some(v) = cli.preserve_metadata {
        cfg.preserve_metadata = v;
    }
    if let Some(v) = cli.preserve_xattrs {
        cfg.preserve_xattrs = v;
    }
    if let Some(v) = cli.allow_symlink_escape {
        cfg.allow_symlink_escape = v;
    }
    Ok(())
}

fn parse_verify(s: &str) -> Option<VerifyMode> {
    if s.eq_ignore_ascii_case("none") {
        return Some(VerifyMode::None);
    }
    if s.eq_ignore_ascii_case("all") {
        return Some(VerifyMode::All);
    }
    if let Some(rest) = s.strip_prefix("sample=") {
        let pct: u8 = rest.parse().ok()?;
        if pct > 100 {
            return None;
        }
        return Some(VerifyMode::Sample { pct });
    }
    if let Some(rest) = s.strip_prefix("sample") {
        // Bare "sample" = 10% (a useful default).
        if rest.is_empty() {
            return Some(VerifyMode::Sample { pct: 10 });
        }
        if let Some(p) = rest.strip_prefix('=') {
            let pct: u8 = p.parse().ok()?;
            if pct > 100 {
                return None;
            }
            return Some(VerifyMode::Sample { pct });
        }
    }
    None
}

fn default_config_path() -> Option<PathBuf> {
    let mut p = dirs::config_dir()?;
    p.push("fiex");
    p.push("config.toml");
    Some(p)
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = fmt().with_env_filter(filter).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verify_accepts_keywords() {
        assert!(matches!(parse_verify("none"), Some(VerifyMode::None)));
        assert!(matches!(parse_verify("all"), Some(VerifyMode::All)));
        assert!(matches!(parse_verify("None"), Some(VerifyMode::None)));
    }

    #[test]
    fn parse_verify_sample_with_pct() {
        assert!(matches!(
            parse_verify("sample=25"),
            Some(VerifyMode::Sample { pct: 25 })
        ));
        assert!(matches!(
            parse_verify("sample=0"),
            Some(VerifyMode::Sample { pct: 0 })
        ));
        assert!(matches!(
            parse_verify("sample=100"),
            Some(VerifyMode::Sample { pct: 100 })
        ));
    }

    #[test]
    fn parse_verify_sample_bare_defaults_to_10() {
        assert!(matches!(
            parse_verify("sample"),
            Some(VerifyMode::Sample { pct: 10 })
        ));
    }

    #[test]
    fn parse_verify_rejects_unknown_and_out_of_range() {
        assert!(parse_verify("bogus").is_none());
        assert!(parse_verify("sample=200").is_none());
        assert!(parse_verify("sample=abc").is_none());
    }
}
