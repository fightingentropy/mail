//! Drawing: sidebar, thread list, reader, status bar and the two overlays.
//! Clicks and keys become [`Action`]s applied to the [`Model`] after drawing.

use egui::{
    Align, Color32, CornerRadius, FontId, Id, Layout, Margin, Pos2, RichText, ScrollArea, Sense,
    Stroke, TextEdit, Vec2,
    text::{LayoutJob, TextFormat, TextWrapping},
};
use mail_core::model::{Message, ThreadSummary};

use super::{
    compose,
    keys::{Command, Focus, Keys, Press, SHORTCUTS},
    state::{Action, MAILBOXES, Model, View},
    theme,
};
use crate::dates;

const ROW_HEIGHT: f32 = 74.0;
const SIDEBAR_WIDTH: f32 = 212.0;
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
    search_text: String,
    focus_search: bool,
    search_focused: bool,
    list_offset: f32,
    list_height: f32,
    reader_offset: f32,
    reader_height: f32,
    reader_target: Option<f32>,
    pub status: SyncStatus,
    pub sync_requested: bool,
}

impl Screen {
    pub fn new(status: SyncStatus) -> Self {
        Self {
            keys: Keys::default(),
            search_text: String::new(),
            focus_search: false,
            search_focused: false,
            list_offset: 0.0,
            list_height: 0.0,
            reader_offset: 0.0,
            reader_height: 0.0,
            reader_target: None,
            status,
            sync_requested: false,
        }
    }
}

pub fn draw(ui: &mut egui::Ui, model: &mut Model, screen: &mut Screen) {
    let mut actions = Vec::new();
    handle_keys(ui.ctx(), model, screen, &mut actions);

    egui::Panel::bottom("status")
        .exact_size(32.0)
        .frame(
            egui::Frame::new()
                .fill(theme::SIDEBAR)
                .inner_margin(Margin::symmetric(18, 0)),
        )
        .show(ui, |ui| status_bar(ui, model, screen));
    egui::Panel::left("sidebar")
        .exact_size(SIDEBAR_WIDTH)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(theme::SIDEBAR)
                .inner_margin(Margin::symmetric(12, 20)),
        )
        .show(ui, |ui| sidebar(ui, model, &mut actions));
    egui::Panel::left("threads")
        .exact_size(LIST_WIDTH)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(theme::BACKGROUND)
                .stroke(Stroke::new(1.0, theme::DIVIDER)),
        )
        .show(ui, |ui| thread_list(ui, model, screen, &mut actions));
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::BACKGROUND))
        .show(ui, |ui| match &mut model.compose {
            Some(compose) => compose::draw(ui, compose),
            None => reader(ui, model, screen, &mut actions),
        });

    if model.picker.is_some() {
        label_picker(ui.ctx(), model, &mut actions);
    }
    if model.help {
        help(ui.ctx(), &mut actions);
    }

    for action in actions {
        let clears_search = action == Action::ClearSearch;
        if let Err(error) = model.apply(action) {
            model.notice = Some(format!("{error:#}"));
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
    let focus = if model.help {
        Focus::Help
    } else if model.picker.is_some() {
        Focus::Picker
    } else if model.compose.is_some() {
        Focus::Compose
    } else if screen.search_focused {
        Focus::Search
    } else {
        Focus::List
    };
    let presses: Vec<Press> = ctx.input(|input| {
        input
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => Some(Press::Key(*key, *modifiers)),
                egui::Event::Text(text) => Some(Press::Text(text.clone())),
                _ => None,
            })
            .collect()
    });
    for press in presses {
        match screen.keys.command(&press, focus) {
            Some(Command::Act(action)) => actions.push(action),
            Some(Command::FocusSearch) => screen.focus_search = true,
            Some(Command::FocusList) => {
                ctx.memory_mut(|memory| memory.surrender_focus(search_id()))
            }
            Some(Command::ScrollReader(direction)) => {
                let page = (screen.reader_height * 0.85).max(200.0);
                screen.reader_target = Some((screen.reader_offset + direction * page).max(0.0));
            }
            Some(Command::SyncNow) => screen.sync_requested = true,
            None => {}
        }
    }
}

