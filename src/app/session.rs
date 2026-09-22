use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::component::IndexPath;
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::*;

use crate::depot_downloader::{
    self, BranchInfo, DownloadRequest, DownloadStats, LoginMethod, SteamApp, UserAppsEvent,
    process, provisioning,
};
use crate::steam;

use super::RootView;
use super::catalog::{self, BranchItem, DlcItem, GameItem};
use super::state::{LoginMode, PendingAction, PendingControl, PendingLibraryManifest, QueueContext, RunState};

/// How many times an unexpected exit (a dropped Steam connection is the
/// common cause) is retried before giving up and showing `RunState::Failed`.
pub(super) const MAX_RETRIES: u32 = 3;
/// Wait before a retry: DepotDownloader's own internal reconnect can still be
/// unwinding right after the exit, and Steam itself may need a moment.
const RETRY_DELAY: Duration = Duration::from_secs(3);
/// How long `rebuild_game_combobox` waits after a logo arrives before
/// rebuilding the dropdown, to coalesce a trickle of near-simultaneous
/// completions into one rebuild instead of many.
const LOGO_STREAM_COALESCE_WINDOW: Duration = Duration::from_millis(300);
/// Wait after a real-account DepotDownloader run exits before handing the
/// `credentialed_lock` permit to the next one: the local process being gone
/// confirms *we* are done with it, but Steam's own session tracking can lag
/// a moment behind that, and a new login for the same account arriving
/// inside that window stalls (rather than erroring) on its first request
/// after login. Same reasoning as `RETRY_DELAY`.
const STEAM_SESSION_SETTLE_DELAY: Duration = Duration::from_millis(1500);

