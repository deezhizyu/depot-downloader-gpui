use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use qrcode::{Color, QrCode};

/// Draws each QR module as an explicit black/white square, which is correct
/// regardless of font and reads reliably by a phone camera.
pub(super) fn render_qr_code(url: &str) -> AnyElement {
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
