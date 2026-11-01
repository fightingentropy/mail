//! Colors, type and spacing. One dark charcoal theme, large readable text.

use std::sync::Arc;

use egui::{
    Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Vec2,
    epaint::text::VariationCoords,
};

pub const BACKGROUND: Color32 = Color32::from_rgb(0x17, 0x19, 0x1b);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x12, 0x13, 0x15);
pub const HOVER: Color32 = Color32::from_rgb(0x20, 0x23, 0x26);
pub const SELECTED: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x33);
pub const DIVIDER: Color32 = Color32::from_rgb(0x25, 0x28, 0x2b);
pub const FIELD: Color32 = Color32::from_rgb(0x21, 0x24, 0x27);
pub const TEXT: Color32 = Color32::from_rgb(0xec, 0xee, 0xf0);
pub const SECONDARY: Color32 = Color32::from_rgb(0xa3, 0xaa, 0xb0);
pub const DIM: Color32 = Color32::from_rgb(0x70, 0x78, 0x7f);
/// Unread dots, counts and focus. A calm blue that reads well on charcoal.
pub const ACCENT: Color32 = Color32::from_rgb(0x8a, 0xb4, 0xf8);
pub const STAR: Color32 = Color32::from_rgb(0xf2, 0xc4, 0x6d);
pub const DANGER: Color32 = Color32::from_rgb(0xf2, 0x8b, 0x82);

const MEDIUM: &str = "inter-medium";
const SEMIBOLD: &str = "inter-semibold";

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MEDIUM.into()))
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut style = (*ctx.global_style()).clone();
    style.text_styles = [
        (TextStyle::Small, regular(13.0)),
        (TextStyle::Body, regular(16.0)),
        (TextStyle::Button, regular(15.0)),
        (TextStyle::Heading, semibold(26.0)),
        (
            TextStyle::Monospace,
            FontId::new(14.0, FontFamily::Monospace),
        ),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    style.interaction.selectable_labels = false;
    style.animation_time = 0.1;

    let visuals = &mut style.visuals;
    *visuals = egui::Visuals::dark();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = Color32::from_rgb(0x1e, 0x21, 0x24);
    visuals.window_stroke = Stroke::new(1.0, DIVIDER);
    visuals.extreme_bg_color = FIELD;
    visuals.faint_bg_color = HOVER;
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.text_cursor.stroke = Stroke::new(2.0, ACCENT);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, DIVIDER);
    visuals.widgets.inactive.weak_bg_fill = FIELD;
    visuals.widgets.hovered.weak_bg_fill = HOVER;
    visuals.widgets.active.weak_bg_fill = SELECTED;
    ctx.set_global_style(style);
}

/// Inter, embedded, at three weights of its variable axis.
fn install_fonts(ctx: &egui::Context) {
    let inter: &'static [u8] = include_bytes!("../../assets/fonts/InterVariable.ttf");
    let weighted = |weight: f32| {
        let mut data = FontData::from_static(inter);
        data.tweak.coords = VariationCoords::new([(b"wght", weight)]);
        Arc::new(data)
    };

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("inter".to_owned(), weighted(400.0));
    fonts.font_data.insert(MEDIUM.to_owned(), weighted(500.0));
    fonts.font_data.insert(SEMIBOLD.to_owned(), weighted(600.0));

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "inter".to_owned());
    // egui's default fonts stay behind Inter for symbols and emoji it lacks.
    let fallbacks: Vec<String> = proportional.iter().skip(1).cloned().collect();
    for name in [MEDIUM, SEMIBOLD] {
        let mut family = vec![name.to_owned()];
        family.extend(fallbacks.iter().cloned());
        fonts.families.insert(FontFamily::Name(name.into()), family);
    }
    ctx.set_fonts(fonts);
}
