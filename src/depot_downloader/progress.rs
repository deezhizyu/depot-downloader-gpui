use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::event::{AuthPromptKind, Event, EventLine, LogLevel};

/// Speeds are the counter growth across this much of the most recent time.
const SPEED_WINDOW_MS: u64 = 1000;
const ETA_SMOOTHING_SECS: f64 = 10.0;

#[derive(Debug, Clone)]
pub struct AuthPrompt {
    pub kind: AuthPromptKind,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct DownloadStats {
    pub status_message: String,
    pub total_compressed_bytes: u64,
    pub total_uncompressed_bytes: u64,
    pub total_files: u64,
    /// Compressed bytes received from Steam this run.
    pub network_bytes: u64,
    /// Uncompressed bytes written to disk this run.
    pub written_bytes: u64,
    /// Uncompressed bytes already valid on disk, so not downloaded again.
    pub verified_bytes: u64,
    pub files_done: u64,
    pub download_speed_bytes_per_sec: f64,
    pub disk_speed_bytes_per_sec: f64,
    pub eta: Option<Duration>,
    pub auth_prompt: Option<AuthPrompt>,
    pub qr_url: Option<String>,
    pub error_message: Option<String>,
    pub logged_in_username: Option<String>,
    /// Steam asked for a password although a saved login was used.
    pub login_expired: bool,
    pub is_finished: bool,
}

impl DownloadStats {
    /// Bytes of the download present on disk: written this run plus already valid.
    pub fn completed_uncompressed_bytes(&self) -> u64 {
        self.written_bytes + self.verified_bytes
    }

    pub fn percent_complete(&self) -> f64 {
        if self.total_uncompressed_bytes == 0 {
            return 0.0;
        }
        self.completed_uncompressed_bytes() as f64 / self.total_uncompressed_bytes as f64 * 100.0
    }

    /// The compressed bytes this run has to receive: the planned total, minus
    /// the share belonging to files that were already valid on disk.
    pub fn network_total_bytes(&self) -> u64 {
        if self.total_uncompressed_bytes == 0 {
            return self.total_compressed_bytes;
        }
        let uncompressed_to_write = self
            .total_uncompressed_bytes
            .saturating_sub(self.verified_bytes);
        (self.total_compressed_bytes as u128 * uncompressed_to_write as u128
            / self.total_uncompressed_bytes as u128) as u64
    }
}

struct CounterSample {
    t_ms: u64,
    network_bytes: u64,
    completed_uncompressed_bytes: u64,
}

pub struct ProgressTracker {
    stats: DownloadStats,
    samples: VecDeque<CounterSample>,
    downloading_message: String,
    smoothed_disk_speed_bytes_per_sec: f64,
    smoothed_at_ms: u64,
    latest_t_ms: u64,
    latest_received_at: Instant,
}

impl ProgressTracker {
    pub fn new() -> Self {
        Self {
            stats: DownloadStats::default(),
            samples: VecDeque::new(),
            downloading_message: String::new(),
            smoothed_disk_speed_bytes_per_sec: 0.0,
            smoothed_at_ms: 0,
            latest_t_ms: 0,
            latest_received_at: Instant::now(),
        }
    }

    pub fn stats(&self) -> &DownloadStats {
        &self.stats
    }

    pub fn apply(&mut self, line: EventLine, received_at: Instant) {
        self.latest_t_ms = line.t_ms;
        self.latest_received_at = received_at;
        match line.event {
            Event::AuthPrompt { kind, message } => self.apply_auth_prompt(kind, message),
            Event::Qr { url } => self.stats.qr_url = Some(url),
            event => {
                // The fork prints nothing while a prompt or QR code is pending,
                // so any other event proves it is over.
                self.stats.auth_prompt = None;
                self.stats.qr_url = None;
                self.apply_progress_event(event, line.t_ms);
            }
        }
    }

    /// Recomputes speeds and ETA against the present moment, so they fall to
    /// zero when the fork stops reporting counter growth. Returns whether
    /// anything changed.
    pub fn refresh_speeds(&mut self, now: Instant) -> bool {
        let elapsed_since_latest = now.saturating_duration_since(self.latest_received_at);
        let now_ms = self.latest_t_ms + elapsed_since_latest.as_millis() as u64;
        let before = (
            self.stats.download_speed_bytes_per_sec,
            self.stats.disk_speed_bytes_per_sec,
        );
        self.update_speeds(now_ms);
        before
            != (
                self.stats.download_speed_bytes_per_sec,
                self.stats.disk_speed_bytes_per_sec,
            )
    }

