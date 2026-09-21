use std::path::PathBuf;

use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::config::Config;
use crate::depot_downloader::process;
use crate::depot_downloader::{
    AuthPrompt, DownloadRequest, DownloadStats, LoginMethod, ProcessEvent, provisioning,
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
    Idle,
    Running(DownloadStats),
    ShowingQrCode { ascii_art: String },
    AwaitingSteamGuardCode,
    AwaitingSteamGuardEmailCode { email: String },
    AwaitingSteamGuardConfirmation,
    Finished(DownloadStats),
    Failed(String),
}

pub struct RootView {
    config: Config,
    login_mode: LoginMode,
    username_input: Entity<InputState>,
    password_input: Entity<InputState>,
    app_id_input: Entity<InputState>,
    download_dir_input: Entity<InputState>,
    guard_code_input: Entity<InputState>,
    depot_downloader_binary: Option<PathBuf>,
    run_state: RunState,
    respond_sender: Option<async_channel::Sender<String>>,
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

        let default_download_dir = config.last_download_location.clone().or_else(|| {
            (!config.last_app_id.is_empty())
                .then(|| steam::default_download_dir_for_app(&config.last_app_id))
                .flatten()
        });
        let download_dir_input = cx.new(|cx| {
            let state = InputState::new(window, cx).placeholder("Download location");
            match &default_download_dir {
                Some(dir) => state.default_value(dir.to_string_lossy().into_owned()),
                None => state,
            }
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
            guard_code_input,
            depot_downloader_binary: None,
            run_state: RunState::PreparingDepotDownloader,
            respond_sender: None,
        };
        view.start_provisioning(cx);
        view
    }

    fn start_provisioning(&self, cx: &mut Context<Self>) {
        let mut config = self.config.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    provisioning::ensure_binary(&mut config).map(|path| (path, config))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match outcome {
                    Ok((path, config)) => {
                        view.config = config;
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

        let download_dir = {
            let raw = self.download_dir_input.read(cx).value();
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
        };

        let login = match self.login_mode {
            LoginMode::UsernamePassword => LoginMethod::UsernamePassword {
                username: self.username_input.read(cx).value().trim().to_string(),
                password: self.password_input.read(cx).value().to_string(),
            },
            LoginMode::Qr => LoginMethod::Qr,
        };

        self.config.last_app_id = app_id.clone();
        self.config.last_download_location = download_dir.clone();
        self.config.save();

        let request = DownloadRequest {
            depot_downloader_binary: binary,
            app_id,
            download_dir,
            login,
        };

        let (update_tx, update_rx) = async_channel::unbounded();
        let (respond_tx, respond_rx) = async_channel::unbounded();
        self.respond_sender = Some(respond_tx);
        self.run_state = RunState::Running(DownloadStats::default());

        cx.background_executor()
            .spawn(process::run(request, update_tx, respond_rx))
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

    fn apply_process_event(&mut self, event: ProcessEvent) {
        match event {
            ProcessEvent::FailedToStart(message) => {
                self.run_state = RunState::Failed(message);
            }
            ProcessEvent::Exited(status) => {
                if let RunState::Running(stats) = &self.run_state {
                    self.run_state = if stats.is_finished {
                        RunState::Finished(stats.clone())
                    } else {
                        RunState::Failed(describe_unexpected_exit(status))
                    };
                }
            }
            ProcessEvent::Stats(stats) => {
                self.run_state = if let Some(ascii_art) = stats.qr_code_ascii_art.clone() {
                    RunState::ShowingQrCode { ascii_art }
                } else if let Some(prompt) = stats.auth_prompt.clone() {
                    match prompt {
                        AuthPrompt::Code => RunState::AwaitingSteamGuardCode,
                        AuthPrompt::EmailCode { email } => {
                            RunState::AwaitingSteamGuardEmailCode { email }
                        }
                        AuthPrompt::Confirmation => RunState::AwaitingSteamGuardConfirmation,
                    }
                } else if let Some(message) = stats.error_message.clone() {
                    RunState::Failed(message)
                } else if stats.is_finished {
                    RunState::Finished(stats)
                } else {
                    RunState::Running(stats)
                };
            }
        }
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
            RunState::PreparingDepotDownloader | RunState::Running(_)
        );
        v_flex()
            .gap_3()
            .child(labeled_field(
                "Steam app ID",
                Input::new(&self.app_id_input),
            ))
            .child(labeled_field(
                "Download location",
                Input::new(&self.download_dir_input),
            ))
            .child(self.render_login_mode_toggle(cx))
            .child(self.render_login_fields(cx))
            .child(
                Button::new("start-download")
                    .primary()
                    .label(if is_busy { "Working…" } else { "Download" })
                    .disabled(is_busy)
                    .on_click(cx.listener(|view, _, _, cx| view.start_download(cx))),
            )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.run_state {
            RunState::PreparingDepotDownloader => status_line("Setting up DepotDownloader…", cx),
            RunState::Idle => div().into_any_element(),
            RunState::ShowingQrCode { ascii_art } => v_flex()
                .gap_2()
                .child("Scan this with the Steam Mobile app:")
                .child(
                    v_flex()
                        .font_family(cx.theme().mono_font_family.clone())
                        .children(ascii_art.lines().map(str::to_string)),
                )
                .into_any_element(),
            RunState::AwaitingSteamGuardCode => {
                self.render_guard_code_prompt("Enter your Steam Guard code", cx)
            }
            RunState::AwaitingSteamGuardEmailCode { email } => {
                self.render_guard_code_prompt(&format!("Enter the code emailed to {email}"), cx)
            }
            RunState::AwaitingSteamGuardConfirmation => {
                status_line("Confirm this sign-in in the Steam Mobile app…", cx)
            }
            RunState::Running(stats) => render_progress(stats),
            RunState::Finished(stats) => render_progress(stats),
            RunState::Failed(message) => div()
                .text_color(cx.theme().colors.danger)
                .child(message.clone())
                .into_any_element(),
        }
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("root")
            .size_full()
            .bg(cx.theme().colors.background)
            .child(
                v_flex()
                    .id("content")
                    .flex_1()
                    .p_6()
                    .gap_5()
                    .child(self.render_form(cx))
                    .child(self.render_status(cx)),
            )
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

fn render_progress(stats: &DownloadStats) -> AnyElement {
    v_flex()
        .gap_2()
        .child(stats.status_message.clone())
        .child(format!(
            "Depot progress: {:.1}%",
            stats.current_depot_percent
        ))
        .child(format!(
            "Downloaded: {}",
            format_bytes(stats.downloaded_bytes)
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
