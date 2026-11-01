//! Keyboard shortcuts. Gmail's letters where they exist, so muscle memory
//! carries over.

use egui::{Key, Modifiers};

use super::state::Action;

/// Where typing goes right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    List,
    Search,
    Picker,
    Help,
}

/// One key press or typed character, taken from egui's events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Press {
    Key(Key, Modifiers),
    Text(String),
}

/// What a press means beyond a model [`Action`]: things only the view can do.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Act(Action),
    FocusSearch,
    FocusList,
    ScrollReader(f32),
    SyncNow,
}

/// Remembers the first key of a two-key shortcut such as `g i`.
#[derive(Default)]
pub struct Keys {
    awaiting_go: bool,
}

/// Every shortcut, for the help overlay.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("j  k", "Next and previous conversation"),
    ("e", "Archive"),
    ("#  Delete", "Move to Trash"),
    ("u", "Mark as read or unread"),
    ("s", "Star or unstar"),
    ("l", "Add or remove a label"),
    ("z", "Undo"),
    ("/", "Search"),
    ("Space", "Scroll the conversation"),
    ("h", "Open the HTML version in your browser"),
    ("g i", "Go to Inbox"),
    ("g s", "Go to Starred"),
    ("g t", "Go to Sent"),
    ("g d", "Go to Drafts"),
    ("g a", "Go to All mail"),
    ("⌘R", "Sync now"),
    ("Esc", "Close or clear search"),
    ("?", "Show these shortcuts"),
];

impl Keys {
    pub fn command(&mut self, press: &Press, focus: Focus) -> Option<Command> {
        match focus {
            Focus::Search => search_key(press),
            Focus::Picker => picker_key(press),
            // The overlay closes itself on Esc or a click outside.
            Focus::Help => match press {
                Press::Text(text) if text == "?" => Some(Command::Act(Action::ToggleHelp)),
                _ => None,
            },
            Focus::List => self.list_key(press),
        }
    }

    fn list_key(&mut self, press: &Press) -> Option<Command> {
        if std::mem::take(&mut self.awaiting_go) {
            let mailbox = match press {
                Press::Key(Key::I, _) => "INBOX",
                Press::Key(Key::S, _) => "STARRED",
                Press::Key(Key::T, _) => "SENT",
                Press::Key(Key::D, _) => "DRAFT",
                Press::Key(Key::A, _) => "ALL",
                _ => return None,
            };
            return Some(Command::Act(Action::Show(mailbox.into())));
        }
        let act = |action| Some(Command::Act(action));
        match press {
            Press::Text(text) => match text.as_str() {
                "#" => act(Action::Trash),
                "?" => act(Action::ToggleHelp),
                _ => None,
            },
            Press::Key(Key::R, modifiers) if modifiers.command => Some(Command::SyncNow),
            // Leave other shortcuts with modifiers to macOS and egui.
            Press::Key(_, modifiers) if modifiers.command || modifiers.ctrl || modifiers.alt => {
                None
            }
            Press::Key(key, modifiers) => match key {
                Key::J | Key::ArrowDown => act(Action::Next),
                Key::K | Key::ArrowUp => act(Action::Previous),
                Key::E => act(Action::Archive),
                Key::Delete | Key::Backspace => act(Action::Trash),
                Key::U => act(Action::ToggleUnread),
                Key::S => act(Action::ToggleStar),
                Key::L => act(Action::OpenPicker),
                Key::Z => act(Action::Undo),
                Key::H => act(Action::OpenHtml),
                // Shift+/ is "?", handled as text above.
                Key::Slash if !modifiers.shift => Some(Command::FocusSearch),
                Key::Space if modifiers.shift => Some(Command::ScrollReader(-1.0)),
                Key::Space => Some(Command::ScrollReader(1.0)),
                Key::G => {
                    self.awaiting_go = true;
                    None
                }
                Key::Escape => act(Action::ClearSearch),
                _ => None,
            },
        }
    }
}

/// The search field keeps letters; only these leave it.
fn search_key(press: &Press) -> Option<Command> {
    match press {
        Press::Key(Key::Escape, _) => Some(Command::Act(Action::ClearSearch)),
        Press::Key(Key::Enter | Key::ArrowDown, _) => Some(Command::FocusList),
        _ => None,
    }
}

fn picker_key(press: &Press) -> Option<Command> {
    match press {
        Press::Key(Key::Escape, _) => Some(Command::Act(Action::ClosePicker)),
        Press::Key(Key::ArrowDown, _) => Some(Command::Act(Action::PickerMove(1))),
        Press::Key(Key::ArrowUp, _) => Some(Command::Act(Action::PickerMove(-1))),
        Press::Key(Key::Enter, _) => Some(Command::Act(Action::PickerToggle(None))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key) -> Press {
        Press::Key(key, Modifiers::NONE)
    }

    #[test]
    fn gmail_letters_triage_the_list() {
        let mut keys = Keys::default();
        assert_eq!(
            keys.command(&key(Key::E), Focus::List),
            Some(Command::Act(Action::Archive))
        );
        assert_eq!(
            keys.command(&Press::Text("#".into()), Focus::List),
            Some(Command::Act(Action::Trash))
        );
        assert_eq!(
            keys.command(&key(Key::J), Focus::List),
            Some(Command::Act(Action::Next))
        );
        assert_eq!(
            keys.command(&key(Key::Slash), Focus::List),
            Some(Command::FocusSearch)
        );
    }

    #[test]
    fn go_to_takes_two_keys() {
        let mut keys = Keys::default();
        assert_eq!(keys.command(&key(Key::G), Focus::List), None);
        assert_eq!(
            keys.command(&key(Key::S), Focus::List),
            Some(Command::Act(Action::Show("STARRED".into())))
        );
        // Without the g, s stars.
        assert_eq!(
            keys.command(&key(Key::S), Focus::List),
            Some(Command::Act(Action::ToggleStar))
        );
    }

    #[test]
    fn typing_in_search_does_not_triage() {
        let mut keys = Keys::default();
        assert_eq!(keys.command(&key(Key::E), Focus::Search), None);
        assert_eq!(
            keys.command(&key(Key::Enter), Focus::Search),
            Some(Command::FocusList)
        );
        assert_eq!(
            keys.command(&key(Key::Escape), Focus::Search),
            Some(Command::Act(Action::ClearSearch))
        );
    }

    #[test]
    fn h_opens_the_html_version_but_not_while_searching() {
        let mut keys = Keys::default();
        assert_eq!(
            keys.command(&key(Key::H), Focus::List),
            Some(Command::Act(Action::OpenHtml))
        );
        assert_eq!(keys.command(&key(Key::H), Focus::Search), None);
    }

    #[test]
    fn command_shortcuts_are_left_alone_except_sync() {
        let mut keys = Keys::default();
        let command = |key| Press::Key(key, Modifiers::COMMAND);
        assert_eq!(
            keys.command(&command(Key::R), Focus::List),
            Some(Command::SyncNow)
        );
        assert_eq!(keys.command(&command(Key::E), Focus::List), None);
    }
}
