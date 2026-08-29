# fiex — fast, secure file mover/copier

`fiex` (file-exchange) is a CLI file copy/move tool written in Rust. It pairs an
async, parallel transfer engine with a linear `rich`-style progress renderer on
the terminal, with a plain log-line fallback for pipes, CI, and `NO_COLOR`.

## Highlights

- **Recursive move / copy** of files and directories with live linear progress:
  one overall bar, one transient per-file bar, file count, throughput, ETA.
- **BLAKE3 streaming checksums**: verify is enabled by default and hashes
  source and destination during the same I/O pass (no extra reads).
- **Atomic operations**: every copy writes to a `dst + ".fiex.tmp"` sibling,
  fsyncs the temp, fsyncs the parent directory, then renames. Interrupting
  the process never leaves a half-written destination.
- **Resume**: a pre-existing `dst.fiex.tmp` is byte-compared against the start
  of the source. If the kept prefix matches, the copy seeks past it and
  appends only the remaining suffix. If it doesn't match, the temp is
  discarded and the copy restarts from zero.
- **Conflict policies**: `overwrite`, `skip`, `rename-old`, `rename-new`,
  `prompt`. `prompt` is non-interactive in this build (logs a skip line and
  continues); use one of the non-interactive policies in scripts.
- **Cross-filesystem CoW** via `copy_file_range` (Linux + Android bionic) with
  a `FICLONE` ioctl fallback, then a buffered copy if reflink isn't supported.
- **Symlink handling**: `preserve`, `follow`, `skip`. With `follow`, symlink
  escape from the source tree is forbidden by default (`--allow-symlink-escape`
  to opt in).
- **Metadata preservation**: POSIX mode bits, `mtime`, `atime`; optional xattrs
  (Linux).
- **Multi-threaded work-stealing**: `rayon`-style transfer pool (tokio
  `spawn_blocking` workers pulling from a bounded `crossbeam` channel) plus a
  `tokio` event channel that the renderer consumes.
- **Config file** in TOML at `$XDG_CONFIG_HOME/fiex/config.toml` (overridable
  with `--config`). All flags map to a TOML key.

## Workspace layout

```
.
├── .github/workflows/
│   ├── ci.yml         # fmt + clippy + test + cargo-machete, MSRV build
│   └── release.yml    # cross-build + GitHub release on `v*` tags
├── crates/
│   ├── engine/        # transfer logic, events, BLAKE3, reflink, xattrs — no UI deps
│   │   ├── src/ficlone_shim.c   # C wrapper for the FICLONE ioctl
│   │   └── build.rs             # compiles ficlone_shim.c via the `cc` crate
│   └── cli/           # clap parser, indicatif progress renderer
├── Cargo.toml         # workspace root, pinned dep versions
├── justfile           # local recipes (fmt, clippy, test, build, ci)
└── README.md
```

The engine has zero dependencies on any TUI crate — every test in the
engine runs in a headless process.

## Build

Builds are CI-driven. Local builds aren't part of the supported workflow on
this host (the disk is too tight to fit a `target/` tree for the whole
workspace), but the CI workflow checks everything.

```bash
# from the repo root, in CI
just ci         # fmt + clippy + test + build + cargo-machete
# or
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build   --workspace --all-features
cargo test    --workspace --all-features
```

## Releases

Every pushed `v*` tag triggers `.github/workflows/release.yml`, which builds
the CLI for three targets, uploads the binaries as release artifacts, and
publishes a GitHub release with SHA-256 sums:

| Target                  | Triple                        | Notes                       |
| ----------------------- | ----------------------------- | --------------------------- |
| Linux x86_64            | `x86_64-unknown-linux-gnu`    | host gcc / linker           |
| Linux arm64             | `aarch64-unknown-linux-gnu`   | `gcc-aarch64-linux-gnu` cross |
| Android arm64           | `aarch64-linux-android`       | NDK r27 clang (no `cargo-ndk`) |

The Android artifact is a standalone ELF executable that runs on any
Android API 21+ device with bionic — no APK, no JNI. Push it with
`adb push fiex /data/local/tmp/ && adb shell /data/local/tmp/fiex --version`.

## Run

```bash
# copy a directory — bars on a TTY, plain log lines when piped
fiex /path/to/src /path/to/dst

# move instead of copy
fiex --move /path/to/src /path/to/dst

# non-interactive verify policy (sample 10% of files)
fiex --verify sample /path/to/src /path/to/dst

# force plain text progress (also implied by non-TTY output or NO_COLOR=1)
fiex --no-progress /path/to/src /path/to/dst

# show what would happen, then bail
fiex --help
```

