//! Drawing: sidebar, thread list, reader, status bar and the two overlays.
//! Clicks and keys become [`Action`]s applied to the [`Model`] after drawing.

use egui::{
    Align, Color32, CornerRadius, FontId, Id, Layout, Margin, Pos2, RichText, ScrollArea, Sense,
    Stroke, TextEdit, Vec2,
    text::{LayoutJob, TextFormat, TextWrapping},
};
use std::{collections::HashMap, ops::Range, path::PathBuf};

use mail_core::{
    model::{Account, Attachment, BodyLink, LabelKind, Message, ThreadSummary},
    paths::Paths,
};

use super::{
    compose,
    icloud_form::{self, IcloudForm},
    keys::{Bindable, Command, Focus, Keymap, Keys, Press},
    links,
    preferences::Preferences,
    shortcuts::{self, Settings},
    state::{Action, MAILBOXES, Model, OpenThread, OriginalHtml, SendSummary, View},
    theme::{self, Palette},
    toast::{self, Toast},
};
use crate::{accounts, dates};

const ROW_HEIGHT: f32 = 74.0;
const SIDEBAR_WIDTH: f32 = 228.0;
const LIST_WIDTH: f32 = 440.0;
const READING_WIDTH: f32 = 760.0;

/// Sync as the status bar reports it.
pub enum SyncStatus {
    Idle,
    Working(String),
    Failed(String),
    Demo,
}

/// Window state the model doesn't need: focus, scrolling, the search text.
pub struct Screen {
    keys: Keys,
    /// Where Settings saves changed shortcuts.
    shortcuts_path: PathBuf,
    /// Settings' other choices, and where they are saved.
    preferences: Preferences,
    preferences_path: PathBuf,
    settings: Option<Settings>,
    /// "Add iCloud account…" while it is open.
    pub icloud_form: Option<IcloudForm>,
    toast: Toast,
    paths: Paths,
    /// Profile photos by address, loaded once; `None` when there is none.
    photos: HashMap<String, Option<egui::TextureHandle>>,
    search_text: String,
    focus_search: bool,
    search_focused: bool,
    list_offset: f32,
    list_height: f32,
    reader_offset: f32,
    reader_height: f32,
    reader_target: Option<f32>,
    /// Where the original's WebKit view goes this frame; the shell places it.
    pub original_rect: Option<egui::Rect>,
    /// Pages to scroll the original by, for Space; the shell does it.
    pub scroll_original: Option<f32>,
    /// False when originals can't be shown, as in screenshots.
    pub original_available: bool,
    pub status: SyncStatus,
    pub sync_requested: bool,
}

impl Screen {
    pub fn new(status: SyncStatus, paths: Paths, preferences: Preferences) -> Self {
        let shortcuts_path = paths.root.join("shortcuts.json");
        Self {
            keys: Keys::new(Keymap::load(&shortcuts_path)),
            shortcuts_path,
            preferences,
            preferences_path: Preferences::path(&paths),
            settings: None,
            icloud_form: None,
            toast: Toast::default(),
            paths,
            photos: HashMap::new(),
            search_text: String::new(),
            focus_search: false,
            search_focused: false,
            list_offset: 0.0,
            list_height: 0.0,
            reader_offset: 0.0,
            reader_height: 0.0,
            reader_target: None,
            original_rect: None,
            scroll_original: None,
            original_available: true,
            status,
            sync_requested: false,
        }
    }

    /// Loads the photo again next time it is drawn, after it was saved.
    pub fn forget_photo(&mut self, email: &str) {
        self.photos.remove(email);
    }

    fn photo(&mut self, ctx: &egui::Context, email: &str) -> Option<&egui::TextureHandle> {
        if !self.photos.contains_key(email) {
            let texture = load_photo(ctx, &self.paths, email);
            self.photos.insert(email.to_owned(), texture);
        }
        self.photos.get(email)?.as_ref()
    }
}

/// Decodes a saved photo into a texture, scaled for a sharp small circle.
fn load_photo(ctx: &egui::Context, paths: &Paths, email: &str) -> Option<egui::TextureHandle> {
    let photo = accounts::read_photo(paths, email)?
        .resize_to_fill(128, 128, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let size = [photo.width() as usize, photo.height() as usize];
    let pixels = egui::ColorImage::from_rgba_unmultiplied(size, photo.as_raw());
    Some(ctx.load_texture(
        format!("photo:{email}"),
        pixels,
        egui::TextureOptions::LINEAR,
    ))
}

pub fn draw(ui: &mut egui::Ui, model: &mut Model, screen: &mut Screen) {
    screen.original_rect = None;
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    handle_keys(ui.ctx(), model, screen, &mut actions);

    egui::Panel::bottom("status")
        .exact_size(toast::STATUS_BAR)
        .frame(
            egui::Frame::new()
                .fill(palette.sidebar)
                .inner_margin(Margin::symmetric(18, 0)),
        )
        .show(ui, |ui| status_bar(ui, model, screen, &mut actions));
    egui::Panel::left("sidebar")
        .exact_size(SIDEBAR_WIDTH)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(palette.sidebar)
                .inner_margin(Margin::symmetric(12, 20)),
        )
        .show(ui, |ui| sidebar(ui, model, screen, &mut actions));
    egui::Panel::left("threads")
        .exact_size(LIST_WIDTH)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(palette.background)
                .stroke(Stroke::new(1.0, palette.divider)),
        )
        .show(ui, |ui| thread_list(ui, model, screen, &mut actions));
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(palette.background))
        .show(ui, |ui| {
            let from = if model.compose.is_some() {
                from_accounts(ui.ctx(), model, screen)
            } else {
                Vec::new()
            };
            match &mut model.compose {
                Some(compose) => {
                    if let Some(address) = compose::draw(ui, compose, &screen.keys.keymap, &from) {
                        actions.push(Action::SetFrom(address));
                    }
                }
                None => reader(ui, model, screen, &mut actions),
            }
        });

    if model.picker.is_some() {
        label_picker(ui.ctx(), model, &mut actions);
    }
    if model.help {
        shortcuts::help_overlay(ui.ctx(), &screen.keys.keymap, &mut actions);
    }
    if let Some(settings) = &mut screen.settings {
        let before = screen.preferences.clone();
        let open = shortcuts::settings_window(
            ui.ctx(),
            settings,
            &mut screen.keys.keymap,
            &screen.shortcuts_path,
            &mut screen.preferences,
            &screen.preferences_path,
        );
        let after = &screen.preferences;
        if after.show_original != before.show_original {
            // Text it is, whatever the setting, where originals can't show.
            let show = after.show_original && screen.original_available;
            actions.push(Action::SetShowOriginal(show));
        }
        if after.remote_images != before.remote_images {
            actions.push(Action::SetRemoteImages(after.remote_images));
        }
        if !open {
            screen.settings = None;
        }
    }
    if let Some(summary) = &model.confirm_send {
        confirm_send(ui.ctx(), summary, &mut actions);
    }
    if let Some(form) = &mut screen.icloud_form
        && !icloud_form::draw(ui.ctx(), form, &mut actions)
    {
        screen.icloud_form = None;
    }
    let undo_key = screen.keys.keymap.hint(Bindable::Undo);
    screen.toast.draw(
        ui.ctx(),
        model.notice(),
        model.notice_offers_undo(),
        &undo_key,
        &mut actions,
    );

    for action in actions {
        let clears_search = action == Action::ClearSearch;
        if let Err(error) = model.apply(action) {
            model.notify(format!("{error:#}"));
        }
        if clears_search {
            screen.search_text.clear();
        }
    }
    if !matches!(model.view, View::Search(_)) && !screen.search_focused {
        screen.search_text.clear();
    }
}

