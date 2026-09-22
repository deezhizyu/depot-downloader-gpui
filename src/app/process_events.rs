use crate::depot_downloader::{AuthPromptKind, ProcessEvent};

use super::RootView;

impl RootView {
    pub(super) fn apply_process_event(&mut self, event: ProcessEvent) {
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
                        self.run_state = super::RunState::Failed(describe_unexpected_exit(status));
                    }
                    _ => {}
                },
            },
            ProcessEvent::Stats(stats) => {
                // Only a successful login reports a username, so a bad
                // password or rejected QR scan can never poison the remembered
                // account.
                if let Some(username) = &stats.logged_in_username
                    && self.config.logged_in_username.as_ref() != Some(username)
                {
                    self.config.logged_in_username = Some(username.clone());
                    self.config.save();
                }
                if stats.login_expired {
                    self.config.logged_in_username = None;
                    self.config.save();
                }

                self.run_state = if let Some(message) = stats.error_message {
                    super::RunState::Failed(message)
                } else if let Some(url) = stats.qr_url {
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
