use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::combobox::{ComboboxEvent, ComboboxState};
use gpui_kit::component::input::InputState;
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::{ActiveTheme, Root, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::config::Config;
use crate::depot_downloader::{DiskPhase, DownloadRequest, SteamApp};
use crate::steam;

mod catalog;
mod form;
mod process_events;
mod qr_code;
mod session;
mod speed_chart;
mod speed_history;
mod state;
mod status;

use catalog::{BranchItem, DlcItem, GameItem};
use speed_history::SpeedHistory;
use state::{LoginMode, PendingAction, PendingControl, PendingLibraryManifest, QueueContext, RunState};

pub struct RootView {
    config: Config,
    login_mode: LoginMode,
    username_input: Entity<InputState>,
    password_input: Entity<InputState>,
    download_dir_input: Entity<InputState>,
    max_downloads_input: Entity<InputState>,
    guard_code_input: Entity<InputState>,
    /// Whether to write a Steam `appmanifest_*.acf` after a finished
    /// download; only offered, and only acted on, when the chosen library
    /// folder is a real `steamapps/common`.
    add_to_steam_library: bool,
    depot_downloader_binary: Option<PathBuf>,
    run_state: RunState,
    /// What a currently-running login-prompt state (see `RunState::
    /// FetchingLibrary` and its doc comment) is for.
    pending_action: PendingAction,
    /// The account's own licensed apps, populated by "Log in"; shown first
    /// in the game dropdown.
    owned_apps: Vec<SteamApp>,
    /// The anonymous account's apps (mostly free-to-play/tools), fetched
    /// once at startup; apps already in `owned_apps` are excluded.
    free_apps: Vec<SteamApp>,
    game_combobox: Entity<ComboboxState<SearchableVec<GameItem>>>,
    /// Capsule logos already resolved for the game dropdown, keyed by app
    /// id and kept for the life of the session - `rebuild_game_combobox`
    /// only streams-fetches logos missing from here, so a game whose logo
    /// already loaded never flickers back to blank on a later rebuild (a
    /// fresh login, the anonymous fetch landing, etc).
    logo_cache: HashMap<u64, Arc<Image>>,
    /// The game the user picked, cached from `owned_apps`/`free_apps` at
    /// selection time so later code doesn't have to re-search either list.
    selected_app: Option<SteamApp>,
    dlc_combobox: Entity<ComboboxState<SearchableVec<DlcItem>>>,
    /// Whether the selected game has any DLC, so `form.rs` knows whether to
    /// show the DLC dropdown at all.
    dlc_available: bool,
    /// Whether `on_game_selected`'s background DLC lookup for the currently
    /// selected game is still running, so `form.rs` can show a loading
    /// indicator instead of silently showing nothing until it resolves.
    dlc_loading: bool,
    branch_combobox: Entity<ComboboxState<SearchableVec<BranchItem>>>,
    /// Whether `on_game_selected`'s background branch lookup for the
    /// currently selected game is still running - the branch dropdown
    /// already shows a `"public"` placeholder immediately, so without this
    /// there is no way to tell that from the real (possibly longer) list
    /// still loading.
    branches_loading: bool,
    /// Remaining app ids (the selected game's checked DLCs) to download
    /// after the currently-running leg finishes; see `session::launch`.
    download_queue: VecDeque<u64>,
    /// How many legs the current download started with, so the status area
    /// can show "(2 of 3)"; `0` outside of a download.
    download_queue_total: usize,
    /// The library folder/branch/etc. shared by every leg of the current
    /// download queue; `None` outside of a download.
    queue_context: Option<QueueContext>,
    speed_history: SpeedHistory,
    /// Set while the pointer is over the speed chart, to the instant hovering
    /// began: freezes the chart's scroll at that instant so the tooltip's
    /// values stay put under a stationary cursor, while samples keep
    /// accumulating in `speed_history` in the background.
    speed_chart_hover_anchor: Option<Instant>,
    /// The last `DownloadStats::disk_phase` seen, so `apply_process_event` can
    /// clear `speed_history` the moment it changes: validate and download
    /// speeds aren't comparable, so a graph spanning both is meaningless.
    last_disk_phase: Option<DiskPhase>,
    respond_sender: Option<async_channel::Sender<String>>,
    cancel_sender: Option<async_channel::Sender<()>>,
    /// Single-permit lock serializing every DepotDownloader run that logs in
    /// as the real account (download, `-list-user-apps`, `-list-branches`):
    /// Steam drops one of two simultaneous sessions under the same
    /// credentials, so a second such run must wait for the first to fully
    /// exit rather than starting in parallel. The anonymous account has no
    /// such limit and never touches this lock.
    credentialed_lock_tx: async_channel::Sender<()>,
    credentialed_lock_rx: async_channel::Receiver<()>,
    pending_control: Option<PendingControl>,
    /// The request behind the currently running/paused download, so
    /// `resume_download` can relaunch it without asking the user to fill the
    /// form in again.
    last_request: Option<DownloadRequest>,
    /// How many automatic reconnect attempts have been made since the last
    /// successful login; reset on a fresh `start_download` and on every
    /// `login_success`, so a later unrelated drop gets its own full budget.
    retry_count: u32,
    /// Set when `add_to_steam_library` was on at launch and the library
    /// folder qualified; consumed the moment the download finishes.
    library_manifest: Option<PendingLibraryManifest>,
    /// Whether the just-finished run wrote a Steam appmanifest, so the
    /// "Download complete" status can tell the user to restart Steam.
    steam_library_manifest_written: bool,
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

        let game_combobox = cx.new(|cx| {
            ComboboxState::new(SearchableVec::new(Vec::<GameItem>::new()), vec![], window, cx)
                .searchable(true)
        });
        let dlc_combobox = cx.new(|cx| {
            ComboboxState::new(SearchableVec::new(Vec::<DlcItem>::new()), vec![], window, cx)
                .searchable(true)
                .multiple(true)
        });
        let branch_combobox = cx.new(|cx| {
            ComboboxState::new(SearchableVec::new(Vec::<BranchItem>::new()), vec![], window, cx)
                .searchable(true)
        });

        cx.subscribe_in(
            &game_combobox,
            window,
            |view, _combobox, event: &ComboboxEvent<SearchableVec<GameItem>>, window, cx| {
                if let ComboboxEvent::Confirm(values) = event {
                    view.on_game_selected(values.first().copied(), window, cx);
                }
            },
        )
        .detach();

        let (credentialed_lock_tx, credentialed_lock_rx) = async_channel::bounded(1);
        credentialed_lock_tx
            .try_send(())
            .expect("freshly created channel has room for its one permit");

        let view = Self {
            config,
            login_mode: LoginMode::UsernamePassword,
            username_input,
            password_input,
            download_dir_input,
            max_downloads_input,
            guard_code_input,
            add_to_steam_library: true,
            depot_downloader_binary: None,
            run_state: RunState::PreparingDepotDownloader,
            pending_action: PendingAction::FetchLibrary,
            owned_apps: Vec::new(),
            free_apps: Vec::new(),
            game_combobox,
            logo_cache: HashMap::new(),
            selected_app: None,
            dlc_combobox,
            dlc_available: false,
            dlc_loading: false,
            branch_combobox,
            branches_loading: false,
            download_queue: VecDeque::new(),
            download_queue_total: 0,
            queue_context: None,
            speed_history: SpeedHistory::default(),
            speed_chart_hover_anchor: None,
            last_disk_phase: None,
            respond_sender: None,
            cancel_sender: None,
            credentialed_lock_tx,
            credentialed_lock_rx,
            pending_control: None,
            last_request: None,
            retry_count: 0,
            library_manifest: None,
            steam_library_manifest_written: false,
        };
        view.start_provisioning(window, cx);
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

    /// Whether a DepotDownloader run - a download or a `-list-user-apps`
    /// fetch, which share the same `run_state`/`respond_sender` - is
    /// presently using them, so Download and Log In can't both try to launch
    /// a second one on top of it.
    fn is_busy(&self) -> bool {
        matches!(
            self.run_state,
            RunState::PreparingDepotDownloader
                | RunState::LookingUpApp
                | RunState::FetchingLibrary
                | RunState::Running(_)
                | RunState::Paused(_)
                | RunState::ShowingQrCode { .. }
                | RunState::AwaitingSteamGuardCode { .. }
                | RunState::AwaitingSteamGuardConfirmation
                | RunState::Reconnecting { .. }
        )
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("root")
            .size_full()
            .bg(cx.theme().colors.background)
            // Runs before any child's own mouse-down handler, so a click on
            // an input still focuses it: this only clears whatever was
            // focused *before* the click.
            .capture_any_mouse_down(|_event, window, cx| window.blur(cx))
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
