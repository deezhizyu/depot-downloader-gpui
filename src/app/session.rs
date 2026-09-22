use std::path::Path;
use std::path::PathBuf;

use gpui_kit::*;

use crate::depot_downloader::process;
use crate::depot_downloader::{DownloadRequest, DownloadStats, LoginMethod, provisioning};
use crate::steam;

use super::RootView;
use super::state::{LoginMode, PendingControl, PendingLibraryManifest, RunState};

impl RootView {
    pub(super) fn start_provisioning(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async { provisioning::ensure_binary() })
                .await;
            let _ = this.update(cx, |view, cx| {
                match outcome {
                    Ok(path) => {
                        view.depot_downloader_binary = Some(path);
                        view.run_state = RunState::Idle;
                    }
                    Err(error) => {
                        view.run_state =
                            RunState::Failed(format!("Could not set up DepotDownloader: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn start_download(&mut self, cx: &mut Context<Self>) {
        let Some(binary) = self.depot_downloader_binary.clone() else {
            return;
        };

        let app_id = steam::parse_app_id(&self.app_id_input.read(cx).value());
        if app_id.is_empty() {
            self.run_state = RunState::Failed("Enter a Steam app id first.".to_string());
            cx.notify();
            return;
        }

        let library_dir = {
            let raw = self.download_dir_input.read(cx).value();
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
        };

        let max_downloads_text = self.max_downloads_input.read(cx).value().trim().to_string();
        let max_downloads = if max_downloads_text.is_empty() {
            None
        } else {
            match max_downloads_text.parse::<u32>() {
                Ok(count) if count > 0 => Some(count),
                _ => {
                    self.run_state = RunState::Failed(
                        "Max downloads must be a whole number greater than 0.".to_string(),
                    );
                    cx.notify();
                    return;
                }
            }
        };

        let login = if let Some(username) = self.config.logged_in_username.clone() {
            LoginMethod::RememberedUsername { username }
        } else {
            match self.login_mode {
                LoginMode::UsernamePassword => LoginMethod::UsernamePassword {
                    username: self.username_input.read(cx).value().trim().to_string(),
                    password: self.password_input.read(cx).value().to_string(),
                },
                LoginMode::Qr => LoginMethod::Qr,
            }
        };

        self.config.last_app_id = app_id.clone();
        self.config.last_download_location = library_dir.clone();
        self.config.last_max_downloads = max_downloads_text;
        self.config.save();

        let add_to_steam_library = self.add_to_steam_library;

        self.run_state = RunState::LookingUpApp;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (download_dir, library_manifest) = match library_dir {
                Some(common_dir) => {
                    let lookup_app_id = app_id.clone();
                    let app_info = cx
                        .background_executor()
                        .spawn(async move { steam::fetch_app_info(&lookup_app_id) })
                        .await;
                    let download_dir = common_dir.join(&app_info.install_dir_name);
                    let library_manifest = (add_to_steam_library
                        && steam::is_steam_library_common_dir(&common_dir))
                    .then(|| common_dir.parent().map(|steamapps_dir| PendingLibraryManifest {
                        steamapps_dir: steamapps_dir.to_path_buf(),
                        app_id: app_id.clone(),
                        app_info,
                    }))
                    .flatten();
                    (Some(download_dir), library_manifest)
                }
                None => (None, None),
            };
            let request = DownloadRequest {
                depot_downloader_binary: binary,
                app_id,
                download_dir,
                max_downloads,
                login,
            };
            let _ = this.update(cx, |view, cx| {
                view.library_manifest = library_manifest;
                view.launch(request, cx);
            });
        })
        .detach();
    }

    /// Relaunches the download that was paused, reusing everything about it
    /// except the login method: DepotDownloader is asked to reconnect with
    /// the remembered login rather than repeating the original QR scan or
    /// password entry (see `RunState::Paused`'s doc comment for why that's a
    /// correct continuation, not just a restart).
    pub(super) fn resume_download(&mut self, cx: &mut Context<Self>) {
        let Some(mut request) = self.last_request.clone() else {
            return;
        };
        let Some(username) = self.config.logged_in_username.clone() else {
            return;
        };
        request.login = LoginMethod::RememberedUsername { username };
        self.launch(request, cx);
    }

    /// Wires up the channels for one DepotDownloader run and spawns both the
    /// process supervisor and the task that forwards its events into
    /// `apply_process_event`. Shared by `start_download` and `resume_download`
    /// so channel setup isn't duplicated between them.
    fn launch(&mut self, request: DownloadRequest, cx: &mut Context<Self>) {
        self.last_request = Some(request.clone());

        let (update_tx, update_rx) = async_channel::unbounded();
        let (respond_tx, respond_rx) = async_channel::unbounded();
        let (cancel_tx, cancel_rx) = async_channel::unbounded();
        self.respond_sender = Some(respond_tx);
        self.cancel_sender = Some(cancel_tx);
        self.pending_control = None;
        self.run_state = RunState::Running(DownloadStats::default());

        cx.background_executor()
            .spawn(process::run(request, update_tx, respond_rx, cancel_rx))
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(event) = update_rx.recv().await {
                let updated = this.update(cx, |view, cx| {
                    view.apply_process_event(event);
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();

        cx.notify();
    }

    fn send_control_signal(&self) {
        if let Some(sender) = &self.cancel_sender {
            let _ = sender.try_send(());
        }
    }

    pub(super) fn request_pause(&mut self, cx: &mut Context<Self>) {
        self.send_control_signal();
        self.pending_control = Some(PendingControl::Pausing);
        cx.notify();
    }

    pub(super) fn request_cancel(&mut self, cx: &mut Context<Self>) {
        // A paused download has no process left to signal; waiting for an exit
        // event that will never come would leave the UI stuck on "Cancelling…".
        if matches!(self.run_state, RunState::Paused(_)) {
            self.run_state = RunState::Idle;
            cx.notify();
            return;
        }
        self.send_control_signal();
        self.pending_control = Some(PendingControl::Cancelling);
        cx.notify();
    }

    /// Forgets the remembered login: clears it from `Config` and best-effort
    /// deletes DepotDownloader's own `account.config` (see `process::run`'s
    /// `current_dir`) so a stale token can't be reused even if something
    /// later reconstructs a `RememberedUsername` request.
    pub(super) fn logout(&mut self, cx: &mut Context<Self>) {
        self.config.logged_in_username = None;
        self.config.save();
        if let Some(install_dir) = self
            .depot_downloader_binary
            .as_deref()
            .and_then(Path::parent)
        {
            let _ = std::fs::remove_file(install_dir.join("account.config"));
        }
        cx.notify();
    }

    /// Writes the Steam appmanifest for the download that just finished, if
    /// `add_to_steam_library` was on at launch. Best-effort: a failure here
    /// leaves the completed download itself untouched, so it only goes to
    /// the console rather than the UI.
    pub(super) fn finish_library_manifest(&mut self, stats: &DownloadStats) {
        self.steam_library_manifest_written = false;
        let Some(pending) = self.library_manifest.take() else {
            return;
        };
        match steam::write_app_manifest(
            &pending.steamapps_dir,
            &pending.app_id,
            &pending.app_info,
            stats.total_uncompressed_bytes,
            &stats.depot_ids,
        ) {
            Ok(()) => self.steam_library_manifest_written = true,
            Err(error) => eprintln!(
                "Could not add app {} to the Steam library: {error}",
                pending.app_id
            ),
        }
    }

    pub(super) fn submit_guard_code(&mut self, cx: &mut Context<Self>) {
        let code = self.guard_code_input.read(cx).value().trim().to_string();
        let Some(sender) = &self.respond_sender else {
            return;
        };
        if code.is_empty() {
            return;
        }
        let _ = sender.try_send(code);
        self.run_state = RunState::Running(DownloadStats::default());
        cx.notify();
    }
}
