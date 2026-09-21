use std::path::{Path, PathBuf};

use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Root, TitleBar, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use qrcode::{Color, QrCode};

use crate::config::Config;
use crate::depot_downloader::process;
use crate::depot_downloader::{
    AuthPromptKind, DownloadRequest, DownloadStats, LoginMethod, ProcessEvent, provisioning,
};
use crate::steam;
use crate::ui::{format_bytes, format_eta, format_speed};

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoginMode {
    UsernamePassword,
    Qr,
}

enum RunState {
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
enum PendingControl {
    Pausing,
    Cancelling,
}

pub struct RootView {
    config: Config,
    login_mode: LoginMode,
    username_input: Entity<InputState>,
    password_input: Entity<InputState>,
    app_id_input: Entity<InputState>,
    download_dir_input: Entity<InputState>,
    max_downloads_input: Entity<InputState>,
    guard_code_input: Entity<InputState>,
    depot_downloader_binary: Option<PathBuf>,
    run_state: RunState,
    respond_sender: Option<async_channel::Sender<String>>,
    cancel_sender: Option<async_channel::Sender<()>>,
    pending_control: Option<PendingControl>,
    /// The request behind the currently running/paused download, so
    /// `resume_download` can relaunch it without asking the user to fill the
    /// form in again.
    last_request: Option<DownloadRequest>,
}

impl RootView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = Config::load();

