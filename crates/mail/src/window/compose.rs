//! The compose panel: a reply or a new message. It takes the reader's place
//! while open. ⌘S saves it to Gmail's Drafts; ⌘Enter asks to send it, and it
//! goes only after you confirm. Those keys can be changed in Settings.

use egui::{
    CornerRadius, Margin, Pos2, RichText, ScrollArea, Sense, Stroke, TextEdit, Vec2,
    text::{CCursor, CCursorRange},
};
use mail_core::{
    compose::{Draft, ReplyTo},
    model::Message,
};

use super::{
    keys::{Bindable, Keymap},
    theme, view,
};
use crate::dates;

const READING_WIDTH: f32 = 760.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    To,
    Body,
}

/// One of your accounts, as the From line shows it.
pub struct FromAccount {
    pub email: String,
    /// The first name it signs with; its initial stands in for a photo.
    pub name: String,
    pub photo: Option<egui::TextureHandle>,
}

pub struct Compose {
    pub title: &'static str,
    /// The address of the account it is written from.
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    reply: Option<ReplyTo>,
    /// Gmail's ID once saved, so later saves update the same draft.
    pub draft_id: Option<String>,
    /// What the fields held when opened or last saved: anything else is an
    /// unsaved edit.
    clean: Draft,
    /// The draft on its way to Drafts.
    saving: Option<Draft>,
    /// True from your confirmation until Gmail answers.
    send_pending: bool,
    /// Which field takes the cursor when the panel opens; the view takes it.
    pub focus: Option<Field>,
}

impl Compose {
    pub fn new_message(from: &str) -> Self {
        Self::from_draft("New message", from, Draft::default(), Field::To)
    }

    /// A reply to `message`, or to everyone on it with `all`.
    pub fn reply(message: &Message, own_address: &str, all: bool) -> Self {
        let date = dates::human(message.internal_date);
        let draft = Draft::reply(message, own_address, all, &date);
        let title = if all { "Reply all" } else { "Reply" };
        Self::from_draft(title, own_address, draft, Field::Body)
    }

    fn from_draft(title: &'static str, from: &str, draft: Draft, focus: Field) -> Self {
        Self {
            title,
            from: from.to_owned(),
            to: draft.to.clone(),
            cc: draft.cc.clone(),
            subject: draft.subject.clone(),
            body: draft.body.clone(),
            reply: draft.reply.clone(),
            draft_id: None,
            clean: draft,
            saving: None,
            send_pending: false,
            focus: Some(focus),
        }
    }

    pub fn draft(&self) -> Draft {
        Draft {
            to: self.to.clone(),
            cc: self.cc.clone(),
            subject: self.subject.clone(),
            body: self.body.clone(),
            reply: self.reply.clone(),
        }
    }

    /// A new message can come from any of your accounts; a reply stays
    /// with the conversation's.
    pub fn is_new_message(&self) -> bool {
        self.reply.is_none()
    }

    /// Writes from `from` instead. Returns the draft already saved in the
    /// old account's Drafts, for discarding there; the message is unsaved
    /// until saved in the new account's.
    pub fn change_from(&mut self, from: &str) -> Option<String> {
        self.from = from.to_owned();
        let draft_id = self.draft_id.take()?;
        self.clean = Draft::default();
        Some(draft_id)
    }

    pub fn is_dirty(&self) -> bool {
        self.draft() != self.clean
    }

    pub fn is_saving(&self) -> bool {
        self.saving.is_some()
    }

    pub fn is_sending(&self) -> bool {
        self.send_pending
    }

    /// Starts a save; returns the draft to save, or `None` while another
    /// save or a send is on its way.
    pub fn begin_save(&mut self) -> Option<Draft> {
        if self.saving.is_some() || self.send_pending {
            return None;
        }
        let draft = self.draft();
        self.saving = Some(draft.clone());
        Some(draft)
    }

    pub fn saved(&mut self, draft_id: String) {
        self.draft_id = Some(draft_id);
        if let Some(saved) = self.saving.take() {
            self.clean = saved;
        }
    }

    pub fn save_failed(&mut self) {
        self.saving = None;
    }

