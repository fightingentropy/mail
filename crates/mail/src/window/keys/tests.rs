use super::*;

fn key(key: Key) -> Press {
    Press::Key(key, Modifiers::NONE)
}

fn command(key: Key) -> Press {
    Press::Key(key, Modifiers::COMMAND)
}

fn shift_command(key: Key) -> Press {
    Press::Key(
        key,
        Modifiers {
            shift: true,
            ..Modifiers::COMMAND
        },
    )
}

fn keys() -> Keys {
    Keys::new(Keymap::defaults())
}

fn act(action: Action) -> Option<Command> {
    Some(Command::Act(action))
}

#[test]
fn gmail_letters_triage_the_list() {
    let mut keys = keys();
    assert_eq!(
        keys.command(&key(Key::E), Focus::List),
        act(Action::Archive)
    );
    assert_eq!(
        keys.command(&Press::Text("#".into()), Focus::List),
        act(Action::Trash)
    );
    assert_eq!(keys.command(&key(Key::J), Focus::List), act(Action::Next));
    assert_eq!(keys.command(&key(Key::R), Focus::List), act(Action::Reply));
}

#[test]
fn command_chords_work_like_apple_mail() {
    let mut keys = keys();
    let list = Focus::List;
    assert_eq!(
        keys.command(&command(Key::Backspace), list),
        act(Action::Trash)
    );
    assert_eq!(keys.command(&command(Key::R), list), act(Action::Reply));
    assert_eq!(
        keys.command(&shift_command(Key::R), list),
        act(Action::ReplyAll)
    );
    assert_eq!(
        keys.command(&command(Key::N), list),
        act(Action::NewMessage)
    );
    assert_eq!(keys.command(&command(Key::Z), list), act(Action::Undo));
    assert_eq!(
        keys.command(&shift_command(Key::N), list),
        Some(Command::SyncNow)
    );
    assert_eq!(
        keys.command(&command(Key::Comma), list),
        Some(Command::OpenSettings)
    );
    let control_command = Modifiers {
        ctrl: true,
        ..Modifiers::COMMAND
    };
    assert_eq!(
        keys.command(&Press::Key(Key::A, control_command), list),
        act(Action::Archive)
    );
    // Unbound chords are left to macOS.
    assert_eq!(keys.command(&command(Key::E), list), None);
}

#[test]
fn command_k_searches_and_slash_shows_the_shortcuts() {
    let mut keys = keys();
    assert_eq!(
        keys.command(&command(Key::K), Focus::List),
        Some(Command::FocusSearch)
    );
    // `/` arrives as a key and as text; only the text counts.
    assert_eq!(keys.command(&key(Key::Slash), Focus::List), None);
    assert_eq!(
        keys.command(&Press::Text("/".into()), Focus::List),
        act(Action::ToggleHelp)
    );
    assert_eq!(
        keys.command(&Press::Text("?".into()), Focus::List),
        act(Action::ToggleHelp)
    );
    // The overlay's own shortcut closes it; other keys do nothing there.
    assert_eq!(
        keys.command(&Press::Text("/".into()), Focus::Help),
        act(Action::ToggleHelp)
    );
    assert_eq!(keys.command(&key(Key::E), Focus::Help), None);
}

#[test]
fn go_to_takes_two_keys() {
    let mut keys = keys();
    assert_eq!(keys.command(&key(Key::G), Focus::List), None);
    // The letter's text event doesn't break the sequence.
    assert_eq!(keys.command(&Press::Text("g".into()), Focus::List), None);
    assert_eq!(
        keys.command(&key(Key::S), Focus::List),
        act(Action::Show("STARRED".into()))
    );
    // Without the g, s stars.
    assert_eq!(
        keys.command(&key(Key::S), Focus::List),
        act(Action::ToggleStar)
    );
    // A g followed by a key with no sequence still does that key's job.
    keys.command(&key(Key::G), Focus::List);
    assert_eq!(
        keys.command(&key(Key::E), Focus::List),
        act(Action::Archive)
    );
}

