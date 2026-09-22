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
    Idle,
    Running(DownloadStats),
    ShowingQrCode {
        url: String,
    },
    AwaitingSteamGuardCode {
        message: String,
    },
    AwaitingSteamGuardConfirmation,
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
