//! The shortcuts overlay, and Settings: whether emails load their remote
//! images, and any shortcut, changed by pressing the keys you want.

use std::{path::Path, time::Duration};

use egui::{
    Align, Align2, Color32, CornerRadius, Event, Id, Key, Layout, RichText, ScrollArea, Sense,
    Stroke, StrokeKind, Vec2,
};

use super::{
    keys::{Bindable, Chord, Keymap, Press, Scope, Section, Shortcut},
    preferences::Preferences,
    state::Action,
    theme,
    view::overlay_frame,
};

/// How long a plain first key waits for a second, to make a sequence such
/// as `g i`.
const SEQUENCE_WAIT: f64 = 1.0;

/// The open Settings window.
#[derive(Default)]
pub struct Settings {
    recording: Option<Recording>,
    notice: Option<String>,
}

/// An action waiting for the keys you press next.
struct Recording {
    bindable: Bindable,
    /// A plain first key and when it came, in case a second one follows.
    first: Option<(Chord, f64)>,
}

impl Settings {
    /// Takes this frame's keys while recording, so they reach nothing else.
    /// Esc stops recording without changing anything.
    pub fn record(
        &mut self,
        ctx: &egui::Context,
        presses: &[Press],
        keymap: &mut Keymap,
        path: &Path,
    ) {
        let Some(recording) = &mut self.recording else {
            return;
        };
        ctx.input_mut(|input| {
            input
                .events
                .retain(|event| !matches!(event, Event::Key { .. } | Event::Text(_)));
        });
        let now = ctx.input(|input| input.time);
        for press in presses {
            if matches!(press, Press::Key(Key::Escape, _)) {
                self.recording = None;
                self.notice = None;
                return;
            }
            let Some(chord) = Chord::from_press(press) else {
                continue;
            };
            let bindable = recording.bindable;
            if let Some((first, _)) = recording.first {
                return self.commit(bindable, Shortcut::new(vec![first, chord]), keymap, path);
            }
            match (bindable.scope(), is_plain(chord)) {
                (Scope::Compose, true) => {
                    self.notice =
                        Some("Compose shortcuts need ⌘, ⌃ or ⌥, so letters still type.".into());
                }
                (Scope::Mail, true) => recording.first = Some((chord, now)),
                (_, false) => {
                    return self.commit(bindable, Shortcut::new(vec![chord]), keymap, path);
                }
            }
        }
        if let Some(Recording {
            bindable,
            first: Some((first, at)),
        }) = self.recording
        {
            let waited = now - at;
            if waited >= SEQUENCE_WAIT {
                self.commit(bindable, Shortcut::new(vec![first]), keymap, path);
            } else {
                ctx.request_repaint_after(Duration::from_secs_f64(SEQUENCE_WAIT - waited));
            }
        }
    }

    fn commit(&mut self, bindable: Bindable, shortcut: Shortcut, keymap: &mut Keymap, path: &Path) {
        self.recording = None;
        let losers = keymap.assign(bindable, shortcut.clone());
        let keys = shortcut.display();
        self.notice = match losers.as_slice() {
            [] => None,
            [loser] => Some(format!("{keys} was {}; it moved here.", loser.label())),
            _ => Some(format!(
                "{keys} replaced {} shortcuts that started with it.",
                losers.len()
            )),
        };
        self.save(keymap, path);
    }

    fn save(&mut self, keymap: &Keymap, path: &Path) {
        if let Err(error) = keymap.save(path) {
            self.notice = Some(format!("Couldn't save shortcuts: {error:#}"));
        }
    }
}

/// No ⌘, ⌃ or ⌥: a key that types, or could start a sequence.
fn is_plain(chord: Chord) -> bool {
    match chord {
        Chord::Text(_) => true,
        Chord::Key(_, mods) => !mods.commanding(),
    }
}

