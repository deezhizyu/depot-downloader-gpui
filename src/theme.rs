use std::rc::Rc;

use gpui_kit::App;
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};

const ZED_ONE_DARK_THEME_JSON: &str = include_str!("../assets/theme/zed_one_dark.json");

/// Applies the Zed One Dark palette and typography on top of GPUI Component's
/// built-in dark theme, so every widget matches Zed's look without per-widget
/// style overrides scattered through the view code.
///
/// Must run after `gpui_kit::init`, which is what first creates the global
/// `Theme`. Switching modes through `Theme::change` (rather than calling
/// `Theme::apply_config` directly) matters: `change` is what also resolves
/// fonts and rebuilds the separate `gpui-base` "Base" theme projection that
/// backgrounds/borders read from, so skipping it leaves those foundational
/// colors on the stock light theme even though `ThemeColor`-driven widgets
/// (like buttons) pick up the override correctly.
pub fn install(cx: &mut App) {
    let config: ThemeConfig = serde_json::from_str(ZED_ONE_DARK_THEME_JSON)
        .expect("assets/theme/zed_one_dark.json must be a valid theme config");
    Theme::global_mut(cx).dark_theme = Rc::new(config);
    Theme::change(ThemeMode::Dark, None, cx);
}