    fn apply_auth_prompt(&mut self, kind: AuthPromptKind, message: String) {
        if kind == AuthPromptKind::Password {
            self.stats.login_expired = true;
            self.stats.error_message =
                Some("Your saved Steam login expired. Sign in again.".to_string());
        }
        self.stats.auth_prompt = Some(AuthPrompt { kind, message });
    }

    fn apply_progress_event(&mut self, event: Event, t_ms: u64) {
        match event {
            Event::Log { level, message } => {
                if matches!(level, LogLevel::Info | LogLevel::Warn) {
                    self.stats.status_message = message;
                }
            }
            Event::LoginSuccess { username } => self.stats.logged_in_username = username,
            Event::Plan {
                total_compressed_bytes,
                total_uncompressed_bytes,
                total_files,
            } => {
                self.stats.total_compressed_bytes = total_compressed_bytes;
                self.stats.total_uncompressed_bytes = total_uncompressed_bytes;
                self.stats.total_files = total_files;
            }
            Event::DepotStart { depot_id } => {
                self.downloading_message = format!("Downloading depot {depot_id}");
                self.stats.status_message = self.downloading_message.clone();
            }
            Event::Progress {
                network_bytes,
                written_bytes,
                verified_bytes,
                files_done,
                current_file,
            } => {
                // The fork stops logging once bytes flow, so without this the
                // last log line would go stale.
                if let Some(file) = current_file {
                    if self.stats.status_message.strip_prefix("Downloading ") != Some(&file) {
                        self.stats.status_message = format!("Downloading {file}");
                    }
                } else if network_bytes > self.stats.network_bytes
                    && self.stats.status_message != self.downloading_message
                {
                    self.stats.status_message = self.downloading_message.clone();
                }
                self.record_counters(network_bytes, written_bytes, verified_bytes, t_ms);
                self.stats.files_done = files_done;
            }
            Event::Done {
                network_bytes,
                written_bytes,
                verified_bytes,
            } => {
                self.record_counters(network_bytes, written_bytes, verified_bytes, t_ms);
                self.stats.files_done = self.stats.total_files;
                self.stats.status_message = "Download complete".to_string();
                self.stats.is_finished = true;
            }
            Event::Error { message } => self.stats.error_message = Some(message),
            Event::AuthPrompt { .. } | Event::Qr { .. } | Event::Ignored => {}
        }
    }

    fn record_counters(
        &mut self,
        network_bytes: u64,
        written_bytes: u64,
        verified_bytes: u64,
        t_ms: u64,
    ) {
        self.stats.network_bytes = network_bytes;
        self.stats.written_bytes = written_bytes;
        self.stats.verified_bytes = verified_bytes;
        self.samples.push_back(CounterSample {
            t_ms,
            network_bytes,
            completed_uncompressed_bytes: self.stats.completed_uncompressed_bytes(),
        });
        self.update_speeds(t_ms);
    }

    fn update_speeds(&mut self, now_ms: u64) {
        let window_start_ms = now_ms.saturating_sub(SPEED_WINDOW_MS);
        // Keeps the newest sample at or before the window start, so a stall
        // measures against a real reading instead of an empty window.
        while self
            .samples
            .get(1)
            .is_some_and(|sample| sample.t_ms <= window_start_ms)
        {
            self.samples.pop_front();
        }
        let Some(oldest) = self.samples.front() else {
            return;
        };
        let elapsed_ms = now_ms.saturating_sub(oldest.t_ms);
        if elapsed_ms == 0 {
            return;
        }
        let elapsed_secs = elapsed_ms as f64 / 1000.0;
        self.stats.download_speed_bytes_per_sec =
            (self.stats.network_bytes - oldest.network_bytes) as f64 / elapsed_secs;
        self.stats.disk_speed_bytes_per_sec = (self.stats.completed_uncompressed_bytes()
            - oldest.completed_uncompressed_bytes)
            as f64
            / elapsed_secs;
        self.smooth_disk_speed(now_ms);
        self.stats.eta = self.estimate_remaining_time();
    }