fn handle_keys(ctx: &egui::Context, model: &Model, screen: &mut Screen, actions: &mut Vec<Action>) {
    // Its fields take the typing; it closes itself on Esc.
    if screen.icloud_form.is_some() {
        return;
    }
    let focus = if screen.settings.is_some() {
        Focus::Settings
    } else if model.help {
        Focus::Help
    } else if model.picker.is_some() {
        Focus::Picker
    } else if model.confirm_send.is_some() {
        Focus::ConfirmSend
    } else if model.compose.is_some() {
        Focus::Compose
    } else if screen.search_focused {
        Focus::Search
    } else {
        Focus::List
    };
    let held = ctx.input(|input| held_presses(&input.events));
    let presses: Vec<Press> = held.iter().map(|(press, _)| press.clone()).collect();
    if let Some(settings) = &mut screen.settings {
        settings.record(
            ctx,
            &presses,
            &mut screen.keys.keymap,
            &screen.shortcuts_path,
        );
        return;
    }
    for (press, repeat) in held {
        match screen.keys.command(&press, focus) {
            Some(Command::Act(action)) => {
                // Keep send keys away from the text fields: ⌘Enter and the
                // confirming Enter must not also type a new line.
                if matches!(action, Action::RequestSend | Action::ConfirmSend)
                    && let Press::Key(key, modifiers) = press
                {
                    ctx.input_mut(|input| input.consume_key(modifiers, key));
                    ctx.memory_mut(|memory| memory.stop_text_input());
                }
                // A held key must not confirm a send or trash a run of threads.
                if !(repeat && action.once_per_press()) {
                    actions.push(action);
                }
            }
            Some(Command::FocusSearch) => screen.focus_search = true,
            Some(Command::FocusList) => {
                ctx.memory_mut(|memory| memory.surrender_focus(search_id()))
            }
            Some(Command::ScrollReader(direction)) => {
                let page = (screen.reader_height * 0.85).max(200.0);
                screen.reader_target = Some((screen.reader_offset + direction * page).max(0.0));
                screen.scroll_original = Some(direction);
            }
            Some(Command::SyncNow) => screen.sync_requested = true,
            Some(Command::OpenSettings) => screen.settings = Some(Settings::default()),
            None => {}
        }
    }
}

fn search_id() -> Id {
    Id::new("search")
}

// Sidebar

fn sidebar(ui: &mut egui::Ui, model: &Model, screen: &mut Screen, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    account_header(ui, model, screen, actions);
    ui.add_space(22.0);

    let current = match &model.view {
        View::Mailbox(id) => Some(id.as_str()),
        View::Search(_) => None,
    };
    for (id, name) in MAILBOXES {
        let count = (*id == "INBOX").then(|| model.unread(id));
        if nav_item(ui, name, count, current == Some(id)) {
            actions.push(Action::Show((*id).into()));
        }
    }

    // With every account shown, each account's labels sit under its address;
    // choosing one shows that account.
    let grouped = model.scope.is_none() && model.accounts.len() > 1;
    let groups: Vec<_> = model
        .shown_accounts()
        .map(|account| {
            let labels: Vec<_> = model
                .labels(account.id)
                .iter()
                .filter(|label| label.kind == LabelKind::User)
                .collect();
            (account, labels)
        })
        .filter(|(_, labels)| !labels.is_empty())
        .collect();
    if groups.is_empty() {
        return;
    }
    ui.add_space(20.0);
    ui.label(
        RichText::new("LABELS")
            .font(theme::medium(11.5))
            .color(palette.dim),
    );
    ui.add_space(4.0);
    ScrollArea::vertical().id_salt("labels").show(ui, |ui| {
        for (account, labels) in groups {
            if grouped {
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(&account.email)
                            .font(theme::regular(12.5))
                            .color(palette.dim),
                    )
                    .truncate(),
                );
            }
            for label in labels {
                let count = Some(model.unread_in(account.id, &label.id));
                let selected = !grouped && current == Some(label.id.as_str());
                if nav_item(ui, &label.name, count, selected) {
                    if grouped {
                        actions.push(Action::SetScope(Some(account.id)));
                    }
                    actions.push(Action::Show(label.id.clone()));
                }
            }
        }
    });
}

/// Your photo and first name, and the account shown: an address, or "All"
/// when several accounts are. Clicking the account opens a menu of them.
fn account_header(
    ui: &mut egui::Ui,
    model: &Model,
    screen: &mut Screen,
    actions: &mut Vec<Action>,
) {
    let palette = theme::palette(ui.ctx());
    let several = model.accounts.len() > 1;
    let shown = model.scope.and_then(|id| model.account(id));
    let header_face = model.header_account().map(|account| face(model, account));
    // Your first name, from the mail you send; "Mail" until there is some.
    let title = model
        .header_account()
        .and_then(|account| model.first_name(account.id))
        .unwrap_or("Mail")
        .to_owned();
    let name = match shown {
        Some(account) => account.email.clone(),
        None if several => "All".to_owned(),
        None => model
            .accounts
            .first()
            .map(|account| account.email.clone())
            .unwrap_or_default(),
    };

    let mut opener = None;
    ui.horizontal(|ui| {
        let size = 38.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
        if let Some(header_face) = &header_face {
            paint_avatar(ui, screen, header_face, rect);
        }
        ui.vertical(|ui| {
            ui.add_space(1.0);
            ui.label(
                RichText::new(title)
                    .font(theme::semibold(19.0))
                    .color(palette.text),
            );
            opener = Some(account_button(ui, &name));
        });
    });
    let Some(opener) = opener else { return };
    egui::Popup::menu(&opener)
        .width(260.0)
        .gap(6.0)
        .show(|ui| account_menu(ui, model, screen, actions));
}

