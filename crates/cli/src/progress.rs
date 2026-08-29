//! Linear `rich`-style progress renderer for `fiex`.
//!
//! The engine emits typed `Event`s. This module turns them into a
//! `indicatif` `MultiProgress` with one overall bar and one transient
//! per-file bar, plus scrolled log lines and a final summary. When the
//! destination is not a TTY (or `NO_COLOR` is set, or `--no-progress`
//! was passed), it falls back to a periodic plain-text log on stderr
//! — no terminal control sequences, no cursor tricks.

use std::io::IsTerminal;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use fiex_engine::{Event, FileOutcome, LogLevel, Progress};
use indicatif::{
    HumanBytes, HumanDuration, MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle,
};

/// A small throttle so we don't spam the renderer on tight per-chunk
/// progress ticks. Anything faster than ~30 Hz is just visual noise.
const PROGRESS_THROTTLE: Duration = Duration::from_millis(33);

pub struct Renderer {
    is_tty: bool,
    /// Last time we emitted a `Progress` event to the underlying
    /// progress machinery. Used to throttle updates.
    last_tick: Instant,
    /// Last byte position we printed in plain mode (so we only print
    /// every ~512 MiB of progress, not on every chunk).
    last_printed_bytes: u64,
    multi: Option<MultiProgress>,
    overall: Option<ProgressBar>,
    current: Option<ProgressBar>,
    started_at: Instant,
    files_completed: u64,
    bytes_completed: u64,
    errors: u64,
    /// Whether to force plain text mode (no bars) even on a TTY.
    force_plain: bool,
}

impl Renderer {
    pub fn new(force_plain: bool) -> Self {
        let is_tty = std::io::stderr().is_terminal();
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let disable_progress = force_plain || !is_tty || no_color;
        let multi = if disable_progress {
            None
        } else {
            // DrawTarget::stderr keeps the bars out of the way of any
            // piped output. When stderr is not a TTY we already chose
            // the plain path.
            Some(MultiProgress::with_draw_target(ProgressDrawTarget::stderr()))
        };
        Self {
            is_tty,
            last_tick: Instant::now() - PROGRESS_THROTTLE,
            last_printed_bytes: 0,
            multi,
            overall: None,
            current: None,
            started_at: Instant::now(),
            files_completed: 0,
            bytes_completed: 0,
            errors: 0,
            force_plain,
        }
    }

    /// Drive the renderer from the engine's event stream. Returns when
    /// the channel is closed (engine returned) so the caller can exit.
    pub async fn drive(
        mut self,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<Event>,
        sources: &[std::path::PathBuf],
        dest: &std::path::Path,
    ) -> Result<()> {
        self.print_header(sources, dest);
        while let Some(ev) = rx.recv().await {
            self.handle(ev);
        }
        self.finish();
        Ok(())
    }

