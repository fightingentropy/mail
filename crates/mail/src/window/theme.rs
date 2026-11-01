//! Colors, type and spacing. A charcoal dark palette and a light one, chosen
//! by the macOS appearance; large readable text, no gradients, one accent.

use std::sync::Arc;

use egui::{
    Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Theme, Vec2,
    epaint::text::VariationCoords,
};

/// Every color the window draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: Color32,
    pub sidebar: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub divider: Color32,
    pub field: Color32,
    /// Overlays and tooltips, a step apart from the background.
    pub overlay: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub dim: Color32,
    /// Unread dots, counts, focus and links.
    pub accent: Color32,
    pub star: Color32,
    pub danger: Color32,
}

pub const DARK: Palette = Palette {
    background: Color32::from_rgb(0x17, 0x19, 0x1b),
    sidebar: Color32::from_rgb(0x12, 0x13, 0x15),
    hover: Color32::from_rgb(0x20, 0x23, 0x26),
    selected: Color32::from_rgb(0x2a, 0x2e, 0x33),
    divider: Color32::from_rgb(0x25, 0x28, 0x2b),
    field: Color32::from_rgb(0x21, 0x24, 0x27),
    overlay: Color32::from_rgb(0x1e, 0x21, 0x24),
    text: Color32::from_rgb(0xec, 0xee, 0xf0),
    secondary: Color32::from_rgb(0xa3, 0xaa, 0xb0),
    dim: Color32::from_rgb(0x70, 0x78, 0x7f),
    // A calm blue that reads well on charcoal.
    accent: Color32::from_rgb(0x8a, 0xb4, 0xf8),
    star: Color32::from_rgb(0xf2, 0xc4, 0x6d),
    danger: Color32::from_rgb(0xf2, 0x8b, 0x82),
};

/// The same cool greys, inverted: a white reader, a sidebar a shade darker,
/// and a deeper blue so links and counts keep their contrast.
pub const LIGHT: Palette = Palette {
    background: Color32::from_rgb(0xff, 0xff, 0xff),
    sidebar: Color32::from_rgb(0xf4, 0xf5, 0xf7),
    hover: Color32::from_rgb(0xee, 0xf0, 0xf3),
    selected: Color32::from_rgb(0xe3, 0xe7, 0xed),
    divider: Color32::from_rgb(0xe3, 0xe6, 0xea),
    field: Color32::from_rgb(0xee, 0xf0, 0xf3),
    overlay: Color32::from_rgb(0xff, 0xff, 0xff),
    text: Color32::from_rgb(0x1a, 0x1c, 0x1e),
    secondary: Color32::from_rgb(0x4f, 0x56, 0x5d),
    dim: Color32::from_rgb(0x6b, 0x73, 0x7b),
    accent: Color32::from_rgb(0x1a, 0x5f, 0xcc),
    star: Color32::from_rgb(0xe0, 0x9c, 0x0b),
    danger: Color32::from_rgb(0xc5, 0x22, 0x1f),
};

impl Palette {
    pub fn of(theme: Theme) -> &'static Palette {
        match theme {
            Theme::Dark => &DARK,
            Theme::Light => &LIGHT,
        }
    }
}

/// The palette for the current appearance. egui follows the system's
/// light or dark mode unless `--appearance` overrides it, and falls back to
/// dark when macOS doesn't say.
pub fn palette(ctx: &egui::Context) -> &'static Palette {
    Palette::of(ctx.theme())
}

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

/// Sets fonts and both styles. egui switches between them when the macOS
/// appearance changes.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    for theme in [Theme::Dark, Theme::Light] {
        let mut style = (*ctx.style_of(theme)).clone();
        configure(&mut style, theme);
        ctx.set_style_of(theme, style);
    }
}

fn configure(style: &mut egui::Style, theme: Theme) {
    let palette = Palette::of(theme);
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
    *visuals = theme.default_visuals();
    visuals.panel_fill = palette.background;
    visuals.window_fill = palette.overlay;
    visuals.window_stroke = Stroke::new(1.0, palette.divider);
    visuals.extreme_bg_color = palette.field;
    visuals.faint_bg_color = palette.hover;
    visuals.override_text_color = Some(palette.text);
    visuals.hyperlink_color = palette.accent;
    visuals.selection.bg_fill = palette.accent.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    visuals.text_cursor.stroke = Stroke::new(2.0, palette.accent);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.divider);
    visuals.widgets.inactive.weak_bg_fill = palette.field;
    visuals.widgets.hovered.weak_bg_fill = palette.hover;
    visuals.widgets.active.weak_bg_fill = palette.selected;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs one frame with macOS reporting `system` as its appearance.
    fn frame(ctx: &egui::Context, system: Option<Theme>) {
        let input = egui::RawInput {
            system_theme: system,
            ..egui::RawInput::default()
        };
        ctx.run_ui(input, |_| {}).drop_without_applying_deltas();
    }

    #[test]
    fn the_palette_follows_the_macos_appearance() {
        let ctx = egui::Context::default();
        install(&ctx);

        frame(&ctx, Some(Theme::Light));
        assert_eq!(palette(&ctx), &LIGHT);
        assert_eq!(ctx.global_style().visuals.panel_fill, LIGHT.background);

        frame(&ctx, Some(Theme::Dark));
        assert_eq!(palette(&ctx), &DARK);
        assert_eq!(ctx.global_style().visuals.panel_fill, DARK.background);
    }

    #[test]
    fn dark_is_the_default_when_macos_does_not_say() {
        let ctx = egui::Context::default();
        install(&ctx);
        frame(&ctx, None);
        assert_eq!(palette(&ctx), &DARK);
    }

    #[test]
    fn an_explicit_appearance_wins_over_the_system() {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.set_theme(Theme::Light);
        frame(&ctx, Some(Theme::Dark));
        assert_eq!(palette(&ctx), &LIGHT);
    }

    #[test]
    fn text_and_links_are_readable_in_both_palettes() {
        for palette in [&DARK, &LIGHT] {
            // WCAG: 7:1 is AAA for body text, 4.5:1 is AA.
            assert!(contrast(palette.text, palette.background) >= 7.0);
            assert!(contrast(palette.secondary, palette.background) >= 4.5);
            assert!(contrast(palette.accent, palette.background) >= 4.5);
            assert!(contrast(palette.accent, palette.selected) >= 4.5);
        }
    }

    /// WCAG contrast ratio between two opaque colors.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let luminance = |color: Color32| {
            let channel = |value: u8| {
                let value = f32::from(value) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        };
        let (lighter, darker) = {
            let (a, b) = (luminance(a), luminance(b));
            (a.max(b), a.min(b))
        };
        (lighter + 0.05) / (darker + 0.05)
    }
}