/// The account's name with a chevron, as one clickable thing.
fn account_button(ui: &mut egui::Ui, name: &str) -> egui::Response {
    let palette = theme::palette(ui.ctx());
    let chevron = 12.0;
    let max_text = (ui.available_width() - chevron - 6.0).max(20.0);
    let galley = one_line(
        ui,
        &[(name.to_owned(), theme::regular(13.0), palette.dim)],
        max_text,
    );
    let size = Vec2::new(galley.size().x + 6.0 + chevron, galley.size().y.max(18.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let color = if response.hovered() {
        palette.secondary
    } else {
        palette.dim
    };
    let text_at = Pos2::new(rect.left(), rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text_at, galley, color);
    // Inter has no ▾, so the chevron is two short strokes.
    let center = Pos2::new(rect.right() - chevron / 2.0, rect.center().y + 1.0);
    let stroke = Stroke::new(1.4, color);
    ui.painter().line_segment(
        [center + Vec2::new(-3.5, -2.0), center + Vec2::new(0.0, 1.5)],
        stroke,
    );
    ui.painter().line_segment(
        [center + Vec2::new(0.0, 1.5), center + Vec2::new(3.5, -2.0)],
        stroke,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn account_menu(ui: &mut egui::Ui, model: &Model, screen: &mut Screen, actions: &mut Vec<Action>) {
    let mut chosen = None;
    if model.accounts.len() > 1 {
        let faces: Vec<Face> = model
            .accounts
            .iter()
            .map(|account| face(model, account))
            .collect();
        let count: usize = model
            .accounts
            .iter()
            .map(|account| model.unread_in(account.id, "INBOX"))
            .sum();
        if account_row(
            ui,
            screen,
            &faces,
            "All accounts",
            count,
            model.scope.is_none(),
        ) {
            chosen = Some(Action::SetScope(None));
        }
    }
    for account in &model.accounts {
        let count = model.unread_in(account.id, "INBOX");
        let selected = model.scope == Some(account.id) || model.accounts.len() == 1;
        if account_row(
            ui,
            screen,
            &[face(model, account)],
            &account.email,
            count,
            selected,
        ) {
            chosen = Some(Action::SetScope(Some(account.id)));
        }
    }
    ui.separator();
    if menu_row(ui, MenuIcon::Plus, "Add Gmail account…") {
        chosen = Some(Action::AddAccount);
    }
    if menu_row(ui, MenuIcon::Plus, "Add iCloud account…") {
        screen.icloud_form = Some(IcloudForm::default());
        ui.close();
    }
    if menu_row(ui, MenuIcon::Photo, "Choose photo…") {
        chosen = Some(Action::ChoosePhoto);
    }
    let has_chosen = model
        .header_account()
        .is_some_and(|account| screen.paths.chosen_photo(&account.email).exists());
    if has_chosen && menu_row(ui, MenuIcon::Undo, "Use Google photo") {
        chosen = Some(Action::UseGooglePhoto);
    }
    if let Some(action) = chosen {
        actions.push(action);
        ui.close();
    }
}

/// One row of the account menu: photos, a name and the unread count in the
/// inbox. Returns true when clicked.
fn account_row(
    ui: &mut egui::Ui,
    screen: &mut Screen,
    photos: &[Face],
    name: &str,
    unread: usize,
    selected: bool,
) -> bool {
    let palette = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), palette.selected);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), palette.hover);
    }
    // Several photos overlap a little, the first on top.
    let size = 24.0;
    let left = rect.left() + 8.0;
    for (index, photo) in photos.iter().take(3).enumerate().rev() {
        let at = Pos2::new(left + index as f32 * 12.0, rect.center().y - size / 2.0);
        let circle = egui::Rect::from_min_size(at, Vec2::splat(size));
        if photos.len() > 1 {
            ui.painter()
                .circle_filled(circle.center(), size / 2.0 + 1.5, palette.overlay);
        }
        paint_avatar(ui, screen, photo, circle);
    }
    let text_left = left + size + 12.0 * (photos.len().min(3) - 1) as f32 + 10.0;
    let count_width = if unread > 0 { 34.0 } else { 0.0 };
    let name = one_line(
        ui,
        &[(name.to_owned(), theme::regular(14.5), palette.text)],
        rect.right() - text_left - count_width - 8.0,
    );
    let name_at = Pos2::new(text_left, rect.center().y - name.size().y / 2.0);
    ui.painter().galley(name_at, name, palette.text);
    if unread > 0 {
        ui.painter().text(
            Pos2::new(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            unread.to_string(),
            theme::medium(13.0),
            palette.accent,
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

enum MenuIcon {
    Plus,
    Photo,
    Undo,
}

/// A command in the account menu, lined up with the account rows, its icon
/// drawn in a ring where a photo would be. Returns true when clicked.
fn menu_row(ui: &mut egui::Ui, icon: MenuIcon, text: &str) -> bool {
    let palette = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), palette.hover);
    }
    let center = Pos2::new(rect.left() + 20.0, rect.center().y);
    let painter = ui.painter();
    painter.circle_stroke(center, 11.5, Stroke::new(1.0, palette.dim));
    let stroke = Stroke::new(1.4, palette.secondary);
    let line = |from: Vec2, to: Vec2| painter.line_segment([center + from, center + to], stroke);
    match icon {
        MenuIcon::Plus => {
            line(Vec2::new(-5.0, 0.0), Vec2::new(5.0, 0.0));
            line(Vec2::new(0.0, -5.0), Vec2::new(0.0, 5.0));
        }
        // A head and shoulders.
        MenuIcon::Photo => {
            painter.circle_stroke(center - Vec2::Y * 2.5, 2.8, stroke);
            line(Vec2::new(-5.0, 6.0), Vec2::new(-3.0, 2.5));
            line(Vec2::new(-3.0, 2.5), Vec2::new(3.0, 2.5));
            line(Vec2::new(3.0, 2.5), Vec2::new(5.0, 6.0));
        }
        // An arrow pointing back.
        MenuIcon::Undo => {
            line(Vec2::new(-5.0, 0.0), Vec2::new(5.0, 0.0));
            line(Vec2::new(-5.0, 0.0), Vec2::new(-1.5, -3.5));
            line(Vec2::new(-5.0, 0.0), Vec2::new(-1.5, 3.5));
        }
    }
    painter.text(
        Pos2::new(rect.left() + 42.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        theme::regular(14.5),
        palette.text,
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Whose photo to draw: the address it is saved under, and the name whose
/// first letter stands in when there is no photo.
struct Face<'a> {
    email: &'a str,
    name: &'a str,
}

/// Your accounts with their names and photos, for the compose From line.
fn from_accounts(
    ctx: &egui::Context,
    model: &Model,
    screen: &mut Screen,
) -> Vec<compose::FromAccount> {
    model
        .accounts
        .iter()
        .map(|account| {
            let face = face(model, account);
            compose::FromAccount {
                email: account.email.clone(),
                name: face.name.to_owned(),
                photo: screen.photo(ctx, &account.email).cloned(),
            }
        })
        .collect()
}

fn face<'a>(model: &'a Model, account: &'a Account) -> Face<'a> {
    Face {
        email: &account.email,
        name: model.first_name(account.id).unwrap_or(&account.email),
    }
}

/// A round profile photo, or the first letter of the name when there is none.
fn paint_avatar(ui: &egui::Ui, screen: &mut Screen, face: &Face, rect: egui::Rect) {
    paint_face(ui, screen.photo(ui.ctx(), face.email), face.name, rect);
}

/// A photo in a circle, or the name's initial when there is none.
pub(super) fn paint_face(
    ui: &egui::Ui,
    photo: Option<&egui::TextureHandle>,
    name: &str,
    rect: egui::Rect,
) {
    let palette = theme::palette(ui.ctx());
    let radius = rect.width() / 2.0;
    match photo {
        Some(texture) => {
            egui::Image::new((texture.id(), rect.size()))
                .corner_radius(CornerRadius::same(radius.round() as u8))
                .paint_at(ui, rect);
        }
        None => {
            // A tint of grey rather than a fixed one, so the circle shows on
            // hovered and chosen rows too.
            ui.painter()
                .circle_filled(rect.center(), radius, palette.dim.gamma_multiply(0.35));
            let initial = name
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default();
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                initial,
                theme::semibold(radius * 0.95),
                palette.text,
            );
        }
    }
}

/// One sidebar row. Returns true when clicked.
fn nav_item(ui: &mut egui::Ui, name: &str, count: Option<usize>, selected: bool) -> bool {
    let palette = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(8), palette.selected);
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(8), palette.hover);
    }
    let font = if selected {
        theme::medium(15.0)
    } else {
        theme::regular(15.0)
    };
    let color = if selected {
        palette.text
    } else {
        palette.secondary
    };
    painter.text(
        Pos2::new(rect.left() + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        font,
        color,
    );
    if let Some(count) = count.filter(|count| *count > 0) {
        painter.text(
            Pos2::new(rect.right() - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            count.to_string(),
            theme::medium(13.0),
            palette.accent,
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

// Thread list

fn thread_list(
    ui: &mut egui::Ui,
    model: &mut Model,
    screen: &mut Screen,
    actions: &mut Vec<Action>,
) {
    let palette = theme::palette(ui.ctx());
    egui::Frame::new()
        .inner_margin(Margin {
            left: 18,
            right: 18,
            top: 18,
            bottom: 10,
        })
        .show(ui, |ui| {
            let field = TextEdit::singleline(&mut screen.search_text)
                .id(search_id())
                .hint_text("Search")
                .font(theme::regular(15.0))
                .margin(Vec2::new(12.0, 9.0))
                .desired_width(f32::INFINITY);
            let response = ui.add(field);
            let search_key = screen.keys.keymap.hint(Bindable::Search);
            if !response.has_focus() && screen.search_text.is_empty() && !search_key.is_empty() {
                key_hint(ui, response.rect, &search_key);
            }
            if std::mem::take(&mut screen.focus_search) {
                response.request_focus();
            }
            screen.search_focused = response.has_focus();
            if response.changed() {
                actions.push(Action::Search(screen.search_text.clone()));
            }

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(view_title(model))
                        .font(theme::semibold(22.0))
                        .color(palette.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let count = model.threads.len();
                    let noun = if count == 1 {
                        "conversation"
                    } else {
                        "conversations"
                    };
                    ui.label(
                        RichText::new(format!("{count} {noun}"))
                            .font(theme::regular(13.0))
                            .color(palette.dim),
                    );
                });
            });
        });
    let separator = ui.available_rect_before_wrap();
    ui.painter().hline(
        separator.x_range(),
        separator.top(),
        Stroke::new(1.0, palette.divider),
    );

    if model.threads.is_empty() {
        let message = match &model.view {
            View::Mailbox(id) if id == "INBOX" => "Inbox zero.",
            View::Search(_) => "No matches.",
            View::Mailbox(_) => "Nothing here.",
        };
        ui.add_space(80.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(message)
                    .font(theme::regular(18.0))
                    .color(palette.dim),
            );
        });
        return;
    }

    let mut area = ScrollArea::vertical()
        .id_salt("thread-list")
        .auto_shrink(false);
    if std::mem::take(&mut model.reveal_selected) {
        let top = model.selected as f32 * ROW_HEIGHT;
        if top < screen.list_offset {
            area = area.vertical_scroll_offset(top);
        } else if top + ROW_HEIGHT > screen.list_offset + screen.list_height {
            area = area.vertical_scroll_offset(top + ROW_HEIGHT - screen.list_height);
        }
    }
    let keymap = &screen.keys.keymap;
    let output = area.show_rows(ui, ROW_HEIGHT, model.threads.len(), |ui, rows| {
        for index in rows {
            let thread = &model.threads[index];
            let selected = index == model.selected;
            let response = thread_row(ui, thread, selected);
            // A right click selects the row first, so the menu acts on it.
            if response.clicked() || (response.secondary_clicked() && !selected) {
                actions.push(Action::Select(index));
            }
            response.context_menu(|ui| conversation_menu(ui, thread, keymap, actions));
        }
    });
    screen.list_offset = output.state.offset.y;
    screen.list_height = output.inner_rect.height();
}