/// Draws Settings. Changes save as they are made. Returns false once it
/// should close.
pub fn settings_window(
    ctx: &egui::Context,
    settings: &mut Settings,
    keymap: &mut Keymap,
    path: &Path,
    preferences: &mut Preferences,
    preferences_path: &Path,
) -> bool {
    let palette = theme::palette(ctx);
    let mut open = true;
    let response = egui::Modal::new(Id::new("settings"))
        .frame(overlay_frame(palette))
        .show(ctx, |ui| {
            ui.set_width(680.0);
            ui.label(RichText::new("Settings").font(theme::semibold(20.0)));
            ui.add_space(2.0);
            ui.label(
                RichText::new("Changes save as you go.")
                    .font(theme::regular(13.5))
                    .color(palette.dim),
            );
            ui.add_space(14.0);
            group_heading(ui, "Reading");
            let key = keymap.hint(Bindable::ToggleOriginal);
            let switches = if key.is_empty() {
                String::new()
            } else {
                format!(" Press {key} to switch between the two as you read.")
            };
            let designed = setting_row(
                ui,
                "Show emails as they were designed",
                &format!("When this is off, they read as plain text.{switches}"),
                &mut preferences.show_original,
            );
            ui.add_space(10.0);
            let images = setting_row(
                ui,
                "Load remote images in emails",
                "Loading them tells the sender you opened the email. \
                 When this is off, each email has a Load images button.",
                &mut preferences.remote_images,
            );
            if (designed || images)
                && let Err(error) = preferences.save(preferences_path)
            {
                settings.notice = Some(format!("Couldn't save settings: {error:#}"));
            }
            ui.add_space(16.0);
            group_heading(ui, "Keyboard shortcuts");
            ui.label(
                RichText::new("Click + and press the keys you want.")
                    .font(theme::regular(13.5))
                    .color(palette.dim),
            );
            ui.add_space(10.0);
            let height = (ctx.content_rect().height() - 440.0).max(220.0);
            ScrollArea::vertical()
                .max_height(height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for section in Section::ALL {
                        section_heading(ui, section);
                        for bindable in Bindable::ALL {
                            if bindable.section() == section {
                                shortcut_row(ui, settings, keymap, path, bindable);
                            }
                        }
                        ui.add_space(6.0);
                    }
                });
            ui.add_space(12.0);
            if let Some(notice) = &settings.notice {
                ui.label(
                    RichText::new(notice)
                        .font(theme::regular(13.5))
                        .color(palette.secondary),
                );
                ui.add_space(6.0);
            }
            ui.horizontal(|ui| {
                let reset =
                    egui::Button::new(RichText::new("Reset shortcuts").font(theme::regular(14.5)));
                if ui.add_enabled(!keymap.is_default(), reset).clicked() {
                    *keymap = Keymap::defaults();
                    settings.recording = None;
                    settings.notice = Some("Shortcuts are back to their defaults.".into());
                    settings.save(keymap, path);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let done = egui::Button::new(
                        RichText::new("Done")
                            .font(theme::semibold(14.5))
                            .color(palette.background),
                    )
                    .fill(palette.accent);
                    if ui.add(done).clicked() {
                        open = false;
                    }
                });
            });
        });
    // A click outside stops recording first, then closes.
    if response.should_close() {
        if settings.recording.is_some() {
            settings.recording = None;
        } else {
            open = false;
        }
    }
    open
}

fn shortcut_row(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    keymap: &mut Keymap,
    path: &Path,
    bindable: Bindable,
) {
    let palette = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        let (label, _) = ui.allocate_exact_size(Vec2::new(240.0, 32.0), Sense::hover());
        ui.painter().text(
            label.left_center(),
            Align2::LEFT_CENTER,
            bindable.label(),
            theme::regular(14.5),
            palette.text,
        );
        let mut removed = None;
        for (index, shortcut) in keymap.shortcuts(bindable).iter().enumerate() {
            let id = Id::new(("remove-shortcut", bindable.id(), index));
            if removable_chip(ui, id, &shortcut.display()) {
                removed = Some(index);
            }
        }
        if let Some(index) = removed {
            keymap.remove(bindable, index);
            settings.notice = None;
            settings.save(keymap, path);
        }

        let recording = settings
            .recording
            .as_ref()
            .filter(|recording| recording.bindable == bindable);
        match recording {
            Some(recording) => {
                let text = match recording.first {
                    Some((first, _)) => format!("{} …", first.display()),
                    None => "Press keys…".to_owned(),
                };
                chip(ui, &text, palette.accent);
            }
            None => {
                let add = ui
                    .add(
                        egui::Button::new(RichText::new("+").font(theme::medium(15.0)))
                            .frame(false),
                    )
                    .on_hover_text("Add a shortcut")
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if add.clicked() {
                    settings.recording = Some(Recording {
                        bindable,
                        first: None,
                    });
                    settings.notice = None;
                }
            }
        }
    });
}

/// Every shortcut in two columns, drawn from the keymap in effect.
pub fn help_overlay(ctx: &egui::Context, keymap: &Keymap, actions: &mut Vec<Action>) {
    let palette = theme::palette(ctx);
    let response = egui::Modal::new(Id::new("help"))
        .frame(overlay_frame(palette))
        .show(ctx, |ui| {
            ui.set_width(860.0);
            ui.label(RichText::new("Keyboard shortcuts").font(theme::semibold(20.0)));
            ui.add_space(10.0);
            let height = (ctx.content_rect().height() - 200.0).max(220.0);
            ScrollArea::vertical()
                .max_height(height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.columns(2, |columns| {
                        shortcut_list(&mut columns[0], keymap, &[Section::Conversations]);
                        shortcut_list(
                            &mut columns[1],
                            keymap,
                            &[Section::Writing, Section::MovingAround],
                        );
                    });
                });
            ui.add_space(10.0);
            let settings = keymap.hint(Bindable::Settings);
            let change = if settings.is_empty() {
                "Change any shortcut in Settings.".to_owned()
            } else {
                format!("Change any shortcut in Settings ({settings}).")
            };
            ui.label(
                RichText::new(format!("Esc closes overlays and clears search. {change}"))
                    .font(theme::regular(13.5))
                    .color(palette.dim),
            );
        });
    if response.should_close() {
        actions.push(Action::CloseHelp);
    }
}