        let username_input = cx.new(|cx| InputState::new(window, cx).placeholder("Steam username"));
        let password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Steam password")
                .masked(true)
        });
        let app_id_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Steam app id, e.g. 440")
                .default_value(config.last_app_id.clone())
        });

        let default_download_dir = config
            .last_download_location
            .clone()
            .or_else(steam::default_steam_common_dir);
        let download_dir_input = cx.new(|cx| {
            let state = InputState::new(window, cx).placeholder("Download location");
            match &default_download_dir {
                Some(dir) => state.default_value(dir.to_string_lossy().into_owned()),
                None => state,
            }
        });
        let max_downloads_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Concurrent downloads, default 8")
                .default_value(config.last_max_downloads.clone())
        });
        let guard_code_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Steam Guard code"));

        let view = Self {
            config,
            login_mode: LoginMode::UsernamePassword,
            username_input,
            password_input,
            app_id_input,
            download_dir_input,
            max_downloads_input,
            guard_code_input,
            depot_downloader_binary: None,
            run_state: RunState::PreparingDepotDownloader,
            respond_sender: None,
            cancel_sender: None,
            pending_control: None,
            last_request: None,
        };
        view.start_provisioning(cx);
        view
    }

    fn start_provisioning(&self, cx: &mut Context<Self>) {
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

    fn start_download(&mut self, cx: &mut Context<Self>) {
        let Some(binary) = self.depot_downloader_binary.clone() else {
            return;
        };

        let app_id = self.app_id_input.read(cx).value().trim().to_string();
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

        self.run_state = RunState::LookingUpApp;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let lookup_app_id = app_id.clone();
            let install_dir_name = cx
                .background_executor()
                .spawn(async move { steam::fetch_install_dir_name(&lookup_app_id) })
                .await;
            let request = DownloadRequest {
                depot_downloader_binary: binary,
                app_id,
                download_dir: library_dir.map(|dir| dir.join(install_dir_name)),
                max_downloads,
                login,
            };
            let _ = this.update(cx, |view, cx| view.launch(request, cx));
        })
        .detach();
    }

    /// Relaunches the download that was paused, reusing everything about it
    /// except the login method: DepotDownloader is asked to reconnect with
    /// the remembered login rather than repeating the original QR scan or
    /// password entry (see `RunState::Paused`'s doc comment for why that's a
    /// correct continuation, not just a restart).
    fn resume_download(&mut self, cx: &mut Context<Self>) {
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

    fn send_control_signal(&self, cx: &mut Context<Self>) {
        let Some(sender) = self.cancel_sender.clone() else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let _ = sender.send(()).await;
            })
            .detach();
    }

    fn request_pause(&mut self, cx: &mut Context<Self>) {
        self.send_control_signal(cx);
        self.pending_control = Some(PendingControl::Pausing);
        cx.notify();
    }

    fn request_cancel(&mut self, cx: &mut Context<Self>) {
        // A paused download has no process left to signal; waiting for an exit
        // event that will never come would leave the UI stuck on "Cancelling…".
        if matches!(self.run_state, RunState::Paused(_)) {
            self.run_state = RunState::Idle;
            cx.notify();
            return;
        }
        self.send_control_signal(cx);
        self.pending_control = Some(PendingControl::Cancelling);
        cx.notify();
    }

    /// Forgets the remembered login: clears it from `Config` and best-effort
    /// deletes DepotDownloader's own `account.config` (see `process::run`'s
    /// `current_dir`) so a stale token can't be reused even if something
    /// later reconstructs a `RememberedUsername` request.
    fn logout(&mut self, cx: &mut Context<Self>) {
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

    fn apply_process_event(&mut self, event: ProcessEvent) {
        match event {
            ProcessEvent::FailedToStart(message) => {
                self.run_state = RunState::Failed(message);
            }
            ProcessEvent::Exited(status) => match self.pending_control.take() {
                Some(PendingControl::Pausing) => {
                    if let RunState::Running(stats) = &self.run_state {
                        self.run_state = RunState::Paused(stats.clone());
                    }
                }
                Some(PendingControl::Cancelling) => {
                    self.run_state = RunState::Idle;
                }
                None => {
                    if let RunState::Running(stats) = &self.run_state {
                        self.run_state = if stats.is_finished {
                            RunState::Finished(stats.clone())
                        } else {
                            RunState::Failed(describe_unexpected_exit(status))
                        };
                    }
                }
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

                self.run_state = if let Some(url) = stats.qr_url {
                    RunState::ShowingQrCode { url }
                } else if let Some(prompt) = stats.auth_prompt {
                    match prompt.kind {
                        AuthPromptKind::DeviceConfirmation => {
                            RunState::AwaitingSteamGuardConfirmation
                        }
                        _ => RunState::AwaitingSteamGuardCode {
                            message: prompt.message,
                        },
                    }
                } else if let Some(message) = stats.error_message {
                    RunState::Failed(message)
                } else if stats.is_finished {
                    RunState::Finished(stats)
                } else {
                    RunState::Running(stats)
                };
            }
        }
    }

    fn browse_for_download_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a library folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.pop() else { return };
            let _ = this.update_in(cx, |view, window, cx| {
                view.download_dir_input.update(cx, |state, cx| {
                    state.set_value(path.to_string_lossy().into_owned(), window, cx);
                });
            });
        })
        .detach();
    }

    fn submit_guard_code(&mut self, cx: &mut Context<Self>) {
        let code = self.guard_code_input.read(cx).value().trim().to_string();
        let Some(sender) = self.respond_sender.clone() else {
            return;
        };
        if code.is_empty() {
            return;
        }
        cx.background_executor()
            .spawn(async move {
                let _ = sender.send(code).await;
            })
            .detach();
        self.run_state = RunState::Running(DownloadStats::default());
        cx.notify();
    }

    fn render_login_mode_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .child(
                Button::new("login-mode-password")
                    .label("Username & Password")
                    .when(self.login_mode == LoginMode::UsernamePassword, |btn| {
                        btn.primary()
                    })
                    .when(self.login_mode != LoginMode::UsernamePassword, |btn| {
                        btn.secondary()
                    })
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.login_mode = LoginMode::UsernamePassword;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("login-mode-qr")
                    .label("QR Code")
                    .when(self.login_mode == LoginMode::Qr, |btn| btn.primary())
                    .when(self.login_mode != LoginMode::Qr, |btn| btn.secondary())
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.login_mode = LoginMode::Qr;
                        cx.notify();
                    })),
            )
    }

    /// Shows "Logged in as X" + a Logout button when a remembered login
    /// exists, instead of the login-mode toggle and username/password/QR
    /// fields - there's nothing left for the user to fill in.
    fn render_login_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(username) = self.config.logged_in_username.clone() else {
            return v_flex()
                .gap_3()
                .child(self.render_login_mode_toggle(cx))
                .child(self.render_login_fields(cx))
                .into_any_element();
        };
        h_flex()
            .gap_2()
            .items_center()
            .child(format!("Logged in as {username}"))
            .child(
                Button::new("logout")
                    .secondary()
                    .label("Logout")
                    .on_click(cx.listener(|view, _, _, cx| view.logout(cx))),
            )
            .into_any_element()
    }

    fn render_login_fields(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.login_mode {
            LoginMode::UsernamePassword => v_flex()
                .gap_3()
                .child(labeled_field("Username", Input::new(&self.username_input)))
                .child(labeled_field("Password", Input::new(&self.password_input)))
                .into_any_element(),
            LoginMode::Qr => div()
                .text_color(cx.theme().colors.muted_foreground)
                .child(
                    "A QR code to scan with the Steam Mobile app will appear below once you \
                     start the download.",
                )
                .into_any_element(),
        }
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_busy = matches!(
            self.run_state,
            RunState::PreparingDepotDownloader
                | RunState::LookingUpApp
                | RunState::Running(_)
                | RunState::Paused(_)
        );
        let download_button_label = match self.run_state {
            RunState::PreparingDepotDownloader | RunState::LookingUpApp | RunState::Running(_) => {
                "Working…"
            }
            RunState::Paused(_) => "Paused",
            _ => "Download",
        };
        v_flex()
            .gap_3()
            .child(labeled_field(
                "Steam app ID",
                Input::new(&self.app_id_input),
            ))
            .child(labeled_field(
                "Library folder (game installs in its own subfolder)",
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.download_dir_input)))
                    .child(
                        Button::new("browse-download-dir")
                            .label("Browse…")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.browse_for_download_dir(window, cx);
                            })),
                    ),
            ))
            .child(labeled_field(
                "Max concurrent downloads (optional)",
                Input::new(&self.max_downloads_input),
            ))
            .child(self.render_login_section(cx))
            .child(
                Button::new("start-download")
                    .primary()
                    .label(download_button_label)
                    .disabled(is_busy)
                    .on_click(cx.listener(|view, _, _, cx| view.start_download(cx))),
            )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.run_state {
            RunState::PreparingDepotDownloader => status_line("Setting up DepotDownloader…", cx),
            RunState::LookingUpApp => status_line("Looking up app…", cx),
            RunState::Idle => div().into_any_element(),
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
                .child(render_progress(stats))
                .child(self.render_running_controls(cx))
                .into_any_element(),
            RunState::Paused(stats) => v_flex()
                .gap_3()
                .child(render_progress(stats))
                .child(self.render_paused_controls(cx))
                .into_any_element(),
            RunState::Finished(stats) => render_progress(stats),
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

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("root")
            .size_full()
            .bg(cx.theme().colors.background)
            .child(TitleBar::new().child("DepotDownloader"))
            .child(
                v_flex()
                    .id("content")
                    .flex_1()
                    .p_6()
                    .gap_5()
                    .child(self.render_form(cx))
                    .child(self.render_status(cx)),
            )
            // Without this layer `open_alert_dialog` (the Cancel confirmation)
            // registers its dialog but nothing ever draws it.
            .children(Root::render_dialog_layer(window, cx))
    }
}