/// A small key cap at the right end of a field, like `⌘K` for search.
fn key_hint(ui: &egui::Ui, field: egui::Rect, key: &str) {
    let palette = theme::palette(ui.ctx());
    let galley =
        ui.painter()
            .layout_no_wrap(key.to_owned(), theme::medium(12.5), palette.secondary);
    let width = (galley.size().x + 10.0).max(22.0);
    let center = Pos2::new(field.right() - 11.0 - width / 2.0, field.center().y);
    let cap = egui::Rect::from_center_size(center, Vec2::new(width, 22.0));
    ui.painter().rect_stroke(
        cap,
        CornerRadius::same(5),
        Stroke::new(1.0, palette.dim),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(center - galley.size() / 2.0, galley, palette.secondary);
}

fn view_title(model: &Model) -> String {
    match &model.view {
        View::Search(text) => format!("“{}”", text.trim()),
        View::Mailbox(id) => MAILBOXES
            .iter()
            .find(|(mailbox, _)| mailbox == id)
            .map(|(_, name)| (*name).to_owned())
            .or_else(|| {
                model
                    .shown_accounts()
                    .flat_map(|account| model.labels(account.id))
                    .find(|label| &label.id == id)
                    .map(|label| label.name.clone())
            })
            .unwrap_or_else(|| id.clone()),
    }
}

/// One conversation in the list.
fn thread_row(ui: &mut egui::Ui, thread: &ThreadSummary, selected: bool) -> egui::Response {
    let palette = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::ZERO, palette.selected);
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::ZERO, palette.hover);
    }
    if thread.unread {
        painter.circle_filled(
            Pos2::new(rect.left() + 11.0, rect.top() + 24.0),
            4.0,
            palette.accent,
        );
    }

    let left = rect.left() + 24.0;
    let right = rect.right() - 18.0;
    let date = dates::short(thread.last_date);
    let date_font = if thread.unread {
        theme::medium(13.0)
    } else {
        theme::regular(13.0)
    };
    let date_color = if thread.unread {
        palette.text
    } else {
        palette.dim
    };
    let date_galley = painter.layout_no_wrap(date, date_font, date_color);
    let date_width = date_galley.size().x;
    painter.galley(
        Pos2::new(right - date_width, rect.top() + 15.0),
        date_galley,
        date_color,
    );

    let sender_font = if thread.unread {
        theme::semibold(15.5)
    } else {
        theme::regular(15.5)
    };
    let mut parts = vec![(thread.senders.join(", "), sender_font, palette.text)];
    if thread.message_count > 1 {
        parts.push((
            format!("  {}", thread.message_count),
            theme::regular(14.0),
            palette.dim,
        ));
    }
    let sender = one_line(ui, &parts, right - left - date_width - 12.0);
    painter.galley(Pos2::new(left, rect.top() + 13.0), sender, palette.text);

    let subject_font = if thread.unread {
        theme::medium(14.5)
    } else {
        theme::regular(14.5)
    };
    let mut parts = vec![(
        subject_or_placeholder(&thread.subject),
        subject_font,
        palette.text,
    )];
    if !thread.snippet.is_empty() {
        parts.push((
            format!("  {}", thread.snippet),
            theme::regular(14.5),
            palette.secondary,
        ));
    }
    let star_room = if thread.starred { 22.0 } else { 0.0 };
    let preview = one_line(ui, &parts, right - left - star_room);
    painter.galley(Pos2::new(left, rect.top() + 40.0), preview, palette.text);
    if thread.starred {
        painter.text(
            Pos2::new(right, rect.top() + 49.0),
            egui::Align2::RIGHT_CENTER,
            "★",
            theme::regular(14.0),
            palette.star,
        );
    }
    painter.hline(
        left..=rect.right(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, palette.divider),
    );
    response
}

/// The right-click menu on a conversation, with each item's shortcut.
fn conversation_menu(
    ui: &mut egui::Ui,
    thread: &ThreadSummary,
    keymap: &Keymap,
    actions: &mut Vec<Action>,
) {
    ui.set_min_width(230.0);
    let in_inbox = thread.label_ids.iter().any(|id| id == "INBOX");
    let read = if thread.unread {
        "Mark as read"
    } else {
        "Mark as unread"
    };
    let star = if thread.starred { "Unstar" } else { "Star" };
    let groups: [&[(&str, Bindable, Action)]; 3] = [
        &[
            ("Archive", Bindable::Archive, Action::Archive),
            ("Move to Trash", Bindable::Trash, Action::Trash),
            (read, Bindable::ToggleUnread, Action::ToggleUnread),
            (star, Bindable::ToggleStar, Action::ToggleStar),
            ("Label…", Bindable::Label, Action::OpenPicker),
        ],
        &[
            ("Reply", Bindable::Reply, Action::Reply),
            ("Reply all", Bindable::ReplyAll, Action::ReplyAll),
        ],
        &[(
            "Open the HTML version",
            Bindable::OpenHtml,
            Action::OpenHtml,
        )],
    ];
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            ui.separator();
        }
        for (label, bindable, action) in group {
            if *bindable == Bindable::Archive && !in_inbox {
                continue;
            }
            let item = egui::Button::new(RichText::new(*label).font(theme::regular(14.5)))
                .shortcut_text(
                    RichText::new(keymap.menu_hint(*bindable)).font(theme::regular(13.5)),
                );
            if ui.add(item).clicked() {
                actions.push(action.clone());
                ui.close();
            }
        }
    }
}