    fn print_header(&self, sources: &[std::path::PathBuf], dest: &std::path::Path) {
        if self.is_tty && !self.force_plain {
            // On a TTY, defer to bars; log a single summary line.
            eprintln!("fiex: {} source(s) → {}", sources.len(), dest.display());
        } else {
            eprintln!(
                "fiex: copying {} source(s) to {}",
                sources.len(),
                dest.display()
            );
            for s in sources {
                eprintln!("  src: {}", s.display());
            }
        }
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Started {
                files_total,
                bytes_total,
            } => {
                self.started_at = Instant::now();
                if let Some(mp) = &self.multi {
                    let pb = mp.add(ProgressBar::new(bytes_total.max(1)));
                    pb.set_style(overall_style());
                    pb.set_prefix("total ");
                    pb.set_message(format!("{files_total} files"));
                    self.overall = Some(pb);
                } else {
                    eprintln!(
                        "fiex: starting — {files_total} files, {}",
                        HumanBytes(bytes_total)
                    );
                }
            }
            Event::FileStarted { source, bytes, .. } => {
                let name = source
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| source.display().to_string());
                if let Some(mp) = &self.multi {
                    if let Some(prev) = self.current.take() {
                        prev.finish_and_clear();
                    }
                    let pb = mp.add(ProgressBar::new(bytes.max(1)));
                    pb.set_style(per_file_style());
                    pb.set_prefix(format!("{name} "));
                    self.current = Some(pb);
                }
            }
            Event::Progress(p) => {
                let use_visual = self.is_tty && !self.force_plain;
                if !self.should_tick() {
                    return;
                }
                if use_visual {
                    self.apply_progress_visual(&p);
                } else {
                    self.apply_progress_plain(&p);
                }
            }
            Event::FileCompleted {
                outcome,
                source,
                destination,
                bytes,
                elapsed,
                ..
            } => {
                self.files_completed += 1;
                self.bytes_completed += bytes;
                if let Some(pb) = self.current.take() {
                    pb.finish_and_clear();
                }
                if let Some(pb) = &self.overall {
                    pb.inc(bytes);
                }
                self.print_file_outcome(outcome, &source, &destination, bytes, elapsed);
            }
            Event::FileError {
                source, message, ..
            } => {
                self.errors += 1;
                if let Some(pb) = &self.overall {
                    pb.println(format!(
                        "\n  ✗ error: {} — {}",
                        short_path(&source),
                        message
                    ));
                } else {
                    eprintln!("  ✗ error: {} — {}", short_path(&source), message);
                }
            }
            Event::Log { level, message } => {
                let prefix = match level {
                    LogLevel::Info => "  i",
                    LogLevel::Warn => "  !",
                    LogLevel::Error => "  ✗",
                };
                if let Some(pb) = &self.overall {
                    pb.println(format!("{prefix} {message}"));
                } else {
                    eprintln!("{prefix} {message}");
                }
            }
            Event::Done { success, errors } => {
                if !success {
                    self.errors = self.errors.max(errors);
                }
            }
        }
    }

    fn apply_progress_visual(&self, p: &Progress) {
        if let Some(pb) = &self.overall {
            pb.set_position(p.bytes_done);
            let speed = p.current_speed_bps;
            if speed > 0.0 {
                pb.set_message(format!(
                    "{}/{} files  {}  {:.1} MB/s",
                    p.files_done,
                    p.files_total,
                    p.eta
                        .map(|d| HumanDuration(d).to_string())
                        .unwrap_or_default(),
                    speed / 1_000_000.0
                ));
            } else {
                pb.set_message(format!("{}/{} files", p.files_done, p.files_total));
            }
        }
        if let (Some(cur), Some(written), Some(total)) =
            (&self.current, p.current_file_written, p.current_file_total)
        {
            cur.set_position(written);
            cur.set_message(format!("{} / {}", HumanBytes(written), HumanBytes(total)));
        }
    }

    fn apply_progress_plain(&mut self, p: &Progress) {
        // Only print every ~512 MiB of progress OR on the first
        // progress event, to keep plain-text mode readable.
        let last = self.last_printed_bytes;
        if last > 0 && p.bytes_done.saturating_sub(last) < 512 * 1024 * 1024 {
            return;
        }
        self.last_printed_bytes = p.bytes_done;
        if let Some(name) = &p.current_file {
            eprintln!(
                "  · {}/{}  {}/{}  ({:.1} MB/s){}",
                p.files_done,
                p.files_total,
                HumanBytes(p.bytes_done),
                HumanBytes(p.bytes_total),
                p.current_speed_bps / 1_000_000.0,
                if name.is_empty() {
                    String::new()
                } else {
                    format!("  {name}")
                }
            );
        } else {
            eprintln!(
                "  · {}/{}  {}/{}  ({:.1} MB/s)",
                p.files_done,
                p.files_total,
                HumanBytes(p.bytes_done),
                HumanBytes(p.bytes_total),
                p.current_speed_bps / 1_000_000.0
            );
        }
    }

    fn print_file_outcome(
        &self,
        outcome: FileOutcome,
        source: &std::path::Path,
        destination: &std::path::Path,
        bytes: u64,
        elapsed: Duration,
    ) {
        let mark = match outcome {
            FileOutcome::Copied => "✓",
            FileOutcome::Moved => "✓",
            FileOutcome::Skipped => "↷",
            FileOutcome::Resumed => "↻",
            FileOutcome::Reflinked => "⧉",
        };
        let line = format!(
            "  {} {} {} in {}",
            mark,
            outcome_verb(outcome),
            short_path(source),
            HumanDuration(elapsed)
        );
        let _ = (bytes, destination);
        if let Some(pb) = &self.overall {
            pb.println(line);
        } else {
            eprintln!("{line}");
        }
    }

    fn should_tick(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_tick) >= PROGRESS_THROTTLE {
            self.last_tick = now;
            true
        } else {
            false
        }
    }

    fn finish(self) {
        if let Some(pb) = self.overall.as_ref() {
            pb.finish_and_clear();
        }
        if let Some(pb) = self.current.as_ref() {
            pb.finish_and_clear();
        }
        let elapsed = self.started_at.elapsed();
        let speed = if elapsed.as_secs_f64() > 0.0 {
            (self.bytes_completed as f64 / elapsed.as_secs_f64()) / 1_000_000.0
        } else {
            0.0
        };
        let summary = format!(
            "fiex: done — {} files, {} in {} ({:.1} MB/s, {} errors)",
            self.files_completed,
            HumanBytes(self.bytes_completed),
            HumanDuration(elapsed),
            speed,
            self.errors
        );
        eprintln!("{summary}");
    }
}