fn search_id() -> Id {
    Id::new("search")
}

// Sidebar

fn sidebar(ui: &mut egui::Ui, model: &Model, actions: &mut Vec<Action>) {
    ui.label(
        RichText::new("Mail")
            .font(theme::semibold(21.0))
            .color(theme::TEXT),
    );
    ui.label(
        RichText::new(&model.account.email)
            .font(theme::regular(13.0))
            .color(theme::DIM),
    );
    ui.add_space(22.0);

    let current = match &model.view {
        View::Mailbox(id) => Some(id.as_str()),
        View::Search(_) => None,
    };
    for (id, name) in MAILBOXES {
        let count = (*id == "INBOX")
            .then(|| model.unread.get(*id).copied())
            .flatten();
        if nav_item(ui, name, count, current == Some(id)) {
            actions.push(Action::Show((*id).into()));
        }
    }

    let user_labels: Vec<_> = model
        .labels
        .iter()
        .filter(|label| label.kind == mail_core::model::LabelKind::User)
        .collect();
    if !user_labels.is_empty() {
        ui.add_space(20.0);
        ui.label(
            RichText::new("LABELS")
                .font(theme::medium(11.5))
                .color(theme::DIM),
        );
        ui.add_space(4.0);
        ScrollArea::vertical().id_salt("labels").show(ui, |ui| {
            for label in user_labels {
                let count = model.unread.get(&label.id).copied();
                if nav_item(ui, &label.name, count, current == Some(label.id.as_str())) {
                    actions.push(Action::Show(label.id.clone()));
                }
            }
        });
    }
}

/// One sidebar row. Returns true when clicked.
fn nav_item(ui: &mut egui::Ui, name: &str, count: Option<usize>, selected: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(8), theme::SELECTED);
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(8), theme::HOVER);
    }
    let font = if selected {
        theme::medium(15.0)
    } else {
        theme::regular(15.0)
    };
    let color = if selected {
        theme::TEXT
    } else {
        theme::SECONDARY
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
            theme::ACCENT,
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
            if !response.has_focus() && screen.search_text.is_empty() {
                key_hint(ui, response.rect, "/");
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
                        .color(theme::TEXT),
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
                            .color(theme::DIM),
                    );
                });
            });
        });
    let separator = ui.available_rect_before_wrap();
    ui.painter().hline(
        separator.x_range(),
        separator.top(),
        Stroke::new(1.0, theme::DIVIDER),
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
                    .color(theme::DIM),
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
    let output = area.show_rows(ui, ROW_HEIGHT, model.threads.len(), |ui, rows| {
        for index in rows {
            if thread_row(ui, &model.threads[index], index == model.selected) {
                actions.push(Action::Select(index));
            }
        }
    });
    screen.list_offset = output.state.offset.y;
    screen.list_height = output.inner_rect.height();
}

/// A small key cap at the right end of a field, like `/` for search.
fn key_hint(ui: &egui::Ui, field: egui::Rect, key: &str) {
    let center = Pos2::new(field.right() - 22.0, field.center().y);
    let cap = egui::Rect::from_center_size(center, Vec2::new(22.0, 22.0));
    ui.painter().rect_stroke(
        cap,
        CornerRadius::same(5),
        Stroke::new(1.0, theme::DIM),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        key,
        theme::medium(12.5),
        theme::SECONDARY,
    );
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
                    .labels
                    .iter()
                    .find(|label| &label.id == id)
                    .map(|label| label.name.clone())
            })
            .unwrap_or_else(|| id.clone()),
    }
}