#[test]
fn typing_in_search_does_not_triage() {
    let mut keys = keys();
    assert_eq!(keys.command(&key(Key::E), Focus::Search), None);
    assert_eq!(
        keys.command(&key(Key::Enter), Focus::Search),
        Some(Command::FocusList)
    );
    assert_eq!(
        keys.command(&key(Key::Escape), Focus::Search),
        act(Action::ClearSearch)
    );
}

#[test]
fn compose_keeps_typing_and_saves_with_command_s() {
    let mut keys = keys();
    assert_eq!(keys.command(&key(Key::E), Focus::Compose), None);
    assert_eq!(keys.command(&key(Key::R), Focus::Compose), None);
    assert_eq!(
        keys.command(&command(Key::S), Focus::Compose),
        act(Action::SaveDraft)
    );
    // ⌘⌫ deletes text in the field; it must not trash or discard anything.
    assert_eq!(keys.command(&command(Key::Backspace), Focus::Compose), None);
    assert_eq!(
        keys.command(&shift_command(Key::D), Focus::Compose),
        act(Action::DiscardDraft)
    );
}

#[test]
fn sending_takes_command_enter_then_a_confirmation() {
    let mut keys = keys();
    assert_eq!(
        keys.command(&command(Key::Enter), Focus::Compose),
        act(Action::RequestSend)
    );
    // Plain Enter in the body is a new line, never a send.
    assert_eq!(keys.command(&key(Key::Enter), Focus::Compose), None);
    assert_eq!(
        keys.command(&key(Key::Enter), Focus::ConfirmSend),
        act(Action::ConfirmSend)
    );
    assert_eq!(
        keys.command(&key(Key::Escape), Focus::ConfirmSend),
        act(Action::CancelSend)
    );
    assert_eq!(keys.command(&key(Key::E), Focus::ConfirmSend), None);
}

#[test]
fn settings_take_every_key() {
    let mut keys = keys();
    assert_eq!(keys.command(&key(Key::E), Focus::Settings), None);
    assert_eq!(
        keys.command(&command(Key::Backspace), Focus::Settings),
        None
    );
}

#[test]
fn shortcuts_read_and_write_as_text() {
    for text in [
        "j",
        "#",
        "/",
        "cmd+backspace",
        "shift+cmd+r",
        "ctrl+cmd+a",
        "cmd+,",
        "shift+space",
        "g i",
        "cmd+plus",
        "F5",
    ] {
        let shortcut = Shortcut::parse(text).unwrap_or_else(|| panic!("{text} parses"));
        assert_eq!(shortcut.to_config(), text);
    }
    for broken in ["", "hyper+k", "a b c", "shift+#", "cmd+nonsense"] {
        assert_eq!(Shortcut::parse(broken), None, "{broken:?}");
    }
}

#[test]
fn a_recorded_plus_survives_saving_and_loading() {
    let shortcut = Shortcut::new(vec![Chord::Text('+')]);
    let text = shortcut.to_config();
    assert_eq!(Shortcut::parse(&text), Some(shortcut));
}

#[test]
fn shortcuts_display_like_macos_menus() {
    let display = |text: &str| Shortcut::parse(text).unwrap().display();
    assert_eq!(display("cmd+backspace"), "⌘⌫");
    assert_eq!(display("shift+cmd+r"), "⇧⌘R");
    assert_eq!(display("ctrl+cmd+a"), "⌃⌘A");
    assert_eq!(display("cmd+enter"), "⌘↩");
    assert_eq!(display("e"), "e");
    assert_eq!(display("g i"), "g i");
    assert_eq!(display("down"), "↓");
}

