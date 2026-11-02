//! "Add iCloud account…": an address and an app-specific password, which
//! you make at account.apple.com. Mail never asks for your Apple Account
//! password, and iCloud wouldn't take it here.

use egui::{Align, Id, Key, Layout, RichText, TextEdit, Vec2};

use super::{state::Action, theme, view::overlay_frame};

/// Where you make an app-specific password: Sign-In and Security.
const APPLE_ACCOUNT: &str = "https://account.apple.com/account/manage";

#[derive(Default)]
pub struct IcloudForm {
    pub address: String,
    pub password: String,
    /// Checking the password with iCloud.
    pub waiting: bool,
    /// Why the last try failed.
    pub error: Option<String>,
    focused: bool,
}

/// Draws the form. Returns false once it should close.
pub fn draw(ctx: &egui::Context, form: &mut IcloudForm, actions: &mut Vec<Action>) -> bool {
    let palette = theme::palette(ctx);
    let mut open = true;
    let mut submit = false;
    let response = egui::Modal::new(Id::new("add-icloud"))
        .frame(overlay_frame(palette))
        .show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(RichText::new("Add an iCloud account").font(theme::semibold(18.0)));
            ui.add_space(10.0);
            let dim = |text: &str| {
                RichText::new(text)
                    .font(theme::regular(14.5))
                    .color(palette.dim)
            };
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(dim(
                    "Mail signs in with an app-specific password, not your Apple Account \
                     password. Make one at ",
                ));
                let link = ui
                    .add(
                        egui::Label::new(
                            RichText::new("account.apple.com")
                                .font(theme::regular(14.5))
                                .color(palette.accent),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if link.clicked() {
                    actions.push(Action::OpenLink(APPLE_ACCOUNT.to_owned()));
                }
                ui.label(dim(", under Sign-In and Security."));
            });
            ui.add_space(16.0);

            let waiting = form.waiting;
            let field = |ui: &mut egui::Ui, label: &str, edit: TextEdit| {
                ui.horizontal(|ui| {
                    // A fixed column, so both fields start at the same place.
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(108.0, 32.0), egui::Sense::hover());
                    ui.painter().text(
                        rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        label,
                        theme::regular(14.5),
                        palette.dim,
                    );
                    ui.add_enabled(
                        !waiting,
                        edit.font(theme::regular(15.5))
                            .margin(Vec2::new(10.0, 7.0))
                            .desired_width(f32::INFINITY),
                    )
                })
                .inner
            };
            let address = field(
                ui,
                "iCloud address",
                TextEdit::singleline(&mut form.address).hint_text("name@icloud.com"),
            );
            ui.add_space(6.0);
            let password = field(
                ui,
                "Password",
                TextEdit::singleline(&mut form.password)
                    .password(true)
                    .hint_text("xxxx-xxxx-xxxx-xxxx"),
            );
            if !form.focused {
                address.request_focus();
                form.focused = true;
            }
            let entered = ui.input(|input| input.key_pressed(Key::Enter));
            if entered && address.lost_focus() {
                password.request_focus();
            }
            if entered && password.lost_focus() {
                submit = true;
            }

            if let Some(error) = &form.error {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(error)
                        .font(theme::regular(14.0))
                        .color(palette.danger),
                );
            }
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Cancel   Esc").font(theme::regular(14.5)))
                    .clicked()
                {
                    open = false;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let text = if form.waiting {
                        "Checking with iCloud…"
                    } else {
                        "Add account   ↩"
                    };
                    let add = egui::Button::new(
                        RichText::new(text)
                            .font(theme::semibold(14.5))
                            .color(palette.background),
                    )
                    .fill(palette.accent);
                    if ui.add_enabled(!form.waiting, add).clicked() {
                        submit = true;
                    }
                });
            });
        });
    if response.should_close() {
        open = false;
    }
    let ready = !form.address.trim().is_empty() && !form.password.trim().is_empty();
    if submit && ready && !form.waiting {
        form.waiting = true;
        form.error = None;
        actions.push(Action::AddIcloudAccount {
            address: form.address.trim().to_owned(),
            password: form.password.clone(),
        });
    }
    open
}
