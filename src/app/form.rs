use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::combobox::Combobox;
use gpui_kit::component::input::Input;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::steam;

use super::RootView;
use super::state::LoginMode;

impl RootView {
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

    fn render_login_mode_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .child(self.login_mode_button(
                "login-mode-password",
                "Username & Password",
                LoginMode::UsernamePassword,
                cx,
            ))
            .child(self.login_mode_button("login-mode-qr", "QR Code", LoginMode::Qr, cx))
    }

    fn login_mode_button(
        &self,
        id: &'static str,
        label: &'static str,
        mode: LoginMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let button = Button::new(id)
            .label(label)
            .disabled(self.is_busy())
            .on_click(cx.listener(move |view, _, _, cx| {
                view.login_mode = mode;
                cx.notify();
            }));
        if self.login_mode == mode {
            button.primary()
        } else {
            button.secondary()
        }
    }

    /// Shows "Logged in as X" + a Logout button when a remembered login
    /// exists, instead of the login-mode toggle, username/password/QR
    /// fields and Log In button - there's nothing left for the user to do.
    pub(super) fn render_login_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(username) = self.config.logged_in_username.clone() else {
            return v_flex()
                .gap_3()
                .child(self.render_login_mode_toggle(cx))
                .child(self.render_login_fields(cx))
                .child(
                    Button::new("log-in")
                        .primary()
                        .label("Log In")
                        .disabled(self.is_busy())
                        .on_click(cx.listener(|view, _, window, cx| view.log_in(window, cx))),
                )
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
                    .disabled(self.is_busy())
                    .on_click(cx.listener(|view, _, window, cx| view.logout(window, cx))),
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
                     click Log In.",
                )
                .into_any_element(),
        }
    }

    pub(super) fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_busy = self.is_busy();
        let download_button_label = if matches!(self.run_state, super::RunState::Paused(_)) {
            "Paused"
        } else if is_busy {
            "Working…"
        } else {
            "Download"
        };

        let mut column = v_flex()
            .gap_3()
            .child(labeled_field(
                "Game",
                Combobox::new(&self.game_combobox)
                    .search_placeholder("Search by name or app id")
                    .placeholder("Choose a game")
                    .cleanable(true)
                    .disabled(is_busy)
                    .w_full(),
            ));

        if self.selected_app.is_some() {
            if self.dlc_loading {
                column = column.child(labeled_field("DLC", loading_row("Checking for DLC…", cx)));
            } else if self.dlc_available {
                column = column.child(labeled_field(
                    "DLC",
                    Combobox::new(&self.dlc_combobox)
                        .placeholder("No DLC selected")
                        .cleanable(true)
                        .disabled(is_busy)
                        .w_full(),
                ));
            }
            column = column.child(labeled_field(
                "Branch",
                v_flex()
                    .gap_1()
                    .child(Combobox::new(&self.branch_combobox).disabled(is_busy).w_full())
                    .when(self.branches_loading, |branch| {
                        branch.child(loading_row("Loading branches…", cx))
                    }),
            ));
        }

        column
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
            .when(
                steam::is_steam_library_common_dir(std::path::Path::new(
                    self.download_dir_input.read(cx).value().trim(),
                )),
                |column| {
                    column.child(
                        Checkbox::new("add-to-steam-library")
                            .label("Add to Steam library when finished")
                            .checked(self.add_to_steam_library)
                            .on_click(cx.listener(|view, checked, _, cx| {
                                view.add_to_steam_library = *checked;
                                cx.notify();
                            })),
                    )
                },
            )
            .child(labeled_field(
                "Max concurrent downloads (optional)",
                Input::new(&self.max_downloads_input),
            ))
            .child(
                Button::new("start-download")
                    .primary()
                    .label(download_button_label)
                    .disabled(is_busy || self.selected_app.is_none())
                    .on_click(cx.listener(|view, _, _, cx| view.start_download(cx))),
            )
    }
}

fn labeled_field(label: &'static str, field: impl IntoElement) -> impl IntoElement {
    v_flex().gap_1().child(label).child(field)
}

/// A small spinner + message, for a background fetch (DLC/branch lookup)
/// that's still running - without this the field it belongs to either shows
/// nothing yet or, for branches, a `"public"` placeholder indistinguishable
/// from the real (possibly longer) result still on the way.
fn loading_row(message: &'static str, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_2()
        .items_center()
        .text_color(cx.theme().colors.muted_foreground)
        .child(Spinner::new())
        .child(message)
}