    /// Starts sending; returns the draft to send, or `None` while a save or
    /// another send is on its way.
    pub fn begin_send(&mut self) -> Option<Draft> {
        if self.saving.is_some() || self.send_pending {
            return None;
        }
        self.send_pending = true;
        Some(self.draft())
    }

    pub fn send_failed(&mut self) {
        self.send_pending = false;
    }
}

/// Draws the panel. Typing goes straight into the fields; shortcuts are
/// handled in `keys.rs`, and the footer shows the ones in effect. With more
/// than one account a From line shows which writes, and a new message can
/// change it. Returns the account picked there.
pub fn draw(
    ui: &mut egui::Ui,
    compose: &mut Compose,
    keymap: &Keymap,
    accounts: &[FromAccount],
) -> Option<String> {
    let palette = theme::palette(ui.ctx());
    let mut picked = None;
    let side = ((ui.available_width() - READING_WIDTH) / 2.0).clamp(36.0, 120.0) as i8;
    egui::Frame::new()
        .inner_margin(Margin {
            left: side,
            right: side,
            top: 28,
            bottom: 20,
        })
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(READING_WIDTH));
            ui.label(RichText::new(compose.title).font(theme::semibold(22.0)));
            ui.add_space(14.0);
            if accounts.len() > 1 {
                picked = from_field(ui, compose, accounts, palette);
            }

            let focus = compose.focus.take();
            let to = field(ui, "To", &mut compose.to, palette);
            if focus == Some(Field::To) {
                to.request_focus();
            }
            field(ui, "Cc", &mut compose.cc, palette);
            field(ui, "Subject", &mut compose.subject, palette);
            ui.add_space(10.0);

            let body_id = ui.id().with("compose-body");
            let footer_height = 44.0;
            ScrollArea::vertical()
                .id_salt("compose-scroll")
                .max_height((ui.available_height() - footer_height).max(120.0))
                .show(ui, |ui| {
                    let body = ui.add(
                        TextEdit::multiline(&mut compose.body)
                            .id(body_id)
                            .font(theme::regular(17.0))
                            .margin(Vec2::new(14.0, 12.0))
                            .desired_rows(16)
                            .desired_width(f32::INFINITY)
                            .lock_focus(true),
                    );
                    if focus == Some(Field::Body) {
                        body.request_focus();
                        // Replies start above the quoted message.
                        if let Some(mut state) = TextEdit::load_state(ui.ctx(), body_id) {
                            state
                                .cursor
                                .set_char_range(Some(CCursorRange::one(CCursor::new(0))));
                            state.store(ui.ctx(), body_id);
                        }
                    }
                });

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(footer_hints(keymap))
                        .font(theme::regular(13.0))
                        .color(palette.dim),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (status, color) = if compose.is_sending() {
                        ("Sending…", palette.accent)
                    } else if compose.is_saving() {
                        ("Saving…", palette.secondary)
                    } else if compose.is_dirty() {
                        ("Not saved", palette.dim)
                    } else if compose.draft_id.is_some() {
                        ("Saved in Drafts", palette.accent)
                    } else {
                        ("", palette.dim)
                    };
                    ui.label(RichText::new(status).font(theme::medium(13.0)).color(color));
                });
            });
        });
    picked
}

/// The From line: the account's photo and address, which open a menu of
/// your accounts for a new message. A reply stays with the conversation's
/// account. Returns the account picked.
fn from_field(
    ui: &mut egui::Ui,
    compose: &Compose,
    accounts: &[FromAccount],
    palette: &theme::Palette,
) -> Option<String> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.add_sized(
            [72.0, 32.0],
            egui::Label::new(
                RichText::new("From")
                    .font(theme::regular(14.5))
                    .color(palette.dim),
            ),
        );
        let shown = accounts
            .iter()
            .find(|account| account.email == compose.from);
        let changeable = compose.is_new_message();
        let opener = from_button(ui, shown, &compose.from, changeable);
        if !changeable {
            return;
        }
        egui::Popup::menu(&opener)
            .width(opener.rect.width().max(300.0))
            .gap(4.0)
            .show(|ui| {
                for account in accounts {
                    let chosen = account.email == compose.from;
                    if from_row(ui, account, chosen) {
                        if !chosen {
                            picked = Some(account.email.clone());
                        }
                        ui.close();
                    }
                }
            });
    });
    picked
}

