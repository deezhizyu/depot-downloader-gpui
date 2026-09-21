mod app;
mod config;
mod depot_downloader;
mod steam;
mod theme;
mod ui;

use gpui_kit::assets::Assets;
use gpui_kit::component::Root;
use gpui_kit::*;

fn main() {
    let application = gpui_kit::application().with_assets(Assets);
    application.run(|cx| {
        gpui_kit::init(cx);
        theme::install(cx);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(960.0), px(680.0)), cx)),
            ..Default::default()
        };

        cx.spawn(async move |cx| {
            cx.open_window(window_options, |window, cx| {
                let view = cx.new(|cx| app::RootView::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open window");
        })
        .detach();
    });
}
