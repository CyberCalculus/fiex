//! Live transfer dashboard: per-file progress, overall bar, throughput
//! sparkline, file count, ETA.

use std::time::Duration;

use fiex_engine::{Event, FileOutcome, Progress};

use crate::sparkline::Sparkline;

#[derive(Debug, Clone)]
pub struct Dashboard {
    pub files_total: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub files_done: u64,
    pub current_speed_bps: f64,
    pub eta: Option<Duration>,
    pub current_file: Option<CurrentFile>,
    pub sparkline: Sparkline,
    pub last_progress_at: Option<std::time::Instant>,
}

#[derive(Debug, Clone)]
pub struct CurrentFile {
    pub name: String,
    pub bytes: u64,
    pub outcome: Option<FileOutcome>,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            files_total: 0,
            bytes_total: 0,
            bytes_done: 0,
            files_done: 0,
            current_speed_bps: 0.0,
            eta: None,
            current_file: None,
            sparkline: Sparkline::new(120),
            last_progress_at: None,
        }
    }
}

impl Dashboard {
    pub fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Started {
                files_total,
                bytes_total,
            } => {
                self.files_total = *files_total;
                self.bytes_total = *bytes_total;
                self.bytes_done = 0;
                self.files_done = 0;
                self.current_file = None;
            }
            Event::FileStarted { source, bytes, .. } => {
                self.current_file = Some(CurrentFile {
                    name: source
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| source.display().to_string()),
                    bytes: *bytes,
                    outcome: None,
                });
            }
            Event::FileCompleted { outcome, .. } => {
                if let Some(cf) = self.current_file.as_mut() {
                    cf.outcome = Some(*outcome);
                }
                self.files_done += 1;
            }
            Event::Progress(p) => {
                self.apply_progress(p.clone());
            }
            Event::Done { .. } => {
                self.current_file = None;
            }
            _ => {}
        }
    }

    fn apply_progress(&mut self, p: Progress) {
        self.bytes_done = p.bytes_done;
        self.current_speed_bps = p.current_speed_bps;
        self.eta = p.eta;
        let now = std::time::Instant::now();
        let dt = self
            .last_progress_at
            .map(|t| now.duration_since(t).as_secs_f64())
            .unwrap_or(0.5);
        self.last_progress_at = Some(now);
        if dt > 0.0 {
            let bps = p.current_speed_bps;
            // Convert to MB/s for the sparkline.
            self.sparkline.push(bps / 1_000_000.0);
        }
    }

    pub fn overall_fraction(&self) -> f64 {
        if self.bytes_total == 0 {
            0.0
        } else {
            (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0)
        }
    }

    pub fn file_fraction(&self) -> f64 {
        match &self.current_file {
            Some(cf) if cf.bytes > 0 => {
                // We don't have per-file byte progress; use the per-file
                // status as a qualitative indicator and report 1.0 for
                // completed, 0.5 for in-flight, 0.0 for not started.
                if cf.outcome.is_some() {
                    1.0
                } else {
                    0.5
                }
            }
            _ => 0.0,
        }
    }
}