#[test]
fn assigning_a_taken_shortcut_moves_it() {
    let mut keymap = Keymap::defaults();
    let e = Shortcut::parse("e").unwrap();
    let losers = keymap.assign(Bindable::Trash, e.clone());
    assert_eq!(losers, vec![Bindable::Archive]);
    assert!(keymap.shortcuts(Bindable::Trash).contains(&e));
    assert!(!keymap.shortcuts(Bindable::Archive).contains(&e));

    // A single g would swallow every "g …" sequence, so they go.
    let losers = keymap.assign(Bindable::Sync, Shortcut::parse("g").unwrap());
    assert_eq!(losers.len(), 5);
    assert!(keymap.shortcuts(Bindable::GoInbox).is_empty());

    // Scopes don't compete: ⌘S in the mailbox leaves compose's ⌘S alone.
    let losers = keymap.assign(Bindable::ToggleStar, Shortcut::parse("cmd+s").unwrap());
    assert!(losers.is_empty());
    assert_eq!(keymap.hint(Bindable::SaveDraft), "⌘S");
}

#[test]
fn changes_survive_a_restart_and_reset_cleanly() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("shortcuts.json");
    assert_eq!(Keymap::load(&path), Keymap::defaults());

    let mut keymap = Keymap::defaults();
    keymap.remove(Bindable::Trash, 2);
    keymap.assign(Bindable::Archive, Shortcut::parse("y").unwrap());
    keymap.save(&path).unwrap();
    let saved = fs::read_to_string(&path).unwrap();
    // Only the two changed actions are written down.
    let written: BTreeMap<String, Vec<String>> = serde_json::from_str(&saved).unwrap();
    assert_eq!(written.len(), 2);
    assert_eq!(written["archive"], ["e", "ctrl+cmd+a", "y"]);
    assert_eq!(Keymap::load(&path), keymap);

    let mut keys = Keys::new(Keymap::load(&path));
    assert_eq!(
        keys.command(&key(Key::Y), Focus::List),
        act(Action::Archive)
    );
    assert_eq!(keys.command(&key(Key::Backspace), Focus::List), None);

    Keymap::defaults().save(&path).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap().trim(), "{}");
}

#[test]
fn a_broken_file_gives_the_defaults() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("shortcuts.json");
    fs::write(&path, "{ not json").unwrap();
    assert_eq!(Keymap::load(&path), Keymap::defaults());
    fs::write(&path, r#"{"archive": ["hyper+q", "y"], "fly": ["f"]}"#).unwrap();
    let keymap = Keymap::load(&path);
    assert_eq!(keymap.hint(Bindable::Archive), "y");
}

#[test]
fn a_first_key_waits_only_a_second_for_its_partner() {
    let now = Instant::now();
    let letter = |text: &str| Key::from_name(text).map(key).unwrap();
    let mut keys = keys();
    let list = Focus::List;
    assert_eq!(keys.command_at(&letter("G"), list, now), None);
    // Pressed together, as quickly as typed: a jump.
    assert_eq!(
        keys.command_at(&letter("S"), list, now + Duration::from_millis(300)),
        Some(Bindable::GoStarred.command())
    );
    // A stray g, then s minutes later is just star.
    assert_eq!(keys.command_at(&letter("G"), list, now), None);
    assert_eq!(
        keys.command_at(&letter("S"), list, now + Duration::from_secs(90)),
        act(Action::ToggleStar)
    );
}

#[test]
fn a_first_key_is_dropped_when_focus_moves() {
    let now = Instant::now();
    let mut keys = keys();
    assert_eq!(keys.command_at(&key(Key::G), Focus::List, now), None);
    assert_eq!(keys.command_at(&key(Key::A), Focus::Search, now), None);
    assert_eq!(
        keys.command_at(&key(Key::S), Focus::List, now),
        act(Action::ToggleStar)
    );
}

