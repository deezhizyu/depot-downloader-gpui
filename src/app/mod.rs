use std::path::PathBuf;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Root, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::config::Config;
use crate::depot_downloader::DownloadRequest;
use crate::steam;

mod form;
mod process_events;
mod qr_code;
mod session;
mod state;
mod status;

use state::{LoginMode, PendingControl, RunState};

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
                .placeholder("Steam app id or store URL")
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

        cx.subscribe_in(
            &app_id_input,
            window,
            |_view, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let current = input.read(cx).value().to_string();
                let parsed = steam::parse_app_id(&current);
                if parsed != current {
                    input.update(cx, |state, cx| state.set_value(parsed, window, cx));
                }
            },
        )
        .detach();

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

    fn is_awaiting_download_start(&self) -> bool {
        matches!(
            self.run_state,
            RunState::PreparingDepotDownloader
                | RunState::Idle
                | RunState::Finished(_)
                | RunState::Failed(_)
        )
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
                h_flex()
                    .id("content")
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(
                        v_flex()
                            .h_full()
                            .w(px(400.0))
                            .flex_none()
                            .p_6()
                            .border_r_1()
                            .border_color(cx.theme().colors.border)
                            .child(self.render_form(cx)),
                    )
                    .child(
                        v_flex()
                            .id("status")
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .p_6()
                            .gap_5()
                            .overflow_y_scroll()
                            .when(self.is_awaiting_download_start(), |column| {
                                column.child(self.render_login_section(cx))
                            })
                            .child(self.render_status(cx)),
                    ),
            )
            // Without this layer `open_alert_dialog` (the Cancel confirmation)
            // registers its dialog but nothing ever draws it.
            .children(Root::render_dialog_layer(window, cx))
    }
}
