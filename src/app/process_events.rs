use gpui_kit::Context;

use crate::depot_downloader::{AuthPromptKind, ProcessEvent};

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
                    self.run_state = super::RunState::Idle;
                }
                None => match &self.run_state {
                    super::RunState::Running(stats) if stats.is_finished => {
                        let stats = stats.clone();
                        self.finish_library_manifest(&stats);
                        self.run_state = super::RunState::Finished(stats);
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
                    super::RunState::Finished(stats)
                } else {
                    super::RunState::Running(stats)
                };
            }
        }
    }
}

fn describe_unexpected_exit(status: std::io::Result<std::process::ExitStatus>) -> String {
    match status {
        Ok(status) if !status.success() => format!("DepotDownloader exited with {status}"),
        Err(error) => format!("DepotDownloader could not be waited on: {error}"),
        Ok(_) => "DepotDownloader exited before finishing the download.".to_string(),
    }
}