#[test]
fn command_u_asks_to_unsubscribe_and_enter_confirms() {
    let mut keys = keys();
    assert_eq!(
        keys.command(&command(Key::U), Focus::List),
        act(Action::Unsubscribe)
    );
    // ⇧⌘U is still read or unread.
    assert_eq!(
        keys.command(&shift_command(Key::U), Focus::List),
        act(Action::ToggleUnread)
    );
    let asking = Focus::ConfirmUnsubscribe;
    assert_eq!(
        keys.command(&key(Key::Enter), asking),
        act(Action::ConfirmUnsubscribe)
    );
    assert_eq!(
        keys.command(&key(Key::Escape), asking),
        act(Action::CancelUnsubscribe)
    );
    // Nothing else reaches the mailbox while it asks.
    assert_eq!(keys.command(&key(Key::E), asking), None);
    assert_eq!(keys.command(&Press::Text("#".into()), asking), None);
}

#[test]
fn command_y_previews_attachments() {
    let mut keys = keys();
    assert_eq!(
        keys.command(&command(Key::Y), Focus::List),
        act(Action::QuickLook)
    );
}

#[test]
fn the_palette_opens_with_its_shortcut_and_keeps_letters_for_itself() {
    let mut keys = keys();
    assert_eq!(
        keys.command(&shift_command(Key::P), Focus::List),
        Some(Command::OpenPalette)
    );
    let open = Focus::Palette;
    assert_eq!(keys.command(&key(Key::E), open), None, "typed, not archive");
    assert_eq!(keys.command(&Press::Text("#".into()), open), None);
    assert_eq!(
        keys.command(&key(Key::ArrowDown), open),
        Some(Command::PaletteMove(1))
    );
    assert_eq!(
        keys.command(&key(Key::ArrowUp), open),
        Some(Command::PaletteMove(-1))
    );
    assert_eq!(
        keys.command(&key(Key::Enter), open),
        Some(Command::PaletteRun)
    );
    assert_eq!(
        keys.command(&key(Key::Escape), open),
        Some(Command::ClosePalette)
    );
    // Its own shortcut closes it; other chords do nothing behind it.
    assert_eq!(
        keys.command(&shift_command(Key::P), open),
        Some(Command::ClosePalette)
    );
    assert_eq!(keys.command(&command(Key::Backspace), open), None);
    assert_eq!(keys.command(&command(Key::R), open), None);
}

#[test]
fn suggested_recipients_take_the_arrows_enter_and_tab() {
    let mut keys = keys();
    let listed = Focus::Suggestions;
    assert_eq!(
        keys.command(&key(Key::ArrowDown), listed),
        act(Action::MoveSuggestion(1))
    );
    assert_eq!(
        keys.command(&key(Key::ArrowUp), listed),
        act(Action::MoveSuggestion(-1))
    );
    for choose in [Key::Enter, Key::Tab] {
        assert_eq!(
            keys.command(&key(choose), listed),
            act(Action::AcceptSuggestion(None))
        );
    }
    assert_eq!(
        keys.command(&key(Key::Escape), listed),
        act(Action::DismissSuggestions),
        "Esc closes the list, not the message"
    );
    // Typing goes on, and the compose shortcuts still work.
    assert_eq!(keys.command(&key(Key::E), listed), None);
    assert_eq!(
        keys.command(&command(Key::Enter), listed),
        act(Action::RequestSend)
    );
    assert_eq!(
        keys.command(&command(Key::S), listed),
        act(Action::SaveDraft)
    );
}

#[test]
fn every_action_has_a_name_of_its_own_in_the_saved_file() {
    let mut ids: Vec<&str> = Bindable::ALL.iter().map(|bindable| bindable.id()).collect();
    ids.sort_unstable();
    let count = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), count);
    for bindable in Bindable::ALL {
        assert_eq!(Bindable::from_id(bindable.id()), Some(bindable));
    }
}

#[test]
fn no_two_default_shortcuts_clash_within_a_scope() {
    let keymap = Keymap::defaults();
    for first in Bindable::ALL {
        for second in Bindable::ALL {
            if first >= second || first.scope() != second.scope() {
                continue;
            }
            for a in keymap.shortcuts(first) {
                for b in keymap.shortcuts(second) {
                    assert!(
                        !a.clashes_with(b),
                        "{} and {} share {}",
                        first.label(),
                        second.label(),
                        a.display()
                    );
                }
            }
        }
    }
}
