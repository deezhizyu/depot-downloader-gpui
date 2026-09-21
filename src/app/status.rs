use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::*;

use crate::depot_downloader::DownloadStats;
use crate::ui::{format_bytes, format_eta, format_speed};

use super::RootView;
use super::qr_code::render_qr_code;
use super::state::{PendingControl, RunState};

impl RootView {
    pub(super) fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.run_state {
            RunState::PreparingDepotDownloader => status_line("Setting up DepotDownloader…", cx),
            RunState::LookingUpApp => status_line("Looking up app…", cx),
            RunState::Idle => status_line("Download progress appears here.", cx),
            RunState::ShowingQrCode { url } => v_flex()
                .gap_2()
                .child("Scan this with the Steam Mobile app:")
                .child(render_qr_code(url))
                .into_any_element(),
            RunState::AwaitingSteamGuardCode { message } => {
                self.render_guard_code_prompt(message, cx)
            }
            RunState::AwaitingSteamGuardConfirmation => {
                status_line("Confirm this sign-in in the Steam Mobile app…", cx)
            }
            RunState::Running(stats) => v_flex()
                .gap_3()
                .child(render_progress(stats, cx))
                .child(self.render_running_controls(cx))
                .into_any_element(),
            RunState::Paused(stats) => v_flex()
                .gap_3()
                .child(render_progress(stats, cx))
                .child(self.render_paused_controls(cx))
                .into_any_element(),
            RunState::Finished(stats) => render_progress(stats, cx),
            RunState::Failed(message) => div()
                .text_color(cx.theme().colors.danger)
                .child(message.clone())
                .into_any_element(),
        }
    }

    fn render_running_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let is_pending = self.pending_control.is_some();
        h_flex()
            .gap_2()
            .child(
                Button::new("pause-download")
                    .secondary()
                    .label(
                        if matches!(self.pending_control, Some(PendingControl::Pausing)) {
                            "Pausing…"
                        } else {
                            "Pause"
                        },
                    )
                    .disabled(is_pending)
                    .on_click(cx.listener(|view, _, _, cx| view.request_pause(cx))),
            )
            .child(self.render_cancel_button(cx))
            .into_any_element()
    }

    fn render_paused_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .gap_2()
            .child(
                Button::new("resume-download")
                    .primary()
                    .label("Resume")
                    .disabled(self.pending_control.is_some())
                    .on_click(cx.listener(|view, _, _, cx| view.resume_download(cx))),
            )
            .child(self.render_cancel_button(cx))
            .into_any_element()
    }

    /// The confirmation is a plain `window.open_alert_dialog` (gpui-kit's
    /// existing dialog component, not a bespoke one) rather than cancelling
    /// on the first click: cancelling discards the running process outright,
    /// unlike Pause, so it warrants one extra step before it happens.
    fn render_cancel_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_pending = self.pending_control.is_some();
        Button::new("cancel-download")
            .danger()
            .label(
                if matches!(self.pending_control, Some(PendingControl::Cancelling)) {
                    "Cancelling…"
                } else {
                    "Cancel"
                },
            )
            .disabled(is_pending)
            .on_click(cx.listener(|_view, _, window, cx| {
                let entity = cx.entity();
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let entity = entity.clone();
                    alert
                        .title("Cancel this download?")
                        .description(
                            "DepotDownloader will stop right away. Anything already \
                             downloaded stays on disk.",
                        )
                        .show_cancel(true)
                        .on_ok(move |_, _, cx| {
                            entity.update(cx, |view, cx| view.request_cancel(cx));
                            true
                        })
                });
            }))
    }

    fn render_guard_code_prompt(&self, label: &str, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_2()
            .child(label.to_string())
            .child(Input::new(&self.guard_code_input))
            .child(
                Button::new("submit-guard-code")
                    .primary()
                    .label("Submit")
                    .on_click(cx.listener(|view, _, _, cx| view.submit_guard_code(cx))),
            )
            .into_any_element()
    }
}

fn status_line(message: &str, cx: &mut Context<RootView>) -> AnyElement {
    div()
        .text_color(cx.theme().colors.muted_foreground)
        .child(message.to_string())
        .into_any_element()
}

fn render_progress(stats: &DownloadStats, cx: &App) -> AnyElement {
    v_flex()
        .gap_2()
        .child(
            div()
                .h(px(96.0))
                .p_2()
                .overflow_hidden()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().input)
                .bg(cx.theme().input_background())
                .child(stats.status_message.clone()),
        )
        .child(Progress::new("download-progress").value(stats.percent_complete() as f32))
        .child(
            h_flex()
                .justify_between()
                .child(format!("Progress: {:.1}%", stats.percent_complete()))
                .child(format!("ETA: {}", format_eta(stats.eta))),
        )
        .child(format!(
            "Downloaded: {} of {}",
            format_bytes(stats.network_bytes),
            format_bytes(stats.network_total_bytes())
        ))
        .child(format!(
            "Written to disk: {} of {}",
            format_bytes(stats.completed_uncompressed_bytes()),
            format_bytes(stats.total_uncompressed_bytes)
        ))
        .child(format!(
            "Files: {} of {}",
            stats.files_done, stats.total_files
        ))
        .child(
            h_flex()
                .justify_between()
                .child(format!(
                    "Download: {}",
                    format_speed(stats.download_speed_bytes_per_sec)
                ))
                .child(format!(
                    "Disk: {}",
                    format_speed(stats.disk_speed_bytes_per_sec)
                )),
        )
        .into_any_element()
}
