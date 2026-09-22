use std::time::Duration;

const KIB: f64 = 1024.0;
const MIB: f64 = KIB * 1024.0;
const GIB: f64 = MIB * 1024.0;

pub fn format_bytes(bytes: u64) -> String {
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.2} GB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.2} MB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}

pub fn format_speed(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec.max(0.0) as u64))
}

pub fn format_eta(eta: Option<Duration>) -> String {
    let Some(eta) = eta else {
        return "Calculating…".to_string();
    };
    let total_seconds = eta.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// Fixed-width elapsed time (`1:02:03` or `2:03`), unlike `format_eta`'s
/// rounded-to-the-largest-unit style - an elapsed counter is expected to
/// tick every second, so it always shows seconds.
pub fn format_elapsed(elapsed: Duration) -> String {
    let total_seconds = elapsed.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_at_the_right_scale() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.00 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    #[test]
    fn eta_falls_back_when_unknown() {
        assert_eq!(format_eta(None), "Calculating…");
        assert_eq!(format_eta(Some(Duration::from_secs(45))), "45s");
        assert_eq!(format_eta(Some(Duration::from_secs(125))), "2m 5s");
        assert_eq!(format_eta(Some(Duration::from_secs(3700))), "1h 1m");
    }

    #[test]
    fn elapsed_always_shows_seconds() {
        assert_eq!(format_elapsed(Duration::from_secs(45)), "0:45");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2:05");
        assert_eq!(format_elapsed(Duration::from_secs(3700)), "1:01:40");
    }
}
