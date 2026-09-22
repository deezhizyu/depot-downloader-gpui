use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::searchable_list::SearchableListItem;
use gpui_kit::component::{ActiveTheme, h_flex};
use gpui_kit::*;

use crate::depot_downloader::{BranchInfo, SteamApp};

/// One row of the game dropdown: an owned or free-to-download app, plus its
/// capsule logo once `fetch_logos` has resolved it (`None` while unresolved
/// or on a failed fetch - rendered as a blank placeholder rather than
/// blocking the row on a retry).
#[derive(Clone)]
pub(super) struct GameItem {
    pub app: SteamApp,
    pub logo: Option<Arc<Image>>,
}

impl SearchableListItem for GameItem {
    type Value = u64;

    fn title(&self) -> SharedString {
        self.app.name.clone().into()
    }

    fn value(&self) -> &u64 {
        &self.app.app_id
    }

    /// Search matches the name or the raw app id, per the ask.
    fn matches(&self, query: &str) -> bool {
        let query = query.trim();
        self.app.name.to_lowercase().contains(&query.to_lowercase())
            || (!query.is_empty() && self.app.app_id.to_string().contains(query))
    }

    fn render(&self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .gap_2()
            .items_center()
            .child(render_logo(self.logo.as_ref(), cx))
            .child(self.app.name.clone())
    }
}

const LOGO_WIDTH_PX: f32 = 46.0;
const LOGO_HEIGHT_PX: f32 = 17.0;

fn render_logo(logo: Option<&Arc<Image>>, cx: &App) -> AnyElement {
    let frame = div()
        .flex_none()
        .w(px(LOGO_WIDTH_PX))
        .h(px(LOGO_HEIGHT_PX))
        .rounded(cx.theme().radius);
    match logo {
        Some(logo) => frame
            .overflow_hidden()
            .child(img(logo.clone()).size_full().object_fit(ObjectFit::Cover))
            .into_any_element(),
        None => frame.bg(cx.theme().colors.muted).into_any_element(),
    }
}

/// One row of the DLC toggle list - just a name and id, no logo, per the
/// ask.
#[derive(Clone)]
pub(super) struct DlcItem {
    pub app_id: u64,
    pub name: String,
}

impl SearchableListItem for DlcItem {
    type Value = u64;

    fn title(&self) -> SharedString {
        self.name.clone().into()
    }

    fn value(&self) -> &u64 {
        &self.app_id
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.trim();
        self.name.to_lowercase().contains(&query.to_lowercase())
            || (!query.is_empty() && self.app_id.to_string().contains(query))
    }
}

/// One row of the branch dropdown. Password-protected branches are shown
/// disabled - the UI has no `-branchpassword` field to satisfy them.
#[derive(Clone)]
pub(super) struct BranchItem {
    pub name: String,
    pub password_required: bool,
}

impl SearchableListItem for BranchItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.name.clone().into()
    }

    fn value(&self) -> &String {
        &self.name
    }

    fn disabled(&self) -> bool {
        self.password_required
    }
}

impl From<BranchInfo> for BranchItem {
    fn from(branch: BranchInfo) -> Self {
        Self {
            name: branch.name,
            password_required: branch.password_required,
        }
    }
}

/// The index of the entry named `"public"` (DepotDownloader's own default
/// branch, per its `-branch` flag docs), case-insensitively, or `0` when
/// there's no such entry.
pub(super) fn default_branch_index(branches: &[BranchItem]) -> usize {
    branches
        .iter()
        .position(|branch| branch.name.eq_ignore_ascii_case("public"))
        .unwrap_or(0)
}

/// Fetches every app id's capsule logo and streams each one to `results` as
/// soon as it resolves, rather than waiting for the whole list - so the
/// combobox can show logos incrementally instead of freezing on a blank list
/// until every one of a large (free-apps) catalog has downloaded. App ids are
/// spread round-robin across `WORKER_COUNT` threads (item `i` goes to worker
/// `i % WORKER_COUNT`), so the *front* of `app_ids` - what the combobox shows
/// first, before scrolling - is serviced by every worker at once instead of
/// being stuck behind one worker's whole contiguous slice. Blocking; call
/// from a background thread.
pub(super) fn fetch_logos_streaming(app_ids: &[u64], results: async_channel::Sender<(u64, Arc<Image>)>) {
    const WORKER_COUNT: usize = 16;

    if app_ids.is_empty() {
        return;
    }

    std::thread::scope(|scope| {
        for worker in 0..WORKER_COUNT {
            let results = results.clone();
            let app_ids: Vec<u64> = app_ids.iter().copied().skip(worker).step_by(WORKER_COUNT).collect();
            scope.spawn(move || {
                for app_id in app_ids {
                    let Ok(bytes) = crate::steam::fetch_logo_bytes(app_id) else {
                        continue;
                    };
                    let image = Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes));
                    if results.send_blocking((app_id, image)).is_err() {
                        return;
                    }
                }
            });
        }
    });
}

/// Resolves every DLC app id's store name, `WORKER_COUNT` at a time - the
/// store API's `dlc` filter only gives ids, so each one needs its own
/// `appdetails` lookup (see `steam::fetch_app_name`). Blocking; call from a
/// background thread.
pub(super) fn resolve_dlc_names(app_ids: &[u64]) -> HashMap<u64, String> {
    const WORKER_COUNT: usize = 12;

    if app_ids.is_empty() {
        return HashMap::new();
    }
    let chunk_size = app_ids.len().div_ceil(WORKER_COUNT).max(1);

    std::thread::scope(|scope| {
        app_ids
            .chunks(chunk_size)
            .map(|chunk| scope.spawn(move || resolve_dlc_name_chunk(chunk)))
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect()
    })
}

fn resolve_dlc_name_chunk(app_ids: &[u64]) -> Vec<(u64, String)> {
    app_ids
        .iter()
        .filter_map(|&app_id| Some((app_id, crate::steam::fetch_app_name(app_id)?)))
        .collect()
}