/// The account a message is written from, its photo and address, drawn
/// like the fields below it; with a chevron when it can change.
fn from_button(
    ui: &mut egui::Ui,
    account: Option<&FromAccount>,
    email: &str,
    changeable: bool,
) -> egui::Response {
    let palette = theme::palette(ui.ctx());
    let address = ui
        .painter()
        .layout_no_wrap(email.to_owned(), theme::regular(15.5), palette.text);
    let (padding, photo, gap, chevron) = (10.0, 20.0, 8.0, 12.0);
    let width = padding
        + photo
        + gap
        + address.size().x
        + if changeable { gap + chevron } else { 0.0 }
        + padding;
    let sense = if changeable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), sense);
    if changeable {
        let fill = if response.hovered() || response.is_pointer_button_down_on() {
            palette.selected
        } else {
            ui.visuals().text_edit_bg_color()
        };
        let corner = ui.visuals().widgets.inactive.corner_radius;
        ui.painter().rect_filled(rect, corner, fill);
    }
    let photo_rect = egui::Rect::from_min_size(
        Pos2::new(rect.left() + padding, rect.center().y - photo / 2.0),
        Vec2::splat(photo),
    );
    let name = account.map_or(email, |account| account.name.as_str());
    view::paint_face(
        ui,
        account.and_then(|account| account.photo.as_ref()),
        name,
        photo_rect,
    );
    let address_width = address.size().x;
    let address_left = photo_rect.right() + gap;
    ui.painter().galley(
        Pos2::new(address_left, rect.center().y - address.size().y / 2.0),
        address,
        palette.text,
    );
    if changeable {
        let center = Pos2::new(
            address_left + address_width + gap + chevron / 2.0,
            rect.center().y + 1.0,
        );
        let stroke = Stroke::new(1.4, palette.secondary);
        ui.painter().line_segment(
            [center + Vec2::new(-3.5, -2.0), center + Vec2::new(0.0, 1.5)],
            stroke,
        );
        ui.painter().line_segment(
            [center + Vec2::new(0.0, 1.5), center + Vec2::new(3.5, -2.0)],
            stroke,
        );
        return response.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    response
}

/// One account in the From menu. Returns true when clicked.
fn from_row(ui: &mut egui::Ui, account: &FromAccount, chosen: bool) -> bool {
    let palette = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
    if chosen {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), palette.selected);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), palette.hover);
    }
    let photo = egui::Rect::from_min_size(
        Pos2::new(rect.left() + 8.0, rect.center().y - 12.0),
        Vec2::splat(24.0),
    );
    view::paint_face(ui, account.photo.as_ref(), &account.name, photo);
    ui.painter().text(
        Pos2::new(photo.right() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &account.email,
        theme::regular(14.5),
        palette.text,
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

fn field(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    palette: &theme::Palette,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_sized(
            [72.0, 32.0],
            egui::Label::new(
                RichText::new(label)
                    .font(theme::regular(14.5))
                    .color(palette.dim),
            ),
        );
        ui.add(
            TextEdit::singleline(text)
                .font(theme::regular(15.5))
                .margin(Vec2::new(10.0, 7.0))
                .desired_width(f32::INFINITY),
        )
    })
    .inner
}

/// "⌘↩ send    ⌘S save to Drafts    ⇧⌘D discard    Esc close", skipping
/// any action whose shortcuts were all removed.
fn footer_hints(keymap: &Keymap) -> String {
    let actions = [
        (Bindable::Send, "send"),
        (Bindable::SaveDraft, "save to Drafts"),
        (Bindable::DiscardDraft, "discard"),
    ];
    let mut hints: Vec<String> = actions
        .into_iter()
        .filter_map(|(bindable, what)| {
            let key = keymap.hint(bindable);
            (!key.is_empty()).then(|| format!("{key} {what}"))
        })
        .collect();
    hints.push("Esc close".to_owned());
    hints.join("    ")
}
