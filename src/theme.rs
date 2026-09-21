use std::rc::Rc;

use gpui_kit::App;
use gpui_kit::component::{Theme, ThemeConfig};

const ZED_ONE_DARK_THEME_JSON: &str = include_str!("../assets/theme/zed_one_dark.json");

/// Applies the Zed One Dark palette and typography on top of GPUI Component's
/// built-in dark theme, so every widget matches Zed's look without per-widget
/// style overrides scattered through the view code.
pub fn install(cx: &mut App) {
    let config: ThemeConfig = serde_json::from_str(ZED_ONE_DARK_THEME_JSON)
        .expect("assets/theme/zed_one_dark.json must be a valid theme config");
    let config = Rc::new(config);
    Theme::global_mut(cx).apply_config(&config);
}