/// One conversation in the list. Returns true when clicked.
fn thread_row(ui: &mut egui::Ui, thread: &ThreadSummary, selected: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::ZERO, theme::SELECTED);
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::ZERO, theme::HOVER);
    }
    if thread.unread {
        painter.circle_filled(
            Pos2::new(rect.left() + 11.0, rect.top() + 24.0),
            4.0,
            theme::ACCENT,
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
        theme::TEXT
    } else {
        theme::DIM
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
    let mut parts = vec![(thread.senders.join(", "), sender_font, theme::TEXT)];
    if thread.message_count > 1 {
        parts.push((
            format!("  {}", thread.message_count),
            theme::regular(14.0),
            theme::DIM,
        ));
    }
    let sender = one_line(ui, &parts, right - left - date_width - 12.0);
    painter.galley(Pos2::new(left, rect.top() + 13.0), sender, theme::TEXT);

    let subject_font = if thread.unread {
        theme::medium(14.5)
    } else {
        theme::regular(14.5)
    };
    let mut parts = vec![(
        subject_or_placeholder(&thread.subject),
        subject_font,
        theme::TEXT,
    )];
    if !thread.snippet.is_empty() {
        parts.push((
            format!("  {}", thread.snippet),
            theme::regular(14.5),
            theme::SECONDARY,
        ));
    }
    let star_room = if thread.starred { 22.0 } else { 0.0 };
    let preview = one_line(ui, &parts, right - left - star_room);
    painter.galley(Pos2::new(left, rect.top() + 40.0), preview, theme::TEXT);
    if thread.starred {
        painter.text(
            Pos2::new(right, rect.top() + 49.0),
            egui::Align2::RIGHT_CENTER,
            "★",
            theme::regular(14.0),
            theme::STAR,
        );
    }
    painter.hline(
        left..=rect.right(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::DIVIDER),
    );
    response.clicked()
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
    let Some(open) = &model.open else {
        ui.add_space(120.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("No conversation selected.")
                    .font(theme::regular(18.0))
                    .color(theme::DIM),
            );
        });
        return;
    };

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
                let labels: Vec<&str> = model
                    .labels
                    .iter()
                    .filter(|label| label.kind == mail_core::model::LabelKind::User)
                    .filter(|label| open.messages.iter().any(|m| m.has_label(&label.id)))
                    .map(|label| label.name.as_str())
                    .collect();
                if !labels.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(labels.join("  ·  "))
                            .font(theme::medium(13.0))
                            .color(theme::ACCENT),
                    );
                }
                ui.add_space(18.0);

                for (index, message) in open.messages.iter().enumerate() {
                    let expanded = open.expanded.get(index).copied().unwrap_or(true);
                    let toggled = if expanded {
                        expanded_message(ui, message, &model.account.email)
                    } else {
                        collapsed_message(ui, message, &model.account.email)
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
                            Stroke::new(1.0, theme::DIVIDER),
                        );
                        ui.add_space(14.0);
                    }
                }
            });
    });
    screen.reader_offset = output.state.offset.y;
    screen.reader_height = output.inner_rect.height();
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
    let response = ui
        .horizontal(|ui| {
            ui.label(RichText::new(sender_label(message, own_address)).font(theme::semibold(16.0)));
            if let Some(detail) = detail {
                ui.add(
                    egui::Label::new(
                        RichText::new(detail)
                            .font(theme::regular(14.5))
                            .color(theme::SECONDARY),
                    )
                    .truncate(),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(dates::human(message.internal_date))
                        .font(theme::regular(13.5))
                        .color(theme::DIM),
                );
            });
        })
        .response
        .interact(Sense::click());
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

fn expanded_message(ui: &mut egui::Ui, message: &Message, own_address: &str) -> bool {
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
                .color(theme::DIM),
        )
        .truncate(),
    );
    ui.add_space(12.0);

    let body = if message.body_text.trim().is_empty() {
        message.snippet.as_str()
    } else {
        message.body_text.as_str()
    };
    ui.add(
        egui::Label::new(
            RichText::new(body)
                .font(theme::regular(17.0))
                .line_height(Some(26.0)),
        )
        .wrap()
        .selectable(true),
    );

    if !message.attachments.is_empty() {
        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            for attachment in &message.attachments {
                egui::Frame::new()
                    .fill(theme::FIELD)
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(&attachment.filename).font(theme::medium(14.0)));
                        ui.label(
                            RichText::new(size(attachment.size))
                                .font(theme::regular(13.0))
                                .color(theme::DIM),
                        );
                    });
            }
        });
    }
    toggled
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

