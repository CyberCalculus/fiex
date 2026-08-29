# fiex — fast, secure TUI file mover/copier

`fiex` (file-exchange) is an advanced, fast, and secure file move/copy TUI tool written in Rust. It pairs an async, parallel transfer engine with a polished `ratatui` terminal interface.

## Highlights

- **Recursive move / copy** of files and directories with real-time progress — per-file bar, overall bar, throughput sparkline, MB/s, ETA, file count.
- **BLAKE3 checksums** for streaming integrity checks. `--verify` mode re-hashes source and destination for an extra guarantee.
- **Atomic operations**: every copy writes to a `.fiex.tmp` sibling, then renames on success. Interrupting the process never leaves a half-written destination.
- **Resume**: pre-existing `.tmp` files are picked up and the kept prefix is verified before the copy continues.
- **Conflict policies**: `overwrite`, `skip`, `rename-old`, `rename-new`, `prompt`.
- **Cross-filesystem CoW** via `copy_file_range` (Linux) with a `FICLONE` fallback, then a buffered copy if reflink isn't supported.
- **Symlink handling**: `preserve`, `follow`, `skip`. Symlink escape from the source tree is forbidden by default.
- **Metadata preservation**: POSIX mode bits, `mtime`, `atime`; optional xattrs (Linux).
- **Multi-threaded work-stealing**: `rayon` for the transfer pool, `tokio` for the engine's event channel and the TUI's render loop. 60-fps redraw cap, no needless repaints.
- **Dual-pane file browser** (Norton-Commander style) with vim-style `hjkl` and arrow keys.
- **Command palette** (`:` or `Ctrl-p`) for fuzzy directory jumps.
- **Modern theme** (Catppuccin Mocha by default, Tokyo Night available), `rounded` borders, generous padding, color-coded log pane.
- **Config file** in TOML at `$XDG_CONFIG_HOME/fiex/config.toml` (overridable with `--config`).
- **Headless mode** (`--headless`) for scripts and CI — same engine, logs to stderr.

## Workspace layout

```
.
├── .github/workflows/
│   ├── ci.yml         # fmt + clippy + test, runs on every push
│   └── release.yml    # cross-build + GitHub release on `v*` tags
├── crates/
│   ├── engine/        # transfer logic, events, BLAKE3, reflink, xattrs — no UI deps
│   │   ├── src/ficlone_shim.c   # C wrapper for the FICLONE ioctl
│   │   └── build.rs             # compiles ficlone_shim.c via the `cc` crate
│   ├── tui/           # ratatui frontend — purely reactive on engine events
│   └── cli/           # clap parser, headless + TUI runners
├── Cargo.toml         # workspace root, pinned dep versions
├── justfile           # local recipes (fmt, clippy, test, build, ci)
└── README.md
```

The engine has zero dependencies on `ratatui`, so all of its logic is testable headlessly. The TUI is a pure consumer of the engine's typed `Event` stream.

## Build

Builds are CI-driven. Local builds aren't part of the supported workflow
on this host (the disk is too tight to fit a `target/` tree for the
whole workspace), but the CI workflow checks everything.

```bash
# from the repo root, in CI
just ci         # fmt + clippy + test + build
# or
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build   --workspace --all-features
cargo test    --workspace --all-features
```

## Releases

Every pushed `v*` tag triggers `.github/workflows/release.yml`, which
builds the engine, the TUI, and the CLI for three targets, uploads the
binaries as release artifacts, and publishes a GitHub release with
SHA-256 sums:

| Target                  | Triple                        | Notes                       |
| ----------------------- | ----------------------------- | --------------------------- |
| Linux x86_64            | `x86_64-unknown-linux-gnu`    | host gcc / linker           |
| Linux arm64             | `aarch64-unknown-linux-gnu`   | `gcc-aarch64-linux-gnu`     |
| Android arm64           | `aarch64-linux-android`       | NDK r27 clang (no `cargo-ndk`) |

The Android artifact is a standalone ELF executable that runs on any
Android API 21+ device with bionic — no APK, no JNI. Push it with
`adb push fiex /data/local/tmp/ && adb shell /data/local/tmp/fiex --version`.