fn shortcut_list(ui: &mut egui::Ui, keymap: &Keymap, sections: &[Section]) {
    let palette = theme::palette(ui.ctx());
    for &section in sections {
        section_heading(ui, section);
        egui::Grid::new(("shortcuts", section.title()))
            .num_columns(2)
            .spacing(Vec2::new(18.0, 5.0))
            .min_col_width(170.0)
            .show(ui, |ui| {
                for bindable in Bindable::ALL {
                    if bindable.section() != section {
                        continue;
                    }
                    // Centred on the key caps beside it.
                    ui.horizontal(|ui| {
                        ui.set_min_height(CHIP_HEIGHT);
                        ui.label(RichText::new(bindable.label()).font(theme::regular(14.5)));
                    });
                    ui.horizontal(|ui| {
                        let shortcuts = keymap.shortcuts(bindable);
                        if shortcuts.is_empty() {
                            ui.label(RichText::new("none").color(palette.dim));
                        }
                        for shortcut in shortcuts {
                            chip(ui, &shortcut.display(), palette.text);
                        }
                    });
                    ui.end_row();
                }
            });
        ui.add_space(10.0);
    }
}

/// A choice that is on or off: its name and a switch, with what it means
/// below. Returns true when it changed.
fn setting_row(ui: &mut egui::Ui, title: &str, detail: &str, on: &mut bool) -> bool {
    let palette = theme::palette(ui.ctx());
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).font(theme::regular(14.5)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            changed = switch(ui, on, title).changed();
        });
    });
    ui.label(
        RichText::new(detail)
            .font(theme::regular(13.0))
            .color(palette.dim),
    );
    changed
}

/// A switch like macOS's: the accent colour with the knob on the right when
/// on, grey with it on the left when off.
fn switch(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let palette = theme::palette(ui.ctx());
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(36.0, 20.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, label)
    });
    let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
    let off = palette.dim.gamma_multiply(0.5);
    let track = off.lerp_to_gamma(palette.accent, how_on);
    let radius = rect.height() / 2.0;
    ui.painter().rect_filled(rect, radius, track);
    let knob = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
    ui.painter().circle_filled(
        egui::pos2(knob, rect.center().y),
        radius - 2.0,
        Color32::WHITE,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A part of Settings, such as "Reading".
fn group_heading(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).font(theme::semibold(15.5)));
    ui.add_space(4.0);
}

fn section_heading(ui: &mut egui::Ui, section: Section) {
    let palette = theme::palette(ui.ctx());
    ui.add_space(4.0);
    ui.label(
        RichText::new(section.title().to_uppercase())
            .font(theme::medium(11.5))
            .color(palette.dim),
    );
    ui.add_space(2.0);
}

const CHIP_HEIGHT: f32 = 24.0;

/// A key cap: one shortcut, such as `⌘⌫` or `g i`.
fn chip(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::medium(13.5), color);
    let width = (galley.size().x + 16.0).max(CHIP_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, CHIP_HEIGHT), Sense::hover());
    paint_cap(ui, rect);
    let text_at = rect.center() - galley.size() / 2.0;
    ui.painter().galley(text_at, galley, color);
    response
}

/// A key cap with a × to remove it. Returns true when the × is clicked.
fn removable_chip(ui: &mut egui::Ui, id: Id, text: &str) -> bool {
    let palette = theme::palette(ui.ctx());
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), theme::medium(13.5), palette.text);
    let width = galley.size().x + 36.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, CHIP_HEIGHT), Sense::hover());
    paint_cap(ui, rect);
    let text_at = egui::pos2(rect.left() + 9.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text_at, galley, palette.text);

    let cross = egui::Rect::from_min_max(egui::pos2(rect.right() - 22.0, rect.top()), rect.max);
    let remove = ui
        .interact(cross, id, Sense::click())
        .on_hover_text("Remove")
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let color = if remove.hovered() {
        palette.danger
    } else if response.hovered() {
        palette.secondary
    } else {
        palette.dim
    };
    ui.painter().text(
        cross.center(),
        Align2::CENTER_CENTER,
        "×",
        theme::regular(15.0),
        color,
    );
    remove.clicked()
}

fn paint_cap(ui: &egui::Ui, rect: egui::Rect) {
    let palette = theme::palette(ui.ctx());
    ui.painter().rect(
        rect,
        CornerRadius::same(6),
        palette.field,
        Stroke::new(1.0, palette.divider),
        StrokeKind::Inside,
    );
}