/// Lays out text pieces on one line, ending in "…" if they don't fit.
fn one_line(
    ui: &egui::Ui,
    parts: &[(String, FontId, Color32)],
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    for (text, font, color) in parts {
        job.append(text, 0.0, TextFormat::simple(font.clone(), *color));
    }
    job.wrap = TextWrapping {
        max_width: max_width.max(10.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

fn subject_or_placeholder(subject: &str) -> String {
    if subject.trim().is_empty() {
        "(no subject)".to_owned()
    } else {
        subject.trim().to_owned()
    }
}

// Reader

fn reader(ui: &mut egui::Ui, model: &Model, screen: &mut Screen, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    let Some(open) = &model.open else {
        ui.add_space(120.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("No conversation selected.")
                    .font(theme::regular(18.0))
                    .color(palette.dim),
            );
        });
        return;
    };
    if let Some(original) = model.showing_original() {
        return original_reader(ui, model, open, original, screen, actions);
    }

    let mut area = ScrollArea::vertical()
        .id_salt(("reader", &open.thread_id))
        .auto_shrink(false);
    if let Some(target) = screen.reader_target.take() {
        area = area.vertical_scroll_offset(target);
    }
    let output = area.show(ui, |ui| {
        let side = ((ui.available_width() - READING_WIDTH) / 2.0).clamp(36.0, 120.0) as i8;
        egui::Frame::new()
            .inner_margin(Margin {
                left: side,
                right: side,
                top: 30,
                bottom: 48,
            })
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(READING_WIDTH));
                ui.add(
                    egui::Label::new(
                        RichText::new(subject_or_placeholder(&open.subject))
                            .font(theme::semibold(26.0)),
                    )
                    .wrap(),
                );
                let own_address = model.own_address(open.account_id);
                let labels: Vec<&str> = model
                    .labels(open.account_id)
                    .iter()
                    .filter(|label| label.kind == LabelKind::User)
                    .filter(|label| open.messages.iter().any(|m| m.has_label(&label.id)))
                    .map(|label| label.name.as_str())
                    .collect();
                if !labels.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(labels.join("  ·  "))
                            .font(theme::medium(13.0))
                            .color(palette.accent),
                    );
                }
                // Its original is on its way: the headers and text show
                // meanwhile rather than an empty pane.
                if screen.original_available
                    && open.loading_original()
                    && model.show_original
                    && model.compose.is_none()
                {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Loading the original…")
                            .font(theme::regular(13.0))
                            .color(palette.dim),
                    );
                } else if screen.original_available
                    && (open.original.is_some() || open.loading_original())
                {
                    // A message with an original, read as text for now.
                    ui.add_space(8.0);
                    let toggle = screen.keys.keymap.hint(Bindable::ToggleOriginal);
                    if pill_button(ui, ButtonIcon::Original, "Show original", &toggle).clicked() {
                        actions.push(Action::ToggleOriginal);
                    }
                }
                ui.add_space(18.0);

                for (index, message) in open.messages.iter().enumerate() {
                    let expanded = open.expanded.get(index).copied().unwrap_or(true);
                    let toggled = if expanded {
                        expanded_message(ui, index, message, own_address, actions)
                    } else {
                        collapsed_message(ui, message, own_address)
                    };
                    if toggled && open.messages.len() > 1 {
                        actions.push(Action::ToggleExpanded(index));
                    }
                    if index + 1 < open.messages.len() {
                        ui.add_space(14.0);
                        let line = ui.available_rect_before_wrap();
                        ui.painter().hline(
                            line.x_range(),
                            line.top(),
                            Stroke::new(1.0, palette.divider),
                        );
                        ui.add_space(14.0);
                    }
                }
            });
    });
    screen.reader_offset = output.state.offset.y;
    screen.reader_height = output.inner_rect.height();
}

/// A message shown as it was designed: Mail draws the subject and sender,
/// and WebKit draws the message below them, in the rect left for it.
fn original_reader(
    ui: &mut egui::Ui,
    model: &Model,
    open: &OpenThread,
    original: &OriginalHtml,
    screen: &mut Screen,
    actions: &mut Vec<Action>,
) {
    let palette = theme::palette(ui.ctx());
    let Some(message) = open.messages.first() else {
        return;
    };
    let panel = ui.max_rect();
    let side = ((ui.available_width() - READING_WIDTH) / 2.0).clamp(36.0, 120.0);
    let column = egui::Rect::from_min_max(
        Pos2::new(panel.left() + side, panel.top() + 30.0),
        Pos2::new(panel.right() - side, panel.bottom() - 16.0),
    );
    let mut header = ui.new_child(egui::UiBuilder::new().max_rect(column));
    let ui = &mut header;
    ui.add(
        egui::Label::new(
            RichText::new(subject_or_placeholder(&open.subject)).font(theme::semibold(26.0)),
        )
        .wrap(),
    );
    ui.add_space(16.0);
    let own_address = model.own_address(open.account_id);
    let address = (!message.from_name.is_empty()
        && !message.from_addr.eq_ignore_ascii_case(own_address))
    .then(|| format!("<{}>", message.from_addr));
    message_header(ui, message, own_address, address.as_deref());
    // Load images shares the "to" line, on the right, so the page starts
    // higher. The text view is a key press away, so it needs no button.
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !original.images
                && pill_button(ui, ButtonIcon::Picture, "Load images", "")
                    .on_hover_text(
                        "Loading them tells the sender you opened this. \
                         Settings can load them for every email.",
                    )
                    .clicked()
            {
                actions.push(Action::LoadImages);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("to {}", message.to))
                            .font(theme::regular(13.5))
                            .color(palette.dim),
                    )
                    .truncate(),
                );
            });
        });
    });
    ui.add_space(12.0);
    // Files sent with the message. Pictures it shows are on the page.
    if message.attachments.iter().any(|file| !file.shown_in_html) {
        ui.horizontal_wrapped(|ui| {
            for (number, file) in message.attachments.iter().enumerate() {
                if !file.shown_in_html && attachment_chip(ui, file) {
                    actions.push(Action::Download {
                        message: 0,
                        attachment: number,
                    });
                }
            }
        });
        ui.add_space(12.0);
    }

    let top = ui.min_rect().bottom();
    // WebKit would cover a toast, so the page ends above one.
    let bottom = if screen.toast.is_showing(model.notice()) {
        column.bottom() - toast::ROOM
    } else {
        column.bottom()
    };
    let page = egui::Rect::from_min_max(
        Pos2::new(column.left(), top),
        Pos2::new(column.right(), bottom.max(top)),
    );
    // White paper under WebKit, so nothing flashes while it loads.
    ui.painter()
        .rect_filled(page, CornerRadius::ZERO, Color32::WHITE);
    screen.original_rect = Some(page);
}

enum ButtonIcon {
    /// A framed picture with a hill and a sun.
    Picture,
    /// A page with a banner across its top, as designed mail has.
    Original,
}

