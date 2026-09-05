use eframe::egui::{self, vec2, Color32, CornerRadius, FontId, Margin, Stroke, TextStyle};

pub(super) const CANVAS: Color32 = Color32::from_rgb(11, 12, 14);
pub(super) const CANVAS_EDGE: Color32 = Color32::from_rgb(5, 6, 7);
pub(super) const CHROME: Color32 = Color32::from_rgb(20, 22, 26);
pub(super) const CHROME_RAISED: Color32 = Color32::from_rgb(26, 29, 34);
pub(super) const CARD: Color32 = Color32::from_rgb(23, 26, 31);
pub(super) const CARD_RAISED: Color32 = Color32::from_rgb(30, 34, 40);
pub(super) const HAIRLINE: Color32 = Color32::from_rgb(40, 44, 52);
pub(super) const HAIRLINE_SOFT: Color32 = Color32::from_rgb(31, 34, 40);

pub(super) const TEXT: Color32 = Color32::from_rgb(228, 231, 236);
pub(super) const TEXT_MUTED: Color32 = Color32::from_rgb(142, 149, 160);
pub(super) const TEXT_DIM: Color32 = Color32::from_rgb(96, 103, 114);

pub(super) const AMBER: Color32 = Color32::from_rgb(226, 164, 62);
pub(super) const AMBER_BRIGHT: Color32 = Color32::from_rgb(244, 186, 96);
pub(super) const AMBER_GLOW: Color32 = Color32::from_rgb(58, 45, 26);

pub(super) const CYAN: Color32 = Color32::from_rgb(112, 192, 206);
pub(super) const GREEN: Color32 = Color32::from_rgb(112, 192, 116);
pub(super) const WARN: Color32 = Color32::from_rgb(206, 142, 60);
pub(super) fn install_visuals(ctx: &egui::Context) {
    install_platform_font(ctx);
    let mut style = (*ctx.global_style()).clone();

    style.text_styles = [
        (TextStyle::Heading, FontId::proportional(17.0)),
        (TextStyle::Body, FontId::proportional(13.5)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Small, FontId::proportional(11.0)),
        (TextStyle::Monospace, FontId::monospace(12.5)),
    ]
    .into();

    style.spacing.item_spacing = vec2(8.0, 7.0);
    style.spacing.button_padding = vec2(10.0, 5.0);
    style.spacing.window_margin = Margin::same(0);
    style.spacing.menu_margin = Margin::same(6);
    style.spacing.interact_size.y = 26.0;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = CHROME;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, HAIRLINE);
    v.window_corner_radius = CornerRadius::same(8);
    v.faint_bg_color = CARD_RAISED;
    v.extreme_bg_color = CANVAS_EDGE;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = CYAN;

    v.selection.bg_fill = AMBER_GLOW;
    v.selection.stroke = Stroke::new(1.0, AMBER);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = CHROME;
    w.noninteractive.weak_bg_fill = CHROME;
    w.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE_SOFT);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_MUTED);
    w.noninteractive.corner_radius = CornerRadius::same(6);

    w.inactive.bg_fill = CARD_RAISED;
    w.inactive.weak_bg_fill = CHROME_RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = CornerRadius::same(6);

    w.hovered.bg_fill = AMBER_GLOW;
    w.hovered.weak_bg_fill = AMBER_GLOW;
    w.hovered.bg_stroke = Stroke::new(1.0, AMBER);
    w.hovered.fg_stroke = Stroke::new(1.0, AMBER_BRIGHT);
    w.hovered.corner_radius = CornerRadius::same(6);

    w.active.bg_fill = AMBER;
    w.active.weak_bg_fill = AMBER;
    w.active.bg_stroke = Stroke::new(1.0, AMBER_BRIGHT);
    w.active.fg_stroke = Stroke::new(1.0, CANVAS_EDGE);
    w.active.corner_radius = CornerRadius::same(6);

    w.open.bg_fill = CARD_RAISED;
    w.open.bg_stroke = Stroke::new(1.0, HAIRLINE);
    w.open.fg_stroke = Stroke::new(1.0, TEXT);

    ctx.set_global_style(style);
}

#[cfg(target_os = "windows")]
fn install_platform_font(ctx: &egui::Context) {
    let windows_directory = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    let path = std::path::PathBuf::from(windows_directory)
        .join("Fonts")
        .join("segoeui.ttf");
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };

    let name = "Segoe UI".to_owned();
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        name.clone(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, name);
    ctx.set_fonts(fonts);
}

#[cfg(not(target_os = "windows"))]
fn install_platform_font(_ctx: &egui::Context) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_visuals_keep_the_viewer_palette_and_spacing() {
        let context = egui::Context::default();

        install_visuals(&context);

        let style = context.global_style();
        assert!(style.visuals.dark_mode);
        assert_eq!(style.visuals.panel_fill, CHROME);
        assert_eq!(style.visuals.selection.bg_fill, AMBER_GLOW);
        assert_eq!(style.spacing.item_spacing, vec2(8.0, 7.0));
        assert_eq!(style.spacing.interact_size.y, 26.0);
    }

    #[test]
    fn text_rasterizer_is_native_only_on_windows() {
        let expected = if cfg!(target_os = "windows") {
            "DirectWrite grayscale"
        } else {
            "skrifa/vello"
        };

        assert_eq!(egui::epaint::text::font_rasterizer_name(), expected);
    }
}