## Run

```bash
# copy a directory interactively (TUI)
fiex /path/to/src /path/to/dst

# move instead of copy
fiex --move /path/to/src /path/to/dst

# headless: useful in CI / scripts (no TUI, events go to stderr)
fiex --headless /path/to/src /path/to/dst

# full verify after copy (re-hash source and destination)
fiex --verify all /path/to/src /path/to/dst
```

### Quick reference

| Flag                    | What it does                                            |
| ----------------------- | ------------------------------------------------------- |
| `--move` / `-m`         | Move instead of copy (deletes source on success).     |
| `--headless`            | Skip the TUI; print engine events to stderr.         |
| `--conflict <policy>`   | `overwrite` / `skip` / `rename-old` / `rename-new` / `prompt`. |
| `--symlinks <policy>`   | `preserve` / `follow` / `skip`.                       |
| `--verify <mode>`       | `none` / `all` / `sample`.                            |
| `--try-reflink <bool>`  | Enable `copy_file_range` / `FICLONE` (default: true). Pass `--try-reflink=false` to force buffered copy. |
| `--config <path>`       | Override the config file (TOML).                      |
| `--parallelism <n>`     | Worker pool size (default = # CPUs).                  |
| `--buffer-size <bytes>` | Read/write buffer (default 256 KiB).                  |

# write a config
cat > ~/.config/fiex/config.toml <<'EOF'
buffer_size = 524288
parallelism = 8
conflict_policy = "rename-new"
symlink_policy = "preserve"
verify = "all"
preserve_metadata = true
preserve_xattrs = true
allow_symlink_escape = false
try_reflink = true
theme = "catppuccin-mocha"
EOF
```

## Keybindings (TUI)

| key            | action                                    |
| -------------- | ----------------------------------------- |
| `hjkl` / arrows | navigate the focused pane                |
| `space`        | toggle selection                          |
| `Tab` / `Shift-Tab` | swap pane focus                     |
| `Enter`        | descend into a directory / select a file |
| `g` / `G`      | jump to top / bottom                      |
| `:` / `Ctrl-p` | open command palette                      |
| `Ctrl-r`       | run the transfer (from pane to pane)     |
| `Ctrl-c`       | cancel the running transfer               |
| `Ctrl-q`       | quit                                      |

## Architecture notes

### Why rayon for transfers and tokio for events

The transfer pool is CPU-bound during the scan and parallelism orchestration, but the actual I/O is the bottleneck. `rayon` gives us a no-fuss work-stealing pool that integrates cleanly with `std::fs::File` (which is `!Send`-safe inside an async context) and lets us drop into a `spawn_blocking` task per worker without ceremony.

`tokio` is reserved for: the engine's event channel, the TUI's input stream, the periodic redraw timer, and graceful shutdown. The TUI never does I/O directly.

### Atomic copy with resume

```
src ──chunked write──▶ dst.fiex.tmp
                            │
                            ▼
                       fsync(.tmp)
                            │
                            ▼
                       b3(source) == b3(.tmp) ?
                            │ yes
                            ▼
                       rename(.tmp → dst)
                            │
                            ▼
                       fsync(parent dir)   ← makes the rename durable
```

On resume, if `dst.fiex.tmp` is present, the routine appends the missing suffix onto it, then runs the same verify + rename path. If the kept prefix doesn't match the source (e.g. the source was modified since the last run), the temp file is removed and the copy restarts from zero.

### Symlink safety

`Follow` policy is gated by a per-scan check that resolves the link target and rejects any path that escapes the source root. `Preserve` re-creates the symlink as-is. `Skip` ignores symlinks entirely.

### io_uring

`io_uring` would be a meaningful win for high-throughput local I/O on Linux, but the `tokio-uring` runtime can't coexist with `tokio`'s default scheduler. The engine is structured so that swapping in `io_uring` later is a one-file change inside the transfer pool — the rest of the pipeline (events, conflicts, metadata) is unaffected.

## License

MIT — see [`LICENSE`](LICENSE).