/// A quiet rounded button: a small icon, its words and, when the action has
/// a shortcut, the shortcut in a key cap.
fn pill_button(ui: &mut egui::Ui, icon: ButtonIcon, text: &str, key: &str) -> egui::Response {
    let palette = theme::palette(ui.ctx());
    let words = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::regular(13.5), palette.text);
    let cap = (!key.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(key.to_owned(), theme::medium(12.0), palette.secondary)
    });
    let cap_width = cap
        .as_ref()
        .map_or(0.0, |cap| (cap.size().x + 10.0).max(20.0));
    let (padding, icon_width, gap) = (11.0, 14.0, 7.0);
    let width = padding
        + icon_width
        + gap
        + words.size().x
        + if cap.is_some() {
            gap + cap_width + 6.0
        } else {
            padding
        };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());
    let fill = if response.hovered() {
        palette.selected
    } else {
        palette.field
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), fill);

    let center = Pos2::new(rect.left() + padding + icon_width / 2.0, rect.center().y);
    let stroke = Stroke::new(1.3, palette.secondary);
    let line = |points: &[(f32, f32)]| {
        let points = points
            .iter()
            .map(|&(x, y)| center + Vec2::new(x, y))
            .collect();
        painter.add(egui::Shape::line(points, stroke));
    };
    let frame = |size: Vec2| {
        painter.rect_stroke(
            egui::Rect::from_center_size(center, size),
            CornerRadius::same(2),
            stroke,
            egui::StrokeKind::Middle,
        );
    };
    match icon {
        ButtonIcon::Picture => {
            frame(Vec2::new(14.0, 11.0));
            line(&[
                (-5.0, 3.0),
                (-1.5, -0.5),
                (1.0, 2.0),
                (2.5, 0.5),
                (5.0, 3.0),
            ]);
            painter.circle_filled(center + Vec2::new(2.8, -2.0), 1.3, palette.secondary);
        }
        ButtonIcon::Original => {
            frame(Vec2::new(14.0, 12.0));
            painter.rect_filled(
                egui::Rect::from_min_max(
                    center + Vec2::new(-7.0, -6.0),
                    center + Vec2::new(7.0, -2.0),
                ),
                CornerRadius::same(2),
                palette.secondary,
            );
            line(&[(-4.0, 1.0), (4.0, 1.0)]);
            line(&[(-4.0, 3.5), (1.0, 3.5)]);
        }
    }

    let words_left = rect.left() + padding + icon_width + gap;
    let words_width = words.size().x;
    painter.galley(
        Pos2::new(words_left, rect.center().y - words.size().y / 2.0),
        words,
        palette.text,
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

fn sender_label(message: &Message, own_address: &str) -> String {
    if message.from_addr.eq_ignore_ascii_case(own_address) {
        "me".to_owned()
    } else if message.from_name.is_empty() {
        message.from_addr.clone()
    } else {
        message.from_name.clone()
    }
}

/// A message's header line. Returns true when clicked.
fn message_header(
    ui: &mut egui::Ui,
    message: &Message,
    own_address: &str,
    detail: Option<&str>,
) -> bool {
    let palette = theme::palette(ui.ctx());
    let date = RichText::new(dates::human(message.internal_date))
        .font(theme::regular(13.5))
        .color(palette.dim);
    // The date goes on the right first; sender and detail share what's left,
    // so a long preview ends in "…" instead of running under the date.
    let date_width = ui
        .painter()
        .layout_no_wrap(date.text().to_owned(), theme::regular(13.5), palette.dim)
        .size()
        .x;
    let response = ui
        .horizontal(|ui| {
            let height = ui.spacing().interact_size.y;
            let room = |ui: &egui::Ui| (ui.available_width() - date_width - 18.0).max(0.0);
            let sender = egui::Label::new(
                RichText::new(sender_label(message, own_address)).font(theme::semibold(16.0)),
            )
            .truncate();
            let sender_room = room(ui);
            ui.allocate_ui(Vec2::new(sender_room, height), |ui| ui.add(sender));
            if let Some(detail) = detail {
                let detail = egui::Label::new(
                    RichText::new(detail)
                        .font(theme::regular(14.5))
                        .color(palette.secondary),
                )
                .truncate();
                let detail_room = room(ui);
                if detail_room > 30.0 {
                    ui.allocate_ui(Vec2::new(detail_room, height), |ui| ui.add(detail));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(date);
            });
        })
        .response
        .interact(Sense::click());
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

fn expanded_message(
    ui: &mut egui::Ui,
    index: usize,
    message: &Message,
    own_address: &str,
    actions: &mut Vec<Action>,
) -> bool {
    let palette = theme::palette(ui.ctx());
    let address = (!message.from_name.is_empty()
        && !message.from_addr.eq_ignore_ascii_case(own_address))
    .then(|| format!("<{}>", message.from_addr));
    let toggled = message_header(ui, message, own_address, address.as_deref());
    let mut recipients = format!("to {}", message.to);
    if !message.cc.is_empty() {
        recipients.push_str(&format!(", cc {}", message.cc));
    }
    ui.add(
        egui::Label::new(
            RichText::new(recipients)
                .font(theme::regular(13.5))
                .color(palette.dim),
        )
        .truncate(),
    );
    ui.add_space(12.0);

    // Link words point into the body text, so a snippet stands in without them.
    let (body_text, link_words) = if message.body_text.trim().is_empty() {
        (message.snippet.as_str(), &[][..])
    } else {
        (message.body_text.as_str(), message.body_links.as_slice())
    };
    if let Some(url) = body(ui, body_text, link_words) {
        actions.push(Action::OpenLink(url));
    }
    if message.body_pending {
        ui.add_space(8.0);
        ui.label(
            RichText::new("The full message is still downloading.")
                .font(theme::regular(13.5))
                .color(theme::palette(ui.ctx()).dim),
        );
    }

    if !message.attachments.is_empty() {
        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            for (number, attachment) in message.attachments.iter().enumerate() {
                if attachment_chip(ui, attachment) {
                    actions.push(Action::Download {
                        message: index,
                        attachment: number,
                    });
                }
            }
        });
    }
    toggled
}

/// A message body as selectable text, with web and email links in the
/// accent color. Returns the link clicked, if any. Mail is untrusted input,
/// so a link opens only on a click, and hovering shows its full address.
fn body(ui: &mut egui::Ui, text: &str, link_words: &[BodyLink]) -> Option<String> {
    let palette = theme::palette(ui.ctx());
    let shown = cached_shown_text(ui.ctx(), text, link_words);
    let (text, found) = (shown.0.as_str(), &shown.1);
    let plain = TextFormat {
        font_id: theme::regular(17.0),
        line_height: Some(26.0),
        color: palette.text,
        ..TextFormat::default()
    };
    let linked = TextFormat {
        color: palette.accent,
        ..plain.clone()
    };
    let mut job = LayoutJob::default();
    let mut written = 0;
    for (range, _) in found {
        job.append(&text[written..range.start], 0.0, plain.clone());
        // A link over several lines, such as a whole job card, reads better
        // in the text color; hovering still underlines it.
        let words = &text[range.clone()];
        let format = if words.contains('\n') {
            &plain
        } else {
            &linked
        };
        job.append(words, 0.0, format.clone());
        written = range.end;
    }
    job.append(&text[written..], 0.0, plain);
    job.wrap.max_width = ui.available_width();
    let galley = ui.painter().layout_job(job);
    let response = ui.add(egui::Label::new(galley.clone()).selectable(true));

    let pointer = response.hover_pos()?;
    let index = char_at(&galley, pointer - response.rect.min)?;
    // The galley counts characters, the links bytes.
    let (url, chars) = found
        .iter()
        .map(|(range, url)| {
            let start = text[..range.start].chars().count();
            let end = start + text[range.clone()].chars().count();
            (url, start..end)
        })
        .find(|(_, chars)| chars.contains(&index))?;

    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    for (left, right, baseline) in underline(&galley, chars) {
        ui.painter().hline(
            response.rect.left() + left..=response.rect.left() + right,
            response.rect.top() + baseline + 2.5,
            Stroke::new(1.0, palette.accent),
        );
    }
    let clicked = response.clicked();
    response.on_hover_text_at_pointer(url);
    clicked.then(|| url.clone())
}

/// What `shown_text` makes of a body, kept while the body keeps being drawn,
/// so a long message is not searched for links on every frame.
type Shown = std::sync::Arc<(String, Vec<(Range<usize>, String)>)>;

#[derive(Default)]
struct ShownText;

impl<'a> egui::cache::ComputerMut<(&'a str, &'a [BodyLink]), Shown> for ShownText {
    fn compute(&mut self, (text, link_words): (&'a str, &'a [BodyLink])) -> Shown {
        std::sync::Arc::new(shown_text(text, link_words))
    }
}

fn cached_shown_text(ctx: &egui::Context, text: &str, link_words: &[BodyLink]) -> Shown {
    ctx.memory_mut(|memory| {
        memory
            .caches
            .cache::<egui::cache::FrameCache<Shown, ShownText>>()
            .get((text, link_words))
            .clone()
    })
}

/// The body as the reader shows it, and each link's place in that text: the
/// words that link (from the HTML) and the addresses written in the text,
/// long ones shortened to their site.
fn shown_text(text: &str, link_words: &[BodyLink]) -> (String, Vec<(Range<usize>, String)>) {
    // Linked words come first; an address inside them stays part of them.
    let mut found: Vec<(Range<usize>, String, bool)> = link_words
        .iter()
        .filter(|link| {
            link.start < link.end
                && text.is_char_boundary(link.start)
                && text.is_char_boundary(link.end)
                && links::is_openable(&link.url)
        })
        .map(|link| (link.start..link.end, link.url.clone(), false))
        .collect();
    for address in links::find(text) {
        let overlaps = found
            .iter()
            .any(|(range, ..)| range.start < address.range.end && address.range.start < range.end);
        if !overlaps {
            found.push((address.range, address.url, true));
        }
    }
    found.sort_by_key(|(range, ..)| range.start);

    let mut shown = String::with_capacity(text.len());
    let mut placed = Vec::with_capacity(found.len());
    let mut written = 0;
    for (range, url, is_address) in found {
        // Overlapping words from a malformed message: keep the first.
        if range.start < written {
            continue;
        }
        shown.push_str(&text[written..range.start]);
        let start = shown.len();
        match is_address.then(|| links::shorten(&url)).flatten() {
            Some(short) => shown.push_str(&short),
            None => shown.push_str(&text[range.clone()]),
        }
        placed.push((start..shown.len(), url));
        written = range.end;
    }
    shown.push_str(&text[written..]);
    (shown, placed)
}

