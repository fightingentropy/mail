//! The command palette: type a few letters of what you want to do and
//! press Enter. It lists every action that has a shortcut, with that
//! shortcut beside it, plus the places shortcuts don't reach: Spam, Trash,
//! each label and each account.
//!
//! Choosing a command runs the same [`Command`] its shortcut would, so the
//! palette can't do anything the keyboard can't.

use egui::{Align2, CornerRadius, Id, Pos2, RichText, Sense, TextEdit, Vec2};
use mail_core::model::LabelKind;

use super::{
    keys::{Bindable, Command, Keymap, Scope},
    state::{Action, Model},
    theme,
    view::overlay_frame,
};

/// How many commands are listed at once.
const LISTED: usize = 9;

/// The open palette: what is typed, and which match is highlighted.
#[derive(Default)]
pub struct CommandPalette {
    pub query: String,
    pub highlighted: usize,
    /// Enter was pressed: run the highlighted command when next drawn.
    pub run: bool,
}

/// One thing the palette can do.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub label: String,
    /// Its shortcut, or "" when it has none.
    pub hint: String,
    pub run: Vec<Command>,
}

/// What became of the palette this frame.
pub enum Outcome {
    Open,
    Closed,
    Run(Vec<Command>),
}

/// Everything the palette offers, in the order shown before any typing.
pub fn entries(model: &Model, keymap: &Keymap) -> Vec<Entry> {
    // Moving and scrolling are quicker by their keys than by name.
    let skipped = [
        Bindable::Next,
        Bindable::Previous,
        Bindable::ScrollDown,
        Bindable::ScrollUp,
        Bindable::Commands,
    ];
    let mut entries: Vec<Entry> = Bindable::ALL
        .into_iter()
        .filter(|bindable| bindable.scope() == Scope::Mail && !skipped.contains(bindable))
        .map(|bindable| Entry {
            label: bindable.label().to_owned(),
            hint: keymap.menu_hint(bindable),
            run: vec![bindable.command()],
        })
        .collect();
    let show = |mailbox: &str| Command::Act(Action::Show(mailbox.to_owned()));
    for (id, name) in [("SPAM", "Spam"), ("TRASH", "Trash")] {
        entries.push(Entry {
            label: format!("Go to {name}"),
            hint: String::new(),
            run: vec![show(id)],
        });
    }
    // A label belongs to one account, so with all of them shown choosing
    // one shows its account, as in the sidebar.
    let all_shown = model.scope.is_none() && model.accounts.len() > 1;
    for account in model.shown_accounts() {
        for label in model.labels(account.id) {
            if label.kind != LabelKind::User {
                continue;
            }
            let (label_text, run) = if all_shown {
                (
                    format!("Go to {} ({})", label.name, account.email),
                    vec![
                        Command::Act(Action::SetScope(Some(account.id))),
                        show(&label.id),
                    ],
                )
            } else {
                (format!("Go to {}", label.name), vec![show(&label.id)])
            };
            entries.push(Entry {
                label: label_text,
                hint: String::new(),
                run,
            });
        }
    }
    if model.accounts.len() > 1 {
        entries.push(Entry {
            label: "Show all accounts".to_owned(),
            hint: String::new(),
            run: vec![Command::Act(Action::SetScope(None))],
        });
        for account in &model.accounts {
            entries.push(Entry {
                label: format!("Show only {}", account.email),
                hint: String::new(),
                run: vec![Command::Act(Action::SetScope(Some(account.id)))],
            });
        }
    }
    entries.push(Entry {
        label: "Add Gmail account…".to_owned(),
        hint: String::new(),
        run: vec![Command::Act(Action::AddAccount)],
    });
    entries
}

/// The entries that match what is typed, best first. With nothing typed,
/// all of them in their own order.
pub fn matching(entries: Vec<Entry>, query: &str) -> Vec<Entry> {
    let mut scored: Vec<(i32, Entry)> = entries
        .into_iter()
        .filter_map(|entry| score(query, &entry.label).map(|score| (score, entry)))
        .collect();
    // Stable, so equal matches keep the palette's own order.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, entry)| entry).collect()
}

/// How well `label` answers `query`, or `None` when it doesn't. Best is the
/// start of the label, then the start of a word in it, then the first
/// letters of its words ("gi" for "Go to Inbox"), then anywhere in a word,
/// then the letters in order with others between them.
pub fn score(query: &str, label: &str) -> Option<i32> {
    let query: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let label: Vec<char> = label.to_lowercase().chars().collect();
    // Spaces in the label are skipped too, so "markunread" finds a run.
    let letters: Vec<(usize, char)> = label
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, c)| !c.is_whitespace())
        .collect();
    let starts_word = |at: usize| at == 0 || !label[at - 1].is_alphanumeric();
    let run_at = |start: usize| {
        letters[start..]
            .iter()
            .map(|(_, c)| *c)
            .take(query.len())
            .eq(query.iter().copied())
    };
    let shorter_is_better = -(label.len().min(200) as i32);
    let run = (0..letters.len()).find(|&start| run_at(start) && starts_word(letters[start].0));
    if let Some(start) = run {
        let base = if start == 0 { 4_000 } else { 3_000 };
        return Some(base + shorter_is_better);
    }
    let initials: Vec<char> = letters
        .iter()
        .filter(|(at, _)| starts_word(*at))
        .map(|(_, c)| *c)
        .collect();
    if is_subsequence(&query, &initials) {
        return Some(2_500 + shorter_is_better);
    }
    if (0..letters.len()).any(run_at) {
        return Some(2_000 + shorter_is_better);
    }
    let all: Vec<char> = letters.iter().map(|(_, c)| *c).collect();
    is_subsequence(&query, &all).then_some(1_000 + shorter_is_better)
}

