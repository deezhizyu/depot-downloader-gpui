use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::parser::DownloadEvent;

/// How much history to keep for smoothing the speed/ETA calculations. A
/// window of a few seconds absorbs the burstiness of chunked downloads
/// without lagging the display noticeably behind reality.
const SAMPLE_WINDOW: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthPrompt {
    Code,
    EmailCode { email: String },
    Confirmation,
}

#[derive(Debug, Clone, Default)]
pub struct DownloadStats {
    pub status_message: String,
    pub depots_seen: u32,
    pub current_depot_id: Option<u64>,
    /// The current depot's own completion percentage (0-100). DepotDownloader
    /// resets this per depot, so it is not the whole run's percentage when an
    /// app has more than one depot to fetch.
    pub current_depot_percent: f32,
    /// Bytes written to the download directory right now, measured directly
    /// from disk rather than parsed from CLI output (DepotDownloader never
    /// prints a running byte total).
    pub downloaded_bytes: u64,
    pub disk_speed_bytes_per_sec: f64,
    /// Estimated network throughput: measured disk speed scaled by the
    /// compressed/uncompressed ratio observed on depots finished so far
    /// (DepotDownloader reports both figures once a depot completes). Until a
    /// depot has finished, network and disk bytes are assumed equal.
    pub download_speed_bytes_per_sec: f64,
    pub eta: Option<Duration>,
    pub auth_prompt: Option<AuthPrompt>,
    pub qr_code_ascii_art: Option<String>,
    pub error_message: Option<String>,
    pub is_finished: bool,
    pub final_compressed_bytes: Option<u64>,
    pub final_uncompressed_bytes: Option<u64>,
}

pub struct ProgressTracker {
    stats: DownloadStats,
    percent_samples: VecDeque<(Instant, f32)>,
    disk_samples: VecDeque<(Instant, u64)>,
    compressed_bytes_from_finished_depots: u64,
    uncompressed_bytes_from_finished_depots: u64,
}

impl ProgressTracker {
    pub fn new() -> Self {
        Self {
            stats: DownloadStats::default(),
            percent_samples: VecDeque::new(),
            disk_samples: VecDeque::new(),
            compressed_bytes_from_finished_depots: 0,
            uncompressed_bytes_from_finished_depots: 0,
        }
    }

    pub fn stats(&self) -> &DownloadStats {
        &self.stats
    }

    pub fn apply_event(&mut self, event: DownloadEvent) {
        match event {
            DownloadEvent::UsingBranch { branch } => {
                self.stats.status_message = format!("Using branch '{branch}'");
            }
            DownloadEvent::ProcessingDepot { depot_id } => {
                self.stats.depots_seen += 1;
                self.stats.current_depot_id = Some(depot_id);
                self.stats.current_depot_percent = 0.0;
                self.percent_samples.clear();
                self.stats.status_message = format!("Processing depot {depot_id}");
            }
            DownloadEvent::DownloadingDepotManifest { depot_id } => {
                self.stats.status_message = format!("Downloading manifest for depot {depot_id}");
            }
            DownloadEvent::DownloadingDepot { depot_id } => {
                self.stats.current_depot_id = Some(depot_id);
                self.stats.status_message = format!("Downloading depot {depot_id}");
            }
            DownloadEvent::FileProgress { percent, path } => {
                self.stats.current_depot_percent = percent;
                self.stats.status_message = format!("{percent:.2}% - {path}");
                self.record_percent_sample(percent);
            }
            DownloadEvent::OverallProgress { percent } => {
                // DepotDownloader only emits this when stdout is a real
                // terminal, which is never true once we pipe it for parsing.
                // Kept as a fallback in case that assumption ever changes.
                self.stats.current_depot_percent = percent as f32;
                self.record_percent_sample(percent as f32);
            }
            DownloadEvent::DepotFinished {
                depot_id,
                compressed_bytes,
                uncompressed_bytes,
            } => {
                self.compressed_bytes_from_finished_depots += compressed_bytes;
                self.uncompressed_bytes_from_finished_depots += uncompressed_bytes;
                self.stats.status_message = format!("Finished depot {depot_id}");
                self.recompute_download_speed();
            }
            DownloadEvent::TotalDownloaded {
                compressed_bytes,
                uncompressed_bytes,
                depot_count,
            } => {
                self.stats.is_finished = true;
                self.stats.final_compressed_bytes = Some(compressed_bytes);
                self.stats.final_uncompressed_bytes = Some(uncompressed_bytes);
                self.stats.eta = Some(Duration::ZERO);
                self.stats.status_message = format!("Done - {depot_count} depot(s) downloaded");
            }
            DownloadEvent::SteamGuardCodeRequested => {
                self.stats.auth_prompt = Some(AuthPrompt::Code);
            }
            DownloadEvent::SteamGuardEmailCodeRequested { email } => {
                self.stats.auth_prompt = Some(AuthPrompt::EmailCode { email });
            }
            DownloadEvent::SteamGuardConfirmationRequested => {
                self.stats.auth_prompt = Some(AuthPrompt::Confirmation);
            }
            DownloadEvent::QrCodeReady { ascii_art } => {
                self.stats.qr_code_ascii_art = Some(ascii_art);
            }
            DownloadEvent::ErrorLine { message } => {
                self.stats.error_message = Some(message);
            }
        }
    }