impl RootView {
    pub(super) fn start_provisioning(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async { provisioning::ensure_binary() })
                .await;
            let binary = match outcome {
                Ok(path) => path,
                Err(error) => {
                    let _ = this.update_in(cx, |view, _, cx| {
                        view.run_state =
                            RunState::Failed(format!("Could not set up DepotDownloader: {error}"));
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update_in(cx, |view, window, cx| {
                view.depot_downloader_binary = Some(binary);
                view.run_state = RunState::Idle;
                cx.notify();
                view.fetch_free_apps(window, cx);
                if let Some(username) = view.config.logged_in_username.clone() {
                    view.launch_user_apps_fetch(
                        LoginMethod::RememberedUsername { username },
                        window,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// Fetches the anonymous account's apps (mostly free-to-play/tools) for
    /// `free_apps`. Never prompts, so unlike `launch_user_apps_fetch` this
    /// doesn't touch `run_state`/`respond_sender` at all - it runs entirely
    /// independently of whatever the interactive login/download flow is
    /// doing.
    fn fetch_free_apps(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(binary) = self.depot_downloader_binary.clone() else {
            return;
        };
        let (update_tx, update_rx) = async_channel::unbounded();
        let (_respond_tx, respond_rx) = async_channel::unbounded();
        cx.background_executor()
            .spawn(depot_downloader::run_list_user_apps(
                binary,
                LoginMethod::Anonymous,
                update_tx,
                respond_rx,
            ))
            .detach();

        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = update_rx.recv().await {
                let is_apps = matches!(event, UserAppsEvent::Apps(_));
                if let UserAppsEvent::Apps(apps) = event {
                    let _ = this.update_in(cx, |view, window, cx| {
                        view.free_apps = apps;
                        view.rebuild_game_combobox(window, cx);
                    });
                }
                if is_apps {
                    break;
                }
            }
        })
        .detach();
    }

    /// Every app the account's own licenses and the anonymous account grant,
    /// of every type (not just games): the account's own apps first, then
    /// the anonymous account's apps, excluding anything already in the
    /// first list. Used as the base for `combined_games` (the dropdown) and
    /// `owned_dlc_ids` (which DLC ids are actually downloadable).
    fn combined_apps(&self) -> Vec<SteamApp> {
        let owned_ids: std::collections::HashSet<u64> =
            self.owned_apps.iter().map(|app| app.app_id).collect();
        let mut apps = self.owned_apps.clone();
        apps.extend(
            self.free_apps
                .iter()
                .filter(|app| !owned_ids.contains(&app.app_id))
                .cloned(),
        );
        apps
    }

    /// `combined_apps`, narrowed to actual games - the game dropdown has no
    /// use for the DLC/tool/demo/soundtrack/dedicated-server entries a
    /// Steam license list is full of.
    fn combined_games(&self) -> Vec<SteamApp> {
        self.combined_apps()
            .into_iter()
            .filter(|app| app.is_type("game"))
            .collect()
    }

    /// App ids of every DLC the account's own licenses or the anonymous
    /// account actually grant - `steam::fetch_dlc_app_ids` returns a game's
    /// *entire* DLC catalog from the Steam store, unpurchased entries
    /// included, so `on_game_selected` intersects it with this set before
    /// showing the DLC dropdown.
    fn owned_dlc_ids(&self) -> std::collections::HashSet<u64> {
        self.combined_apps()
            .into_iter()
            .filter(|app| app.is_type("dlc"))
            .map(|app| app.app_id)
            .collect()
    }

    /// Rebuilds the game dropdown from `owned_apps`/`free_apps`, showing
    /// whatever logos `logo_cache` already has instantly, then streams in
    /// only the logos still missing (see `catalog::fetch_logos_streaming`)
    /// instead of waiting for the whole (potentially hundreds-strong,
    /// free-apps-included) catalog to finish downloading. Reusing the cache
    /// across calls - rather than starting a fresh fetch from empty every
    /// time, as `owned_apps` landing after `free_apps` does at startup -
    /// also means an already-shown logo never flickers back to blank
    /// because two overlapping fetches raced each other.
    pub(super) fn rebuild_game_combobox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_game_items(window, cx);

        let missing_app_ids: Vec<u64> = self
            .combined_games()
            .iter()
            .map(|app| app.app_id)
            .filter(|app_id| !self.logo_cache.contains_key(app_id))
            .collect();
        if missing_app_ids.is_empty() {
            return;
        }

        let (logo_tx, logo_rx) = async_channel::unbounded();
        cx.background_executor()
            .spawn(async move { catalog::fetch_logos_streaming(&missing_app_ids, logo_tx) })
            .detach();

        cx.spawn_in(window, async move |this, cx| {
            while let Ok((app_id, logo)) = logo_rx.recv().await {
                let mut batch = vec![(app_id, logo)];
                // A catalog this size (free apps alone can be in the
                // hundreds) resolves logos in a steady trickle, not one
                // burst - rebuilding the dropdown on every single arrival
                // would re-render it many times a second for as long as the
                // fetch runs. Coalesce a short window of arrivals into one
                // rebuild instead.
                smol::Timer::after(LOGO_STREAM_COALESCE_WINDOW).await;
                while let Ok(item) = logo_rx.try_recv() {
                    batch.push(item);
                }
                let updated = this.update_in(cx, |view, window, cx| {
                    view.logo_cache.extend(batch);
                    view.set_game_items(window, cx);
                });
                if updated.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn set_game_items(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items: Vec<GameItem> = self
            .combined_games()
            .into_iter()
            .map(|app| {
                let logo = self.logo_cache.get(&app.app_id).cloned();
                GameItem { app, logo }
            })
            .collect();
        let previously_selected = self.game_combobox.read(cx).selected_value();
        // `set_selected_values` below clears whatever the user is currently
        // typing (it resets the query as part of re-committing a selection)
        // - this function reruns on every streamed-in logo, so without
        // restoring it here, typing a search while a game is already
        // selected got wiped out from under the user mid-keystroke.
        let query = self.game_combobox.read(cx).query(cx);
        self.game_combobox.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(items), window, cx);
        });
        if let Some(value) = previously_selected {
            self.game_combobox.update(cx, |state, cx| {
                state.set_selected_values(&[value], window, cx);
            });
        }
        if !query.is_empty() {
            self.game_combobox.update(cx, |state, cx| {
                state.set_query(query, window, cx);
            });
        }
    }

    /// The login DepotDownloader should use right now: whatever the account
    /// has remembered, or the anonymous account otherwise. Used for
    /// branch-listing and for the download queue - not for `log_in`, which
    /// builds a fresh `LoginMethod` from whatever the user just typed/chose.
    pub(super) fn current_login_method(&self) -> LoginMethod {
        match &self.config.logged_in_username {
            Some(username) => LoginMethod::RememberedUsername {
                username: username.clone(),
            },
            None => LoginMethod::Anonymous,
        }
    }

    /// Runs `-list-user-apps` with `login`, reusing `RunState`'s login-prompt
    /// variants (see their doc comments) so QR/Steam Guard prompts surface
    /// exactly like they do during a download. On success, populates
    /// `owned_apps` and remembers the login (see `apply_user_apps_event`).
    pub(super) fn launch_user_apps_fetch(
        &mut self,
        login: LoginMethod,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(binary) = self.depot_downloader_binary.clone() else {
            return;
        };

        let (update_tx, update_rx) = async_channel::unbounded();
        let (respond_tx, respond_rx) = async_channel::unbounded();
        self.respond_sender = Some(respond_tx);
        self.cancel_sender = None;
        self.pending_control = None;
        self.pending_action = PendingAction::FetchLibrary;
        self.run_state = RunState::FetchingLibrary;
        cx.notify();

        let is_anonymous = matches!(login, LoginMethod::Anonymous);
        let lock_tx = self.credentialed_lock_tx.clone();
        let lock_rx = self.credentialed_lock_rx.clone();
        cx.background_executor()
            .spawn(run_credentialed(
                is_anonymous,
                lock_tx,
                lock_rx,
                depot_downloader::run_list_user_apps(binary, login, update_tx, respond_rx),
            ))
            .detach();

        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = update_rx.recv().await {
                let is_terminal = matches!(
                    event,
                    UserAppsEvent::Apps(_)
                        | UserAppsEvent::Error(_)
                        | UserAppsEvent::LoginExpired
                        | UserAppsEvent::Exited
                        | UserAppsEvent::FailedToStart(_)
                );
                let updated = this.update_in(cx, |view, window, cx| {
                    view.apply_user_apps_event(event, window, cx);
                });
                if updated.is_err() || is_terminal {
                    break;
                }
            }
        })
        .detach();
    }

    /// The "Log in" button: fetches the account's own games with whatever
    /// credentials/QR mode is currently selected.
    pub(super) fn log_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let login = match self.login_mode {
            LoginMode::UsernamePassword => LoginMethod::UsernamePassword {
                username: self.username_input.read(cx).value().trim().to_string(),
                password: self.password_input.read(cx).value().to_string(),
            },
            LoginMode::Qr => LoginMethod::Qr,
        };
        self.launch_user_apps_fetch(login, window, cx);
    }

    /// A game was picked (or the selection was cleared) in the game
    /// dropdown: looks up its DLC and branches in the background.
    pub(super) fn on_game_selected(
        &mut self,
        app_id: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_dlc_items(Vec::new(), window, cx);
        self.set_branch_items(Vec::new(), window, cx);
        self.dlc_loading = false;
        self.branches_loading = false;

        let Some(app_id) = app_id else {
            self.selected_app = None;
            cx.notify();
            return;
        };
        let Some(app) = self.combined_games().into_iter().find(|app| app.app_id == app_id) else {
            return;
        };
        self.selected_app = Some(app);
        // A cached `-list-branches` result (see `Config::branch_cache`) shows
        // instantly and skips the network round trip entirely - DepotDownloader
        // treats every list-branches call as its own fresh Steam login, so on
        // a repeat selection this is the difference between an instant
        // dropdown and several seconds queued behind whatever other
        // credentialed run currently holds the account lock.
        let cached_branches = self.config.branch_cache.get(&app_id).cloned();
        if let Some(branches) = cached_branches.clone() {
            self.set_branch_items(branches, window, cx);
        }
        cx.notify();

        let Some(binary) = self.depot_downloader_binary.clone() else {
            return;
        };
        let login = self.current_login_method();
        let is_anonymous = matches!(login, LoginMethod::Anonymous);
        let lock_tx = self.credentialed_lock_tx.clone();
        let lock_rx = self.credentialed_lock_rx.clone();
        let owned_dlc_ids = self.owned_dlc_ids();
        self.dlc_loading = true;
        self.branches_loading = cached_branches.is_none();
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let items = cx
                .background_executor()
                .spawn(async move {
                    // A game's DLC catalog from the Steam store includes
                    // every DLC that exists for it, purchased or not -
                    // narrow it to what this account can actually download.
                    let dlc_app_ids: Vec<u64> = steam::fetch_dlc_app_ids(app_id)
                        .into_iter()
                        .filter(|dlc_app_id| owned_dlc_ids.contains(dlc_app_id))
                        .collect();
                    let names = catalog::resolve_dlc_names(&dlc_app_ids);
                    dlc_app_ids
                        .into_iter()
                        .map(|app_id| {
                            let name = names
                                .get(&app_id)
                                .cloned()
                                .unwrap_or_else(|| format!("DLC {app_id}"));
                            DlcItem { app_id, name }
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.set_dlc_items(items, window, cx);
                view.dlc_loading = false;
                cx.notify();
            });
        })
        .detach();

        if cached_branches.is_none() {
            cx.spawn_in(window, async move |this, cx| {
                let branches = cx
                    .background_executor()
                    .spawn(run_credentialed(
                        is_anonymous,
                        lock_tx,
                        lock_rx,
                        depot_downloader::run_list_branches(binary, app_id, login),
                    ))
                    .await;
                let _ = this.update_in(cx, |view, window, cx| {
                    if let Some(branches) = &branches {
                        view.config.branch_cache.insert(app_id, branches.clone());
                        view.config.save();
                    }
                    view.set_branch_items(branches.unwrap_or_default(), window, cx);
                    view.branches_loading = false;
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn set_dlc_items(&mut self, items: Vec<DlcItem>, window: &mut Window, cx: &mut Context<Self>) {
        self.dlc_available = !items.is_empty();
        self.dlc_combobox.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(items), window, cx);
            state.clear_selection(cx);
        });
    }

    /// Replaces the branch dropdown, defaulting the selection to the entry
    /// named `"public"` (see `catalog::default_branch_index`). An empty
    /// `branches` (no game selected yet, or `-list-branches` failed) falls
    /// back to a single `"public"` entry rather than leaving the dropdown
    /// empty.
    fn set_branch_items(
        &mut self,
        branches: Vec<BranchInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut items: Vec<BranchItem> = branches.into_iter().map(BranchItem::from).collect();
        if items.is_empty() {
            items.push(BranchItem {
                name: "public".to_string(),
                password_required: false,
            });
        }
        let default_index = catalog::default_branch_index(&items);
        self.branch_combobox.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(items), window, cx);
            state.set_selected_indices([IndexPath::new(default_index)], window, cx);
        });
    }

    pub(super) fn start_download(&mut self, cx: &mut Context<Self>) {
        let Some(binary) = self.depot_downloader_binary.clone() else {
            return;
        };

        let Some(app) = self.selected_app.clone() else {
            self.run_state = RunState::Failed("Choose a game first.".to_string());
            cx.notify();
            return;
        };

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

        let branch = self.branch_combobox.read(cx).selected_value();
        let dlc_ids = self.dlc_combobox.read(cx).selected_values();

        self.config.last_app_id = Some(app.app_id);
        self.config.last_download_location = library_dir.clone();
        self.config.last_max_downloads = max_downloads_text;
        self.config.save();

        self.download_queue = dlc_ids.into_iter().collect();
        self.download_queue_total = 1 + self.download_queue.len();
        self.queue_context = Some(QueueContext {
            library_dir,
            max_downloads,
            branch,
            add_to_steam_library: self.add_to_steam_library,
        });

        self.speed_history.clear();
        self.speed_chart_hover_anchor = None;
        self.last_disk_phase = None;
        self.retry_count = 0;
        self.pending_action = PendingAction::Download;

        let login = self.current_login_method();
        self.launch_queue_leg(app.app_id.to_string(), login, binary, cx);
    }

    /// Downloads one queue leg (the base game, or one checked DLC): looks up
    /// the app's own install folder name so it lands in its own subfolder
    /// under the chosen library, then launches it. Shared by
    /// `start_download` (the first leg) and `process_events.rs`'s queue
    /// continuation (every later leg).
    pub(super) fn launch_queue_leg(
        &mut self,
        app_id: String,
        login: LoginMethod,
        binary: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let Some(context) = &self.queue_context else {
            return;
        };
        let library_dir = context.library_dir.clone();
        let max_downloads = context.max_downloads;
        let branch = context.branch.clone();
        let add_to_steam_library = context.add_to_steam_library;

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
                    .then(|| {
                        common_dir.parent().map(|steamapps_dir| PendingLibraryManifest {
                            steamapps_dir: steamapps_dir.to_path_buf(),
                            app_id: app_id.clone(),
                            app_info,
                        })
                    })
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
                branch,
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
    /// whatever `current_login_method` presently is (remembered login, or
    /// anonymous), rather than repeating the original QR scan or password
    /// entry (see `RunState::Paused`'s doc comment for why that's a correct
    /// continuation, not just a restart).
    pub(super) fn resume_download(&mut self, cx: &mut Context<Self>) {
        let Some(mut request) = self.last_request.clone() else {
            return;
        };
        request.login = self.current_login_method();
        self.launch(request, cx);
    }

    /// Wires up the channels for one DepotDownloader run and spawns both the
    /// process supervisor and the task that forwards its events into
    /// `apply_process_event`. Shared by `launch_queue_leg` and
    /// `resume_download` so channel setup isn't duplicated between them.
    fn launch(&mut self, request: DownloadRequest, cx: &mut Context<Self>) {
        self.last_request = Some(request.clone());

        let (update_tx, update_rx) = async_channel::unbounded();
        let (respond_tx, respond_rx) = async_channel::unbounded();
        let (cancel_tx, cancel_rx) = async_channel::unbounded();
        self.respond_sender = Some(respond_tx);
        self.cancel_sender = Some(cancel_tx);
        self.pending_control = None;
        self.run_state = RunState::Running(DownloadStats::default());

        let is_anonymous = matches!(request.login, LoginMethod::Anonymous);
        let lock_tx = self.credentialed_lock_tx.clone();
        let lock_rx = self.credentialed_lock_rx.clone();
        cx.background_executor()
            .spawn(run_credentialed(
                is_anonymous,
                lock_tx,
                lock_rx,
                process::run(request, update_tx, respond_rx, cancel_rx),
            ))
            .detach();

        cx.spawn(async move |this, cx| {
            while let Ok(event) = update_rx.recv().await {
                let updated = this.update(cx, |view, cx| {
                    view.apply_process_event(event, cx);
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

    /// Handles DepotDownloader exiting on its own without finishing (a
    /// dropped Steam connection is the common cause): retries by relaunching
    /// with the same kind of login up to `MAX_RETRIES` times before settling
    /// on `RunState::Failed`.
    pub(super) fn handle_unexpected_failure(&mut self, reason: String, cx: &mut Context<Self>) {
        let can_retry = self.retry_count < MAX_RETRIES && self.last_request.is_some();

        if !can_retry {
            let attempts = self.retry_count;
            self.run_state = RunState::Failed(if attempts > 0 {
                format!("{reason} (gave up after {attempts} retries.)")
            } else {
                reason
            });
            cx.notify();
            return;
        }

        self.retry_count += 1;
        self.run_state = RunState::Reconnecting {
            attempt: self.retry_count,
        };
        cx.notify();
        eprintln!(
            "[depot-downloader-gpui] {reason} - retrying ({} of {MAX_RETRIES})",
            self.retry_count
        );

        cx.spawn(async move |this, cx| {
            smol::Timer::after(RETRY_DELAY).await;
            let _ = this.update(cx, |view, cx| view.resume_download(cx));
        })
        .detach();
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
    /// later reconstructs a `RememberedUsername` request. Also drops the
    /// owned-games list, since it came from that login.
    pub(super) fn logout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.config.logged_in_username = None;
        self.config.save();
        if let Some(install_dir) = self
            .depot_downloader_binary
            .as_deref()
            .and_then(Path::parent)
        {
            let _ = std::fs::remove_file(install_dir.join("account.config"));
        }
        self.owned_apps.clear();
        self.rebuild_game_combobox(window, cx);
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
        self.run_state = match self.pending_action {
            PendingAction::Download => RunState::Running(DownloadStats::default()),
            PendingAction::FetchLibrary => RunState::FetchingLibrary,
        };
        cx.notify();
    }
}

/// Runs `fut` (a DepotDownloader launch), serializing it against any other
/// concurrent run under the same real account via `lock_tx`/`lock_rx` (see
/// `RootView::credentialed_lock_tx`'s doc comment). `login_is_anonymous`
/// skips the lock entirely: the anonymous account has no one-session limit
/// to race against.
async fn run_credentialed<T>(
    login_is_anonymous: bool,
    lock_tx: async_channel::Sender<()>,
    lock_rx: async_channel::Receiver<()>,
    fut: impl std::future::Future<Output = T>,
) -> T {
    if !login_is_anonymous {
        let _ = lock_rx.recv().await;
    }
    let result = fut.await;
    if !login_is_anonymous {
        // Hand the permit back after `STEAM_SESSION_SETTLE_DELAY`, on its own
        // thread rather than inline, so the caller gets `result` the moment
        // it's ready instead of waiting out the settle delay too.
        std::thread::spawn(move || {
            std::thread::sleep(STEAM_SESSION_SETTLE_DELAY);
            let _ = lock_tx.send_blocking(());
        });
    }
    result
}