fn labeled_field(label: &'static str, field: impl IntoElement) -> impl IntoElement {
    v_flex().gap_1().child(label).child(field)
}

fn status_line(message: &str, cx: &mut Context<RootView>) -> AnyElement {
    div()
        .text_color(cx.theme().colors.muted_foreground)
        .child(message.to_string())
        .into_any_element()
}

/// Draws each QR module as an explicit black/white square, which is correct
/// regardless of font and reads reliably by a phone camera.
fn render_qr_code(url: &str) -> AnyElement {
    const MODULE_SIZE_PX: f32 = 6.0;
    const PADDING_PX: f32 = 12.0;

    let Ok(code) = QrCode::new(url) else {
        return div()
            .child("Could not draw the QR code.")
            .into_any_element();
    };
    let columns = code.width();
    let modules = code.to_colors();

    // Explicit width and height (rather than letting the container size to
    // its content) because this sits inside a `v_flex`, which stretches
    // children to fill its cross axis - without them the white background
    // would stretch to the full width of the status area instead of hugging
    // the square QR grid.
    let side = columns as f32 * MODULE_SIZE_PX + PADDING_PX * 2.0;

    div()
        .w(px(side))
        .h(px(side))
        .bg(rgb(0xFFFFFF))
        .p(px(PADDING_PX))
        .child(v_flex().children(modules.chunks(columns).map(|row| {
            h_flex().children(row.iter().map(|module| {
                div()
                    .size(px(MODULE_SIZE_PX))
                    .bg(if *module == Color::Dark {
                        rgb(0x000000)
                    } else {
                        rgb(0xFFFFFF)
                    })
            }))
        })))
        .into_any_element()
}

fn render_progress(stats: &DownloadStats) -> AnyElement {
    v_flex()
        .gap_2()
        .child(stats.status_message.clone())
        .child(format!("Progress: {:.1}%", stats.percent_complete()))
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
        .child(format!(
            "Download speed: {}",
            format_speed(stats.download_speed_bytes_per_sec)
        ))
        .child(format!(
            "Disk speed: {}",
            format_speed(stats.disk_speed_bytes_per_sec)
        ))
        .child(format!("ETA: {}", format_eta(stats.eta)))
        .into_any_element()
}

fn describe_unexpected_exit(status: std::io::Result<std::process::ExitStatus>) -> String {
    match status {
        Ok(status) if !status.success() => format!("DepotDownloader exited with {status}"),
        Err(error) => format!("DepotDownloader could not be waited on: {error}"),
        Ok(_) => "DepotDownloader exited before finishing the download.".to_string(),
    }
}