    /// Exponential moving average of the disk speed, so the ETA does not
    /// jump with every window's momentary rate.
    fn smooth_disk_speed(&mut self, now_ms: u64) {
        let elapsed_secs = now_ms.saturating_sub(self.smoothed_at_ms) as f64 / 1000.0;
        self.smoothed_at_ms = now_ms;
        if self.smoothed_disk_speed_bytes_per_sec == 0.0 {
            self.smoothed_disk_speed_bytes_per_sec = self.stats.disk_speed_bytes_per_sec;
            return;
        }
        let weight = 1.0 - (-elapsed_secs / ETA_SMOOTHING_SECS).exp();
        self.smoothed_disk_speed_bytes_per_sec +=
            weight * (self.stats.disk_speed_bytes_per_sec - self.smoothed_disk_speed_bytes_per_sec);
    }

    fn estimate_remaining_time(&self) -> Option<Duration> {
        let remaining_bytes = self
            .stats
            .total_uncompressed_bytes
            .saturating_sub(self.stats.completed_uncompressed_bytes());
        (self.stats.disk_speed_bytes_per_sec > 0.0).then(|| {
            Duration::from_secs_f64(remaining_bytes as f64 / self.smoothed_disk_speed_bytes_per_sec)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(t_ms: u64, event: Event) -> EventLine {
        EventLine { t_ms, event }
    }

    fn progress(network: u64, written: u64, verified: u64) -> Event {
        Event::Progress {
            network_bytes: network,
            written_bytes: written,
            verified_bytes: verified,
            files_done: 0,
            current_file: None,
        }
    }

    fn tracker_with_plan(compressed: u64, uncompressed: u64) -> (ProgressTracker, Instant) {
        let mut tracker = ProgressTracker::new();
        let now = Instant::now();
        tracker.apply(
            line(
                0,
                Event::Plan {
                    total_compressed_bytes: compressed,
                    total_uncompressed_bytes: uncompressed,
                    total_files: 3,
                },
            ),
            now,
        );
        (tracker, now)
    }

    #[test]
    fn speeds_come_from_counter_growth_over_the_window() {
        let (mut tracker, now) = tracker_with_plan(1_000_000, 2_000_000);
        tracker.apply(line(1000, progress(0, 0, 0)), now);
        tracker.apply(line(2000, progress(100_000, 400_000, 0)), now);
        let stats = tracker.stats();
        assert_eq!(stats.download_speed_bytes_per_sec, 100_000.0);
        assert_eq!(stats.disk_speed_bytes_per_sec, 400_000.0);
        assert_eq!(
            stats.eta,
            Some(Duration::from_secs_f64(1_600_000.0 / 400_000.0))
        );
    }

    #[test]
    fn speeds_fall_to_zero_when_counters_stop_growing() {
        let (mut tracker, now) = tracker_with_plan(1_000_000, 2_000_000);
        tracker.apply(line(1000, progress(0, 0, 0)), now);
        tracker.apply(line(2000, progress(100_000, 400_000, 0)), now);
        assert!(tracker.refresh_speeds(now + Duration::from_secs(3)));
        assert_eq!(tracker.stats().download_speed_bytes_per_sec, 0.0);
        assert_eq!(tracker.stats().eta, None);
    }

    #[test]
    fn network_total_excludes_already_verified_files() {
        let (mut tracker, now) = tracker_with_plan(1_000, 4_000);
        assert_eq!(tracker.stats().network_total_bytes(), 1_000);
        tracker.apply(line(10, progress(0, 0, 3_000)), now);
        assert_eq!(tracker.stats().network_total_bytes(), 250);
        assert_eq!(tracker.stats().percent_complete(), 75.0);
    }

    #[test]
    fn prompts_clear_once_anything_else_arrives() {
        let (mut tracker, now) = tracker_with_plan(1, 1);
        tracker.apply(
            line(
                1,
                Event::AuthPrompt {
                    kind: AuthPromptKind::SteamGuardCode,
                    message: "code".into(),
                },
            ),
            now,
        );
        assert!(tracker.stats().auth_prompt.is_some());
        tracker.apply(line(2, Event::Ignored), now);
        assert!(tracker.stats().auth_prompt.is_none());
    }

    #[test]
    fn a_password_prompt_means_the_saved_login_expired() {
        let (mut tracker, now) = tracker_with_plan(1, 1);
        tracker.apply(
            line(
                1,
                Event::AuthPrompt {
                    kind: AuthPromptKind::Password,
                    message: "pw".into(),
                },
            ),
            now,
        );
        assert!(tracker.stats().login_expired);
        assert!(tracker.stats().error_message.is_some());
    }
}
