//! Notices as a toast at the bottom right, above the status bar: what just
//! happened, with Undo after triage. The model decides how long one stays;
//! here it fades in and out.
//!
//! WebKit draws over the window, so while a toast shows, a designed
//! message's page ends [`ROOM`] higher and leaves it a place to sit.

use egui::{Align2, Color32, CornerRadius, Id, Margin, Pos2, RichText, Sense, Stroke, Vec2};

use super::{state::Action, theme};

/// The status bar's height, which the toast sits above.
pub const STATUS_BAR: f32 = 32.0;
/// Space under a designed page kept for the toast.
pub const ROOM: f32 = 68.0;
/// From the window's right edge and the top of the status bar.
const MARGIN: f32 = 14.0;
const FADE: f32 = 0.16;

/// The toast on screen: its line and whether it offers Undo. Kept after
/// the model clears the notice, while it fades out.
#[derive(Clone, Default)]
pub struct Toast {
    shown: Option<(String, bool)>,
}

impl Toast {
    /// Whether a toast is on screen or about to be, so the page leaves room.
    pub fn is_showing(&self, notice: Option<&str>) -> bool {
        notice.is_some() || self.shown.is_some()
    }

    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        notice: Option<&str>,
        offers_undo: bool,
        undo_key: &str,
        actions: &mut Vec<Action>,
    ) {
        if let Some(text) = notice {
            self.shown = Some((text.to_owned(), offers_undo));
        }
        let shown = ctx.animate_bool_with_time(Id::new("toast"), notice.is_some(), FADE);
        if shown == 0.0 {
            self.shown = None;
        }
        let Some((text, undo)) = &self.shown else {
            return;
        };
        let palette = theme::palette(ctx);
        // Rises a few points as it fades in.
        let rise = (1.0 - shown) * 8.0;
        egui::Area::new(Id::new("toast-area"))
            .order(egui::Order::Foreground)
            .anchor(
                Align2::RIGHT_BOTTOM,
                Vec2::new(-MARGIN, -(STATUS_BAR + MARGIN) + rise),
            )
            .interactable(*undo)
            .show(ctx, |ui| {
                ui.set_opacity(shown);
                egui::Frame::new()
                    .fill(palette.overlay)
                    .stroke(Stroke::new(1.0, palette.divider))
                    .corner_radius(CornerRadius::same(10))
                    // Undo has padding of its own.
                    .inner_margin(Margin {
                        left: 16,
                        right: if *undo { 8 } else { 16 },
                        top: 0,
                        bottom: 0,
                    })
                    .shadow(egui::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: Color32::from_black_alpha(60),
                    })
                    .show(ui, |ui| {
                        ui.set_max_width(420.0);
                        ui.set_min_height(42.0);
                        ui.horizontal_centered(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(text.as_str())
                                        .font(theme::regular(14.5))
                                        .color(palette.text),
                                )
                                .wrap(),
                            );
                            if *undo {
                                ui.add_space(10.0);
                                if undo_button(ui, undo_key).clicked() {
                                    actions.push(Action::Undo);
                                }
                            }
                        });
                    });
            });
    }
}

/// "Undo" in the accent colour, then its key in a key cap.
fn undo_button(ui: &mut egui::Ui, key: &str) -> egui::Response {
    let palette = theme::palette(ui.ctx());
    let words = ui
        .painter()
        .layout_no_wrap("Undo".into(), theme::semibold(14.0), palette.accent);
    let cap = (!key.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(key.to_owned(), theme::medium(12.0), palette.secondary)
    });
    let cap_width = cap
        .as_ref()
        .map_or(0.0, |cap| (cap.size().x + 10.0).max(20.0));
    let padding = 8.0;
    let gap = if cap.is_some() { 7.0 } else { 0.0 };
    let width = padding * 2.0 + words.size().x + gap + cap_width;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(7), palette.hover);
    }
    let words_left = rect.left() + padding;
    let words_width = words.size().x;
    painter.galley(
        Pos2::new(words_left, rect.center().y - words.size().y / 2.0),
        words,
        palette.accent,
    );
    if let Some(cap) = cap {
        let cap_rect = egui::Rect::from_min_size(
            Pos2::new(words_left + words_width + gap, rect.center().y - 10.0),
            Vec2::new(cap_width, 20.0),
        );
        painter.rect_stroke(
            cap_rect,
            CornerRadius::same(5),
            Stroke::new(1.0, palette.dim),
            egui::StrokeKind::Inside,
        );
        painter.galley(cap_rect.center() - cap.size() / 2.0, cap, palette.secondary);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
