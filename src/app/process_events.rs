use gpui_kit::{Context, Window};

use crate::depot_downloader::{AuthPromptKind, DownloadStats, ListPrompt, ProcessEvent, UserAppsEvent};

use super::RootView;

impl RootView {
    pub(super) fn apply_process_event(&mut self, event: ProcessEvent, cx: &mut Context<Self>) {
        match event {
            ProcessEvent::FailedToStart(message) => {
                self.run_state = super::RunState::Failed(message);
            }
            ProcessEvent::Exited(status) => match self.pending_control.take() {
                Some(super::PendingControl::Pausing) => {
                    if let super::RunState::Running(stats) = &self.run_state {
                        self.run_state = super::RunState::Paused(stats.clone());
                    }
                }
                Some(super::PendingControl::Cancelling) => {
                    self.queue_context = None;
                    self.download_queue.clear();
                    self.download_queue_total = 0;
                    self.run_state = super::RunState::Idle;
                }
                None => match &self.run_state {
                    super::RunState::Running(stats) if stats.is_finished => {
                        let stats = stats.clone();
                        self.finish_library_manifest(&stats);
                        self.continue_download_queue(stats, cx);
                    }
                    super::RunState::Running(_)
                    | super::RunState::ShowingQrCode { .. }
                    | super::RunState::AwaitingSteamGuardCode { .. }
                    | super::RunState::AwaitingSteamGuardConfirmation => {
                        self.handle_unexpected_failure(describe_unexpected_exit(status), cx);
                    }
                    _ => {}
                },
            },
            ProcessEvent::Stats(stats) => {
                if self.last_disk_phase.is_some() && self.last_disk_phase != stats.disk_phase {
                    self.speed_history.clear();
                }
                self.last_disk_phase = stats.disk_phase;
                self.speed_history
                    .push(stats.download_speed_bytes_per_sec, stats.disk_speed_bytes_per_sec);

                // Only a successful login reports a username, so a bad
                // password or rejected QR scan can never poison the remembered
                // account.
                if let Some(username) = &stats.logged_in_username
                    && self.config.logged_in_username.as_ref() != Some(username)
                {
                    self.config.logged_in_username = Some(username.clone());
                    self.config.save();
                }
                // A login succeeding - including a retry's reconnect - means
                // whatever dropped the connection before is behind us, so a
                // later unrelated drop gets its own full retry budget.
                if stats.logged_in_username.is_some() {
                    self.retry_count = 0;
                }
                if stats.login_expired {
                    self.config.logged_in_username = None;
                    self.config.save();
                }

                if let Some(message) = stats.error_message {
                    self.handle_unexpected_failure(message, cx);
                    return;
                }

                self.run_state = if let Some(url) = stats.qr_url {
                    super::RunState::ShowingQrCode { url }
                } else if let Some(prompt) = stats.auth_prompt {
                    match prompt.kind {
                        AuthPromptKind::DeviceConfirmation => {
                            super::RunState::AwaitingSteamGuardConfirmation
                        }
                        _ => super::RunState::AwaitingSteamGuardCode {
                            message: prompt.message,
                        },
                    }
                } else if stats.is_finished {
                    self.finish_library_manifest(&stats);
                    self.continue_download_queue(stats, cx);
                    return;
                } else {
                    super::RunState::Running(stats)
                };
            }
        }
    }

    /// Called once a download leg finishes: starts the next queued DLC, if
    /// any, or settles on `Finished` once the queue is drained.
    fn continue_download_queue(&mut self, stats: DownloadStats, cx: &mut Context<Self>) {
        let Some(next_app_id) = self.download_queue.pop_front() else {
            self.queue_context = None;
            self.download_queue_total = 0;
            self.run_state = super::RunState::Finished(stats);
            return;
        };
        let Some(binary) = self.depot_downloader_binary.clone() else {
            self.run_state = super::RunState::Finished(stats);
            return;
        };
        let login = self.current_login_method();
        self.launch_queue_leg(next_app_id.to_string(), login, binary, cx);
    }

    /// Handles events from a `-list-user-apps` run (`session::
    /// launch_user_apps_fetch`), reusing the login-prompt `RunState`
    /// variants a download uses (see their doc comments).
    pub(super) fn apply_user_apps_event(
        &mut self,
        event: UserAppsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            UserAppsEvent::Prompt(ListPrompt::Qr { url }) => {
                self.run_state = super::RunState::ShowingQrCode { url };
            }
            UserAppsEvent::Prompt(ListPrompt::AuthPrompt { kind, message }) => {
                self.run_state = match kind {
                    AuthPromptKind::DeviceConfirmation => {
                        super::RunState::AwaitingSteamGuardConfirmation
                    }
                    _ => super::RunState::AwaitingSteamGuardCode { message },
                };
            }
            UserAppsEvent::LoginSuccess { username } => {
                // Only a successful login reports a username, so a bad
                // password or rejected QR scan can never poison the
                // remembered account.
                if let Some(username) = username {
                    self.config.logged_in_username = Some(username);
                    self.config.save();
                }
                self.run_state = super::RunState::FetchingLibrary;
            }
            UserAppsEvent::LoginExpired => {
                self.config.logged_in_username = None;
                self.config.save();
                self.run_state = super::RunState::Failed(
                    "Your saved Steam login expired. Log in again.".to_string(),
                );
            }
            UserAppsEvent::Apps(apps) => {
                self.owned_apps = apps;
                self.rebuild_game_combobox(window, cx);
                self.run_state = super::RunState::Idle;
            }
            UserAppsEvent::Error(message) => {
                self.run_state = super::RunState::Failed(message);
            }
            UserAppsEvent::Exited => {
                self.run_state = super::RunState::Failed(
                    "DepotDownloader exited before finishing.".to_string(),
                );
            }
            UserAppsEvent::FailedToStart(message) => {
                self.run_state = super::RunState::Failed(message);
            }
        }
        cx.notify();
    }
}

fn describe_unexpected_exit(status: std::io::Result<std::process::ExitStatus>) -> String {
    match status {
        Ok(status) if !status.success() => format!("DepotDownloader exited with {status}"),
        Err(error) => format!("DepotDownloader could not be waited on: {error}"),
        Ok(_) => "DepotDownloader exited before finishing the download.".to_string(),
    }
}
