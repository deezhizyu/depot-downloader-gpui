use std::path::PathBuf;

use crate::depot_downloader::DownloadStats;
use crate::steam::AppInfo;

/// Captured when a download starts, from the library folder and app info
/// looked up then - so a finished download can write a Steam
/// `appmanifest_*.acf` without re-deriving anything.
pub(super) struct PendingLibraryManifest {
    pub steamapps_dir: PathBuf,
    pub app_id: String,
    pub app_info: AppInfo,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LoginMode {
    UsernamePassword,
    Qr,
}

pub(super) enum RunState {
    PreparingDepotDownloader,
    /// Between clicking Download and launching DepotDownloader: the app's
    /// install folder and sizes are being looked up. No process exists yet, so
    /// there is nothing for Pause/Cancel to act on.
    LookingUpApp,
    /// A `-list-user-apps` run is underway (see `session::fetch_user_apps`)
    /// and hasn't hit a login prompt yet. `ShowingQrCode`/
    /// `AwaitingSteamGuardCode`/`AwaitingSteamGuardConfirmation` below are
    /// shared with the download flow - nothing about them is download-
    /// specific - so a login prompt during this fetch reuses them as-is;
    /// `pending_action` on `RootView` says what happens once they resolve.
    FetchingLibrary,
    Idle,
    Running(DownloadStats),
    ShowingQrCode {
        url: String,
    },
    AwaitingSteamGuardCode {
        message: String,
    },
    AwaitingSteamGuardConfirmation,
    /// DepotDownloader exited unexpectedly (a dropped Steam connection is the
    /// common cause) and a retry is queued; `attempt` is 1-based against
    /// `session::MAX_RETRIES`.
    Reconnecting {
        attempt: u32,
    },
    /// The process was stopped by our own `request_pause`, not by
    /// DepotDownloader itself - `stats` is the last snapshot before it
    /// stopped. `resume_download` relaunches with the remembered login,
    /// which DepotDownloader treats as a normal continuation: it re-verifies
    /// existing files against the manifest and only re-fetches what's
    /// missing or invalid.
    Paused(DownloadStats),
    Finished(DownloadStats),
    Failed(String),
}

/// What an early process exit means, set right before we ask the running
/// process to stop so `apply_process_event` can tell a user-requested stop
/// apart from DepotDownloader exiting on its own.
pub(super) enum PendingControl {
    Pausing,
    Cancelling,
}

/// What the shared login-prompt states (`RunState::ShowingQrCode` etc.) are
/// presently for, so `submit_guard_code` and the QR/login-success handling
/// in `process_events.rs` know whether to continue into a download or into
/// populating the owned-games list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingAction {
    Download,
    FetchLibrary,
}

/// The parts of a download queue (base game + checked DLCs) that stay the
/// same across every leg - only the app id and, after the first leg's
/// login succeeds, the login method change. Set once in `start_download`
/// and consumed by `session::launch_queue_leg` for each leg in turn.
pub(super) struct QueueContext {
    pub library_dir: Option<std::path::PathBuf>,
    pub max_downloads: Option<u32>,
    pub branch: Option<String>,
    pub add_to_steam_library: bool,
}