fn status_bar(ui: &mut egui::Ui, model: &Model, screen: &Screen) {
    ui.horizontal_centered(|ui| {
        let pending = model.pending_changes();
        let (text, color) = match &screen.status {
            SyncStatus::Demo => ("Demo mailbox".to_owned(), theme::DIM),
            SyncStatus::Working(text) => (text.clone(), theme::SECONDARY),
            SyncStatus::Failed(error) => {
                let waiting = if pending > 0 {
                    format!(" · {pending} changes waiting")
                } else {
                    String::new()
                };
                (format!("Sync failed: {error}{waiting}"), theme::DANGER)
            }
            SyncStatus::Idle => match model.account.synced_at {
                Some(at) => (format!("Synced {}", dates::short(at)), theme::DIM),
                None => ("Not synced yet".to_owned(), theme::DIM),
            },
        };
        ui.add(
            egui::Label::new(RichText::new(text).font(theme::regular(13.0)).color(color))
                .truncate(),
        );
        if let Some(notice) = &model.notice {
            ui.add_space(24.0);
            ui.label(
                RichText::new(notice)
                    .font(theme::regular(13.0))
                    .color(theme::TEXT),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new("?  Shortcuts")
                    .font(theme::regular(13.0))
                    .color(theme::DIM),
            );
        });
    });
}

// Overlays

fn label_picker(ctx: &egui::Context, model: &Model, actions: &mut Vec<Action>) {
    let Some(picker) = &model.picker else { return };
    let thread_labels = model
        .selected_thread()
        .map(|thread| thread.label_ids.clone())
        .unwrap_or_default();
    let mut filter = picker.filter.clone();
    let response = egui::Modal::new(Id::new("label-picker"))
        .frame(overlay_frame())
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
                ui.label(RichText::new("No labels match.").color(theme::DIM));
            }
            for (index, label) in labels.iter().enumerate() {
                let checked = thread_labels.contains(&label.id);
                let (rect, row) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
                if index == picker.highlighted || row.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), theme::SELECTED);
                }
                ui.painter().text(
                    Pos2::new(rect.left() + 12.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    if checked { "✓" } else { "" },
                    theme::semibold(15.0),
                    theme::ACCENT,
                );
                ui.painter().text(
                    Pos2::new(rect.left() + 36.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &label.name,
                    theme::regular(15.0),
                    theme::TEXT,
                );
                if row.clicked() {
                    actions.push(Action::PickerToggle(Some(label.id.clone())));
                }
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("↑ ↓ to choose, Enter to apply, Esc to close")
                    .font(theme::regular(12.5))
                    .color(theme::DIM),
            );
        });
    if response.should_close() {
        actions.push(Action::ClosePicker);
    }
}

fn help(ctx: &egui::Context, actions: &mut Vec<Action>) {
    let response = egui::Modal::new(Id::new("help"))
        .frame(overlay_frame())
        .show(ctx, |ui| {
            ui.set_width(440.0);
            ui.label(RichText::new("Keyboard shortcuts").font(theme::semibold(18.0)));
            ui.add_space(12.0);
            egui::Grid::new("shortcuts")
                .num_columns(2)
                .spacing(Vec2::new(28.0, 9.0))
                .show(ui, |ui| {
                    for (keys, what) in SHORTCUTS {
                        ui.label(
                            RichText::new(*keys)
                                .font(theme::medium(14.5))
                                .color(theme::ACCENT),
                        );
                        ui.label(RichText::new(*what).font(theme::regular(14.5)));
                        ui.end_row();
                    }
                });
        });
    if response.should_close() {
        actions.push(Action::CloseHelp);
    }
}

fn overlay_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgb(0x1e, 0x21, 0x24))
        .stroke(Stroke::new(1.0, theme::DIVIDER))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(22))
}