    /// Feeds in the current size, in bytes, of everything DepotDownloader has
    /// written to the download directory so far. Called periodically by the
    /// process supervisor, which is the only place that actually knows the
    /// destination path.
    pub fn apply_disk_sample(&mut self, bytes_on_disk: u64, at: Instant) {
        self.stats.downloaded_bytes = bytes_on_disk;
        self.disk_samples.push_back((at, bytes_on_disk));
        while let Some(&(sample_time, _)) = self.disk_samples.front() {
            if at.duration_since(sample_time) > SAMPLE_WINDOW {
                self.disk_samples.pop_front();
            } else {
                break;
            }
        }

        if let (Some(&(t0, b0)), Some(&(t1, b1))) =
            (self.disk_samples.front(), self.disk_samples.back())
        {
            let elapsed = t1.duration_since(t0).as_secs_f64();
            if elapsed > 0.0 && b1 >= b0 {
                self.stats.disk_speed_bytes_per_sec = (b1 - b0) as f64 / elapsed;
            }
        }
        self.recompute_download_speed();
        self.recompute_eta();
    }

    fn record_percent_sample(&mut self, percent: f32) {
        let now = Instant::now();
        self.percent_samples.push_back((now, percent));
        while let Some(&(sample_time, _)) = self.percent_samples.front() {
            if now.duration_since(sample_time) > SAMPLE_WINDOW {
                self.percent_samples.pop_front();
            } else {
                break;
            }
        }
        self.recompute_eta();
    }

    fn recompute_download_speed(&mut self) {
        let compression_ratio = if self.uncompressed_bytes_from_finished_depots > 0 {
            self.compressed_bytes_from_finished_depots as f64
                / self.uncompressed_bytes_from_finished_depots as f64
        } else {
            1.0
        };
        self.stats.download_speed_bytes_per_sec =
            self.stats.disk_speed_bytes_per_sec * compression_ratio;
    }

    fn recompute_eta(&mut self) {
        let (Some(&(t0, p0)), Some(&(t1, p1))) =
            (self.percent_samples.front(), self.percent_samples.back())
        else {
            return;
        };
        let elapsed = t1.duration_since(t0).as_secs_f32();
        let percent_gained = p1 - p0;
        if elapsed <= 0.0 || percent_gained <= 0.0 {
            return;
        }
        let percent_per_sec = percent_gained / elapsed;
        let remaining_percent = (100.0 - p1).max(0.0);
        self.stats.eta = Some(Duration::from_secs_f32(remaining_percent / percent_per_sec));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_current_depot_percent_from_file_progress() {
        let mut tracker = ProgressTracker::new();
        tracker.apply_event(DownloadEvent::ProcessingDepot { depot_id: 1 });
        tracker.apply_event(DownloadEvent::FileProgress {
            percent: 50.0,
            path: "a".into(),
        });
        assert_eq!(tracker.stats().current_depot_percent, 50.0);
        assert_eq!(tracker.stats().current_depot_id, Some(1));
    }

    #[test]
    fn disk_speed_comes_from_disk_samples_not_cli_output() {
        let mut tracker = ProgressTracker::new();
        let start = Instant::now();
        tracker.apply_disk_sample(0, start);
        tracker.apply_disk_sample(10_000_000, start + Duration::from_secs(1));
        assert!(tracker.stats().disk_speed_bytes_per_sec > 0.0);
        assert_eq!(tracker.stats().downloaded_bytes, 10_000_000);
    }

    #[test]
    fn download_speed_uses_compression_ratio_once_known() {
        let mut tracker = ProgressTracker::new();
        let start = Instant::now();
        tracker.apply_disk_sample(0, start);
        tracker.apply_event(DownloadEvent::DepotFinished {
            depot_id: 1,
            compressed_bytes: 50,
            uncompressed_bytes: 100,
        });
        tracker.apply_disk_sample(10_000_000, start + Duration::from_secs(1));
        let stats = tracker.stats();
        assert!(
            (stats.download_speed_bytes_per_sec - stats.disk_speed_bytes_per_sec * 0.5).abs() < 1.0
        );
    }

    #[test]
    fn total_downloaded_marks_run_finished() {
        let mut tracker = ProgressTracker::new();
        tracker.apply_event(DownloadEvent::TotalDownloaded {
            compressed_bytes: 100,
            uncompressed_bytes: 200,
            depot_count: 1,
        });
        assert!(tracker.stats().is_finished);
        assert_eq!(tracker.stats().final_uncompressed_bytes, Some(200));
    }
}