### Quick reference

| Flag                          | What it does                                            |
| ----------------------------- | ------------------------------------------------------- |
| `--move` / `-m`               | Move instead of copy (deletes source on success).     |
| `--conflict <policy>`         | `overwrite` / `skip` / `rename-old` / `rename-new` / `prompt`. |
| `--symlinks <policy>`         | `preserve` / `follow` / `skip`.                       |
| `--verify <mode>`             | `none` / `all` / `sample[=PCT]` (bare `sample` = 10%). |
| `--try-reflink <bool>`        | Try `copy_file_range` / `FICLONE` first (default `true`). |
| `--preserve-metadata <bool>`  | Restore POSIX mode + mtime/atime (default `true`).    |
| `--preserve-xattrs <bool>`    | Copy xattrs on Linux (default `false`).               |
| `--allow-symlink-escape <bool>` | Permit `follow` to traverse outside the source root. |
| `--config <path>`             | Override the config file (TOML).                      |
| `--parallelism <n>`           | Worker pool size (default = # CPUs).                  |
| `--buffer-size <bytes>`       | Read/write buffer (default 256 KiB).                  |
| `--no-progress`               | Print plain log lines instead of bars.                |

### Config file

```toml
# ~/.config/fiex/config.toml
buffer_size = 524288
parallelism = 8
conflict_policy = "rename-new"     # overwrite | skip | rename-old | rename-new | prompt
symlink_policy = "preserve"        # preserve | follow | skip
verify = "all"                     # none | all | sample (or sample=PCT)
preserve_metadata = true
preserve_xattrs = false
allow_symlink_escape = false
try_reflink = true
```

CLI flags override the config file. Unknown keys are an error.

## What the output looks like

On a TTY, the renderer emits an overall progress bar plus a per-file bar that
disappears when the file finishes. Each completed file is logged with a glyph:

| Glyph | Meaning                                |
| ----- | -------------------------------------- |
| `✓`   | Copied or moved                        |
| `↻`   | Resumed from a kept `.fiex.tmp` prefix |
| `⧉`   | Reflinked (cross-FS CoW)               |
| `↷`   | Skipped (conflict policy)              |
| `✗`   | Error                                  |
| `i` / `!` | Info / warning log lines           |

When stderr is not a TTY (or `NO_COLOR` is set, or `--no-progress` is
passed) the renderer falls back to a periodic plain-text log:

```
fiex: starting — 12 files, 1.4 GiB
  · 1/12  120 MiB / 1.4 GiB  (212.0 MB/s)  big.iso
  · 1/12  1.4 GiB / 1.4 GiB  (245.7 MB/s)  big.iso
  ✓ big.iso  copied  in 5.8s
  ✓ notes.txt  copied  in 0.0s
fiex: done — 12 files, 1.4 GiB in 6.1s (245.7 MB/s, 0 errors)
```

Ctrl-c cancels the run cleanly and exits with code 130.

## Architecture notes

### Why `tokio` + `crossbeam`

The transfer pool is the bottleneck: each worker is a `tokio::task::spawn_blocking`
task that pulls a `PlanEntry` from a bounded `crossbeam_channel` and runs
`copy_file_with_progress`. Bounded channels give natural backpressure: if
the workers fall behind, the producer (the scan thread) blocks on `send`
instead of buffering the entire tree in memory.

`tokio` is reserved for the engine's event channel, the Ctrl-c listener,
and the renderer's `mpsc::unbounded_channel`. The renderer never does I/O.

### Atomic copy with resume

```
src ──chunked write──▶ dst.fiex.tmp   (HashingWriter, verify-if-enabled)
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

On resume, if `dst.fiex.tmp` is present, the routine opens both files and
byte-compares the first `n` bytes. If they match, the source is seeked to
`n` and the destination is opened in append mode — only the missing suffix
is copied. If they don't match, the temp is removed and the copy restarts
from zero.

### Symlink safety

`Follow` policy is gated by a per-scan check that canonicalizes the symlink
target and rejects any path that escapes the source root. The check uses
the actual canonicalized scan root (not `/`), so a misconfigured caller
can't accidentally disable it. `Preserve` re-creates the symlink as-is.
`Skip` ignores symlinks entirely.

## License

MIT — see [`LICENSE`](LICENSE).