/// Whether `needle`'s letters all appear in `haystack`, in order.
fn is_subsequence(needle: &[char], haystack: &[char]) -> bool {
    let mut haystack = haystack.iter();
    needle.iter().all(|wanted| haystack.any(|c| c == wanted))
}

/// Draws the palette over the window. `matches` are the entries that match
/// what is typed, best first.
pub fn draw(ctx: &egui::Context, palette: &mut CommandPalette, matches: &[Entry]) -> Outcome {
    let colors = theme::palette(ctx);
    palette.highlighted = palette.highlighted.min(matches.len().saturating_sub(1));
    let mut chosen = std::mem::take(&mut palette.run).then_some(palette.highlighted);
    let response = egui::Modal::new(Id::new("palette"))
        .frame(overlay_frame(colors))
        .show(ctx, |ui| {
            ui.set_width(520.0);
            let field = ui.add(
                TextEdit::singleline(&mut palette.query)
                    .hint_text("Type a command")
                    .font(theme::regular(16.0))
                    .margin(Vec2::new(12.0, 9.0))
                    .desired_width(f32::INFINITY),
            );
            field.request_focus();
            if field.changed() {
                palette.highlighted = 0;
            }
            ui.add_space(8.0);
            if matches.is_empty() {
                ui.label(
                    RichText::new("No command matches.")
                        .font(theme::regular(14.5))
                        .color(colors.dim),
                );
            }
            // The list moves so the highlighted row is always in it.
            let first = (palette.highlighted + 1).saturating_sub(LISTED);
            for (index, entry) in matches.iter().enumerate().skip(first).take(LISTED) {
                let (rect, row) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
                if index == palette.highlighted {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), colors.selected);
                } else if row.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), colors.hover);
                }
                ui.painter().text(
                    Pos2::new(rect.left() + 12.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    &entry.label,
                    theme::regular(15.0),
                    colors.text,
                );
                ui.painter().text(
                    Pos2::new(rect.right() - 12.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    &entry.hint,
                    theme::medium(13.0),
                    colors.dim,
                );
                if row.clicked() {
                    chosen = Some(index);
                }
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("↑ ↓ to choose, Enter to run, Esc to close")
                    .font(theme::regular(12.5))
                    .color(colors.dim),
            );
        });
    if let Some(entry) = chosen.and_then(|index| matches.get(index)) {
        return Outcome::Run(entry.run.clone());
    }
    if response.should_close() {
        return Outcome::Closed;
    }
    Outcome::Open
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(query: &str, all: &[&str]) -> Vec<String> {
        let entries = all
            .iter()
            .map(|label| Entry {
                label: (*label).to_owned(),
                hint: String::new(),
                run: Vec::new(),
            })
            .collect();
        matching(entries, query)
            .into_iter()
            .map(|entry| entry.label)
            .collect()
    }

    const ALL: &[&str] = &[
        "Archive",
        "Move to Trash",
        "Mark as read or unread",
        "Star or unstar",
        "Search",
        "Go to Inbox",
        "Go to Starred",
        "Go to Trash",
        "Settings",
    ];

    #[test]
    fn nothing_typed_lists_everything_in_order() {
        assert_eq!(labels("", ALL), ALL);
        assert_eq!(labels("   ", ALL), ALL);
    }

    #[test]
    fn the_start_of_the_name_beats_the_start_of_a_later_word() {
        assert_eq!(labels("arch", ALL), ["Archive", "Search"]);
        assert_eq!(
            labels("st", ALL),
            ["Star or unstar", "Go to Starred", "Settings"]
        );
        assert_eq!(labels("trash", ALL), ["Go to Trash", "Move to Trash"]);
    }

    #[test]
    fn first_letters_of_the_words_find_a_command() {
        assert_eq!(labels("gi", ALL)[0], "Go to Inbox");
        assert_eq!(labels("gts", ALL)[0], "Go to Starred");
        assert_eq!(labels("mt", ALL)[0], "Move to Trash");
    }

    #[test]
    fn spaces_and_case_do_not_matter_and_letters_may_skip() {
        assert_eq!(labels("MARK UNREAD", ALL), ["Mark as read or unread"]);
        assert_eq!(labels("goinbox", ALL), ["Go to Inbox"]);
        assert!(labels("zzz", ALL).is_empty());
        assert_eq!(score("x", "Archive"), None);
    }
}
