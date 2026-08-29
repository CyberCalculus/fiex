//! `fiex` CLI — top-level entry point. Parses args, loads config, dispatches
//! to the TUI (interactive) or the headless engine runner.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, ValueHint};
use fiex_engine::{Config, ConflictPolicy, SymlinkPolicy, VerifyMode};

mod config;
mod headless;
mod tui_runner;

#[derive(Parser, Debug)]
#[command(
    name = "fiex",
    version,
    about = "Fast, secure TUI file mover/copier with BLAKE3 integrity checks",
    long_about = "fiex copies and moves files recursively with a live terminal UI, \
                  BLAKE3 integrity checks, atomic .tmp + rename semantics, \
                  and cross-filesystem CoW reflink when available."
)]
struct Cli {
    /// Source paths (file or directory). Repeatable.
    #[arg(value_hint = ValueHint::FilePath, required = true)]
    sources: Vec<PathBuf>,

    /// Destination directory.
    #[arg(value_hint = ValueHint::DirPath)]
    dest: PathBuf,

    /// Move instead of copy.
    #[arg(short, long = "move")]
    move_: bool,

    /// Skip the TUI and run headlessly. Useful for scripts.
    #[arg(long)]
    headless: bool,

    /// Path to a TOML config file (defaults to $XDG_CONFIG_HOME/fiex/config.toml).
    #[arg(long, value_hint = ValueHint::FilePath)]
    config: Option<PathBuf>,

    /// I/O buffer size in bytes.
    #[arg(long)]
    buffer_size: Option<usize>,

    /// Parallel file transfers.
    #[arg(long)]
    parallelism: Option<usize>,

    /// Conflict policy: overwrite | skip | rename-old | rename-new | prompt
    #[arg(long)]
    conflict: Option<String>,

    /// Symlink policy: preserve | follow | skip
    #[arg(long)]
    symlinks: Option<String>,

    /// Verify mode: none | all | sample
    #[arg(long)]
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

    /// Theme name (catppuccin-mocha | tokyo-night).
    #[arg(long)]
    theme: Option<String>,
}

fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let config_path = cli
        .config
        .clone()
        .or_else(default_config_path)
        .unwrap_or_else(|| PathBuf::from("fiex.toml"));
    let mut cfg = Config::load_from(&config_path)
        .with_context(|| format!("loading config from {}", config_path.display()))?;
    apply_cli_overrides(&cli, &mut cfg);
    cfg.validate()?;

    let mode = if cli.move_ {
        fiex_engine::TransferMode::Move
    } else {
        fiex_engine::TransferMode::Copy
    };

    if cli.headless {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        rt.block_on(async move {
            headless::run(cfg, cli.sources, cli.dest, mode).await
        })?;
    } else {
        // TUI: own the terminal; tokio runtime inside.
        tui_runner::run(cfg, cli.sources, cli.dest, mode)?;
    }
    Ok(())
}

fn apply_cli_overrides(cli: &Cli, cfg: &mut Config) {
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
            other => {
                eprintln!("unknown --conflict value: {other}");
                ConflictPolicy::Prompt
            }
        };
    }
    if let Some(ref v) = cli.symlinks {
        cfg.symlink_policy = match v.as_str() {
            "preserve" => SymlinkPolicy::Preserve,
            "follow" => SymlinkPolicy::Follow,
            "skip" => SymlinkPolicy::Skip,
            other => {
                eprintln!("unknown --symlinks value: {other}");
                SymlinkPolicy::Preserve
            }
        };
    }
    if let Some(ref v) = cli.verify {
        cfg.verify = match v.as_str() {
            "none" => VerifyMode::None,
            "all" => VerifyMode::All,
            "sample" => VerifyMode::Sample,
            other => {
                eprintln!("unknown --verify value: {other}");
                VerifyMode::All
            }
        };
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
    if let Some(ref v) = cli.theme {
        cfg.theme = v.clone();
    }
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
