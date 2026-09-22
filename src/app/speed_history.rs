use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The span shown on the chart.
pub const SPEED_HISTORY_SPAN: Duration = Duration::from_secs(120);
/// Extra history retained beyond the visible span: the chart's line and its
/// EMA smoothing both extend slightly past the visible left edge (and get
/// clipped there), so pruning the oldest sample never visibly pops the line,
/// and the smoothing has already converged by the time it enters view.
const HISTORY_PADDING: Duration = Duration::from_secs(16);
/// One entry per second: DepotDownloader's per-event speed is noisy enough
/// that logging every event would just be noise to smooth back out, so each
/// entry is instead the average of every value seen during its second.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// Samples are dropped for this long after a reset: the moment right after a
/// fresh start or a phase change is a ramp up from zero, not the steady-state
/// speed, and would otherwise dominate the graph's scale.
const STARTUP_TRIM: Duration = Duration::from_secs(5);

pub struct SpeedSample {
    pub at: Instant,
    pub download_bytes_per_sec: f64,
    pub disk_bytes_per_sec: f64,
}

/// The in-progress average for the current second.
struct PendingSample {
    started_at: Instant,
    download_sum: f64,
    disk_sum: f64,
    count: u32,
}

pub struct SpeedHistory {
    samples: VecDeque<SpeedSample>,
    reset_at: Instant,
    pending: Option<PendingSample>,
}

impl Default for SpeedHistory {
    fn default() -> Self {
        Self {
            samples: VecDeque::new(),
            reset_at: Instant::now(),
            pending: None,
        }
    }
}

impl SpeedHistory {
    pub fn push(&mut self, download_bytes_per_sec: f64, disk_bytes_per_sec: f64) {
        let now = Instant::now();
        if now.duration_since(self.reset_at) < STARTUP_TRIM {
            return;
        }

        let pending = self.pending.get_or_insert_with(|| PendingSample {
            started_at: now,
            download_sum: 0.0,
            disk_sum: 0.0,
            count: 0,
        });
        pending.download_sum += download_bytes_per_sec;
        pending.disk_sum += disk_bytes_per_sec;
        pending.count += 1;
        if now.duration_since(pending.started_at) < SAMPLE_INTERVAL {
            return;
        }

        let count = f64::from(pending.count);
        self.samples.push_back(SpeedSample {
            at: now,
            download_bytes_per_sec: pending.download_sum / count,
            disk_bytes_per_sec: pending.disk_sum / count,
        });
        self.pending = None;

        let retention = SPEED_HISTORY_SPAN + HISTORY_PADDING;
        while self
            .samples
            .front()
            .is_some_and(|sample| now.duration_since(sample.at) > retention)
        {
            self.samples.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.reset_at = Instant::now();
        self.pending = None;
    }

    pub fn samples(&self) -> &VecDeque<SpeedSample> {
        &self.samples
    }
}