/// Each character's glyph in a galley, as it is reached: its index and its
/// box, relative to the galley, plus its row's baseline.
fn glyphs(galley: &egui::Galley) -> impl Iterator<Item = (usize, egui::Rect, f32)> + '_ {
    let mut index = 0;
    galley.rows.iter().flat_map(move |placed| {
        let row = &placed.row;
        let first = index;
        // A line break is a character without a glyph.
        index += row.glyphs.len() + usize::from(placed.ends_with_newline);
        row.glyphs.iter().enumerate().map(move |(offset, glyph)| {
            let min = Pos2::new(placed.pos.x + glyph.pos.x, placed.pos.y);
            let size = Vec2::new(glyph.advance_width, row.size.y);
            let baseline = placed.pos.y + glyph.pos.y;
            (
                first + offset,
                egui::Rect::from_min_size(min, size),
                baseline,
            )
        })
    })
}

/// The character under `pos`, relative to the galley.
fn char_at(galley: &egui::Galley, pos: Vec2) -> Option<usize> {
    glyphs(galley)
        .find(|(_, rect, _)| rect.contains(pos.to_pos2()))
        .map(|(index, _, _)| index)
}

/// Where to underline characters `chars`: one span per row, as left, right
/// and baseline.
fn underline(galley: &egui::Galley, chars: Range<usize>) -> Vec<(f32, f32, f32)> {
    let mut spans: Vec<(f32, f32, f32)> = Vec::new();
    for (_, rect, baseline) in glyphs(galley).filter(|(index, _, _)| chars.contains(index)) {
        match spans.last_mut() {
            Some(span) if span.2 == baseline => span.1 = rect.right(),
            _ => spans.push((rect.left(), rect.right(), baseline)),
        }
    }
    spans
}

/// An attachment's name and size. Returns true when clicked.
fn attachment_chip(ui: &mut egui::Ui, attachment: &Attachment) -> bool {
    let palette = theme::palette(ui.ctx());
    let padding = Vec2::new(12.0, 8.0);
    let gap = 8.0;
    let size =
        ui.painter()
            .layout_no_wrap(size(attachment.size), theme::regular(13.0), palette.dim);
    let name_room = (ui.available_width() - size.size().x - gap - padding.x * 2.0).min(420.0);
    let name = one_line(
        ui,
        &[(
            attachment.filename.clone(),
            theme::medium(14.0),
            palette.text,
        )],
        name_room,
    );
    let inner = Vec2::new(
        name.size().x + gap + size.size().x,
        name.size().y.max(size.size().y),
    );
    let (rect, response) = ui.allocate_exact_size(inner + padding * 2.0, Sense::click());
    let fill = if response.hovered() {
        palette.selected
    } else {
        palette.field
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), fill);
    let middle = rect.center().y;
    let name_width = name.size().x;
    painter.galley(
        Pos2::new(rect.left() + padding.x, middle - name.size().y / 2.0),
        name,
        palette.text,
    );
    painter.galley(
        Pos2::new(
            rect.left() + padding.x + name_width + gap,
            middle - size.size().y / 2.0,
        ),
        size,
        palette.dim,
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!(
            "Save {} to Downloads and open it",
            attachment.filename
        ))
        .clicked()
}

fn collapsed_message(ui: &mut egui::Ui, message: &Message, own_address: &str) -> bool {
    message_header(ui, message, own_address, Some(&message.snippet))
}

fn size(bytes: i64) -> String {
    if bytes < 1_000_000 {
        format!("{} KB", (bytes / 1_000).max(1))
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

// Status bar

fn status_bar(ui: &mut egui::Ui, model: &Model, screen: &mut Screen, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    ui.horizontal_centered(|ui| {
        let (text, color) = match &screen.status {
            SyncStatus::Demo => ("Demo mailbox".to_owned(), palette.dim),
            SyncStatus::Working(text) => (text.clone(), palette.secondary),
            SyncStatus::Failed(error) => {
                // Only counted here: it reads the database, and this runs
                // on every frame.
                let pending = model.pending_changes();
                let waiting = if pending > 0 {
                    format!(" · {pending} changes waiting")
                } else {
                    String::new()
                };
                (format!("Sync failed: {error}{waiting}"), palette.danger)
            }
            SyncStatus::Idle => match model.synced_at() {
                Some(at) => (format!("Synced {}", dates::short(at)), palette.dim),
                None => ("Not synced yet".to_owned(), palette.dim),
            },
        };
        ui.add(
            egui::Label::new(RichText::new(text).font(theme::regular(13.0)).color(color))
                .truncate(),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let keymap = &screen.keys.keymap;
            if status_link(ui, &keymap.hint(Bindable::Shortcuts), "Shortcuts") {
                actions.push(Action::ToggleHelp);
            }
            ui.add_space(14.0);
            if status_link(ui, &keymap.hint(Bindable::Settings), "Settings") {
                screen.settings = Some(Settings::default());
            }
        });
    });
}

/// "⌘,  Settings" at the right of the status bar. Returns true when clicked.
fn status_link(ui: &mut egui::Ui, key: &str, name: &str) -> bool {
    let palette = theme::palette(ui.ctx());
    let text = if key.is_empty() {
        name.to_owned()
    } else {
        format!("{key}  {name}")
    };
    let link = egui::Label::new(
        RichText::new(text)
            .font(theme::regular(13.0))
            .color(palette.dim),
    )
    .sense(Sense::click());
    ui.add(link)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

// Overlays

fn label_picker(ctx: &egui::Context, model: &Model, actions: &mut Vec<Action>) {
    let palette = theme::palette(ctx);
    let Some(picker) = &model.picker else { return };
    let thread_labels = model
        .selected_thread()
        .map(|thread| thread.label_ids.clone())
        .unwrap_or_default();
    let mut filter = picker.filter.clone();
    let response = egui::Modal::new(Id::new("label-picker"))
        .frame(overlay_frame(palette))
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(RichText::new("Label conversation").font(theme::semibold(18.0)));
            ui.add_space(10.0);
            let field = ui.add(
                TextEdit::singleline(&mut filter)
                    .hint_text("Filter labels")
                    .font(theme::regular(15.0))
                    .margin(Vec2::new(10.0, 8.0))
                    .desired_width(f32::INFINITY),
            );
            field.request_focus();
            if field.changed() {
                actions.push(Action::PickerFilter(filter.clone()));
            }
            ui.add_space(8.0);
            let labels = model.picker_labels();
            if labels.is_empty() {
                ui.label(RichText::new("No labels match.").color(palette.dim));
            }
            for (index, label) in labels.iter().enumerate() {
                let checked = thread_labels.contains(&label.id);
                let (rect, row) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
                if index == picker.highlighted || row.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), palette.selected);
                }
                ui.painter().text(
                    Pos2::new(rect.left() + 12.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    if checked { "✓" } else { "" },
                    theme::semibold(15.0),
                    palette.accent,
                );
                ui.painter().text(
                    Pos2::new(rect.left() + 36.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &label.name,
                    theme::regular(15.0),
                    palette.text,
                );
                if row.clicked() {
                    actions.push(Action::PickerToggle(Some(label.id.clone())));
                }
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("↑ ↓ to choose, Enter to apply, Esc to close")
                    .font(theme::regular(12.5))
                    .color(palette.dim),
            );
        });
    if response.should_close() {
        actions.push(Action::ClosePicker);
    }
}