fn outcome_verb(o: FileOutcome) -> &'static str {
    match o {
        FileOutcome::Copied => "copied",
        FileOutcome::Moved => "moved",
        FileOutcome::Skipped => "skipped",
        FileOutcome::Resumed => "resumed",
        FileOutcome::Reflinked => "reflinked",
    }
}

/// Build the interactive prompt callback used when
/// `--conflict prompt` is set on a TTY. Reads a single line of
/// input per conflict and returns a [`fiex_engine::PromptDecision`].
///
/// Recognized answers:
/// - `y` / `yes` — overwrite this file
/// - `n` / `no` — skip this file
/// - `a` / `all` — overwrite this and every remaining prompt
/// - `q` / `quit` — cancel the run
///
/// An empty line defaults to "no"; an unknown line re-prompts.
/// EOF on the very first read (e.g. Ctrl-D on an empty stdin) is
/// treated as "quit" so a closed stdin doesn't leave workers
/// blocked.
///
/// Multiple worker threads can hit this concurrently. A `Mutex<()>`
/// is held only across the per-call `stdin().lock()` so the
/// `StdinLock` guard (which isn't `Send`) never has to cross
/// thread boundaries — only the `()` does.
pub fn interactive_prompt() -> fiex_engine::PromptCallback {
    use fiex_engine::PromptDecision;
    use std::io::{BufRead, Write};
    use std::sync::Mutex;

    // A `Mutex<()>` is the serialization point. We lock it, then
    // take `stdin().lock()` inside, and drop both guards before
    // returning. The StdinLock is non-Send but it's only ever on
    // the calling thread, so the `Send + Sync` bound on the
    // closure is satisfied.
    let gate = Mutex::new(());

    Arc::new(
        move |src: &std::path::Path, dst: &std::path::Path| -> PromptDecision {
            loop {
                let _g = gate.lock().unwrap();
                let mut stdin = std::io::stdin().lock();
                eprint!(
                    "\nfiex: {} -> {} exists. Overwrite? [y/n/a/q]: ",
                    short_path(src),
                    short_path(dst),
                );
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                let n = match stdin.read_line(&mut line) {
                    Ok(n) => n,
                    Err(_) => return PromptDecision::Cancel, // broken stdin — bail
                };
                if n == 0 {
                    // EOF (Ctrl-D) on an empty line — treat as "quit"
                    // so a closed stdin doesn't keep the engine running.
                    return PromptDecision::Cancel;
                }
                match line.trim().to_ascii_lowercase().as_str() {
                    "y" | "yes" => return PromptDecision::Overwrite,
                    "n" | "no" | "" => return PromptDecision::Skip,
                    "a" | "all" => return PromptDecision::All,
                    "q" | "quit" => return PromptDecision::Cancel,
                    _ => {
                        eprintln!("  please answer y, n, a, or q");
                    }
                }
            }
        },
    )
}

fn short_path(p: &std::path::Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn overall_style() -> ProgressStyle {
    ProgressStyle::with_template("{prefix}{bar:40.cyan/blue} {pos:>9}/{len:9} {msg}")
        .expect("valid template")
        .progress_chars("##-")
}

fn per_file_style() -> ProgressStyle {
    ProgressStyle::with_template("{prefix:.bold}{bar:30.green/black} {msg}")
        .expect("valid template")
        .progress_chars("=>-")
}

/// Spawn a tokio task that listens for SIGINT and triggers the given
/// `EngineHandle::cancel()`.
pub fn install_ctrl_c_handler(handle: Arc<fiex_engine::EngineHandle>) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            handle.cancel();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_path_strips_directories() {
        let p = std::path::Path::new("/tmp/foo/bar.bin");
        assert_eq!(short_path(p), "bar.bin");
    }

    #[test]
    fn renderer_does_not_panic_on_plain_construction() {
        // Even on a non-TTY (CI) we should construct cleanly.
        let r = Renderer::new(false);
        drop(r);
    }

    #[test]
    fn renderer_handles_started_then_done() {
        let mut r = Renderer::new(true);
        r.handle(Event::Started {
            files_total: 1,
            bytes_total: 100,
        });
        r.handle(Event::Done {
            success: true,
            errors: 0,
        });
        // No panic = pass.
    }
}