/// Shows exactly who a message goes to before it is sent.
fn confirm_send(ctx: &egui::Context, summary: &SendSummary, actions: &mut Vec<Action>) {
    let palette = theme::palette(ctx);
    let response = egui::Modal::new(Id::new("confirm-send"))
        .frame(overlay_frame(palette))
        .show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(RichText::new("Send this message?").font(theme::semibold(18.0)));
            ui.add_space(12.0);
            egui::Grid::new("send-summary")
                .num_columns(2)
                .spacing(Vec2::new(18.0, 8.0))
                .show(ui, |ui| {
                    let subject = if summary.subject.trim().is_empty() {
                        "(no subject)"
                    } else {
                        summary.subject.as_str()
                    };
                    let rows = [
                        ("From", summary.from.as_deref().unwrap_or_default()),
                        ("To", summary.to.as_str()),
                        ("Cc", summary.cc.as_str()),
                        ("Subject", subject),
                    ];
                    for (label, value) in rows {
                        if value.trim().is_empty() {
                            continue;
                        }
                        ui.label(
                            RichText::new(label)
                                .font(theme::regular(14.5))
                                .color(palette.dim),
                        );
                        ui.add(
                            egui::Label::new(RichText::new(value).font(theme::regular(14.5)))
                                .wrap(),
                        );
                        ui.end_row();
                    }
                });
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new("Cancel   Esc").font(theme::regular(14.5)))
                    .clicked()
                {
                    actions.push(Action::CancelSend);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let send = egui::Button::new(
                        RichText::new("Send   ↩")
                            .font(theme::semibold(14.5))
                            .color(palette.background),
                    )
                    .fill(palette.accent);
                    if ui.add(send).clicked() {
                        actions.push(Action::ConfirmSend);
                    }
                });
            });
        });
    if response.should_close() {
        actions.push(Action::CancelSend);
    }
}

pub(super) fn overlay_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.overlay)
        .stroke(Stroke::new(1.0, palette.divider))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(22))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn the_shown_text_description_sits_on_shown_text() {
        let source = include_str!("view.rs");
        let doc_above = |item: &str| -> String {
            let lines: Vec<&str> = source.lines().collect();
            let at = lines.iter().position(|l| l.starts_with(item)).unwrap();
            lines[..at]
                .iter()
                .rev()
                .take_while(|l| l.starts_with("///"))
                .copied()
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(doc_above("fn shown_text(").contains("The body as the reader shows it"));
        assert!(!doc_above("type Shown =").contains("The body as the reader shows it"));
    }

    #[test]
    fn a_saved_photo_becomes_a_small_square_texture() {
        let folder = tempfile::tempdir().unwrap();
        let paths = Paths::under(folder.path());
        let ctx = egui::Context::default();
        assert!(load_photo(&ctx, &paths, "me@example.com").is_none());

        // Google sends JPEGs, often not square.
        let photo = image::RgbImage::from_pixel(300, 200, image::Rgb([200, 40, 40]));
        let path = paths.photo("me@example.com");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        photo
            .save_with_format(&path, image::ImageFormat::Jpeg)
            .unwrap();
        let texture = load_photo(&ctx, &paths, "Me@Example.com").expect("decodes");
        assert_eq!(texture.size(), [128, 128]);
    }

    #[test]
    fn link_words_open_their_address_and_long_addresses_show_their_site() {
        let tracking = format!("https://click.news.example.com/?q={}", "z".repeat(90));
        let text = format!("Track your order or see {tracking} and https://rail.example/b");
        let words = [BodyLink {
            start: 0,
            end: 16,
            url: "https://shop.example/track".into(),
        }];
        let (shown, links) = shown_text(&text, &words);
        assert_eq!(
            shown,
            "Track your order or see click.news.example.com/… and https://rail.example/b"
        );
        let placed: Vec<(&str, &str)> = links
            .iter()
            .map(|(range, url)| (&shown[range.clone()], url.as_str()))
            .collect();
        assert_eq!(
            placed,
            [
                ("Track your order", "https://shop.example/track"),
                ("click.news.example.com/…", tracking.as_str()),
                ("https://rail.example/b", "https://rail.example/b"),
            ]
        );
    }

    #[test]
    fn a_body_is_searched_for_links_once_while_it_stays_on_screen() {
        let ctx = egui::Context::default();
        let text = "See https://rail.example/b now";
        let first = cached_shown_text(&ctx, text, &[]);
        let again = cached_shown_text(&ctx, text, &[]);
        assert!(std::sync::Arc::ptr_eq(&first, &again));
        assert_eq!(first.1.len(), 1);
        let other = cached_shown_text(&ctx, "Something else", &[]);
        assert!(!std::sync::Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn characters_are_found_by_position_across_lines() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let galley = ui.painter().layout_no_wrap(
                "ab\ncd".into(),
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
            );
            let all: Vec<_> = glyphs(&galley).collect();
            // The line break takes index 2, so "c" is character 3.
            assert_eq!(all.iter().map(|g| g.0).collect::<Vec<_>>(), [0, 1, 3, 4]);
            let (_, rect, _) = all[2];
            assert_eq!(char_at(&galley, rect.center().to_vec2()), Some(3));
            assert_eq!(char_at(&galley, Vec2::new(-5.0, -5.0)), None);
        });
        output.textures_delta.clear();
    }

    #[test]
    fn link_words_that_dont_fit_the_text_are_ignored() {
        let words = [
            BodyLink {
                start: 2,
                end: 99,
                url: "https://shop.example".into(),
            },
            BodyLink {
                start: 0,
                end: 2,
                url: "javascript:alert(1)".into(),
            },
        ];
        let (shown, links) = shown_text("Hello", &words);
        assert_eq!(shown, "Hello");
        assert!(links.is_empty());
    }

    #[test]
    fn a_chosen_photo_wins_over_googles_and_loses_its_metadata() {
        let folder = tempfile::tempdir().unwrap();
        let paths = Paths::under(folder.path());
        let google = paths.photo("me@example.com");
        fs::create_dir_all(google.parent().unwrap()).unwrap();
        image::RgbImage::from_pixel(96, 96, image::Rgb([0, 0, 255]))
            .save_with_format(&google, image::ImageFormat::Jpeg)
            .unwrap();

        // A portrait photo, as from a phone: cropped to its middle square.
        let source = folder.path().join("selfie.jpg");
        image::RgbImage::from_pixel(300, 400, image::Rgb([220, 30, 30]))
            .save_with_format(&source, image::ImageFormat::Jpeg)
            .unwrap();
        accounts::choose_photo(&paths, "me@example.com", &source).unwrap();
        let chosen = image::open(paths.chosen_photo("me@example.com")).unwrap();
        assert_eq!((chosen.width(), chosen.height()), (256, 256));

        let shown = accounts::read_photo(&paths, "me@example.com")
            .unwrap()
            .to_rgb8();
        assert!(
            shown.get_pixel(128, 128)[0] > 150,
            "the chosen photo, not Google's"
        );
    }
}

/// The key and text presses in a frame's events, each flagged when it comes
/// from a held key. Text events carry no repeat flag of their own, so a text
/// press counts as a repeat when a key repeat arrives in the same frame (the
/// platform sends both for an autorepeating symbol such as `#`).
fn held_presses(events: &[egui::Event]) -> Vec<(Press, bool)> {
    let key_repeating = events.iter().any(|event| {
        matches!(
            event,
            egui::Event::Key {
                pressed: true,
                repeat: true,
                ..
            }
        )
    });
    events
        .iter()
        .filter_map(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                repeat,
                ..
            } => Some((Press::Key(*key, *modifiers), *repeat)),
            egui::Event::Text(text) => Some((Press::Text(text.clone()), key_repeating)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod held_tests {
    use super::*;

    fn key(repeat: bool) -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Num3,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: egui::Modifiers::SHIFT,
        }
    }

    #[test]
    fn text_from_a_repeating_key_is_a_repeat() {
        let events = [key(true), egui::Event::Text("#".into())];
        assert!(held_presses(&events).contains(&(Press::Text("#".into()), true)));
    }

    #[test]
    fn text_from_a_first_press_is_not_a_repeat() {
        let events = [key(false), egui::Event::Text("#".into())];
        assert!(held_presses(&events).contains(&(Press::Text("#".into()), false)));
    }
}
