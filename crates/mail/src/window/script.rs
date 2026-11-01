//! Scripted key presses for demo screenshots, so the window's keyboard
//! handling can be checked without a person at the keyboard.
//!
//! Steps are separated by spaces: a key name (`j`, `esc`, `enter`, `down`,
//! `/`, `#`, `?`), a chord written as in `shortcuts.json` (`cmd+k`,
//! `shift+cmd+n`, `cmd+backspace`), `text:` followed by characters to type
//! (`_` for a space), `click:x,y`, `rightclick:x,y` and `hover:x,y` in
//! points from the window's top left, or `wait` to let a frame pass.

use std::collections::VecDeque;

use egui::{Event, Key, Modifiers, PointerButton, Pos2};

use super::keys::{Chord, Shortcut};

pub fn parse(script: &str) -> VecDeque<Vec<Event>> {
    script.split_whitespace().map(step).collect()
}

fn step(token: &str) -> Vec<Event> {
    if let Some(text) = token.strip_prefix("text:") {
        return vec![Event::Text(text.replace('_', " "))];
    }
    if let Some(pos) = token.strip_prefix("click:").and_then(point) {
        return click(pos, PointerButton::Primary);
    }
    if let Some(pos) = token.strip_prefix("rightclick:").and_then(point) {
        return click(pos, PointerButton::Secondary);
    }
    if token == "wait" {
        return Vec::new();
    }
    if let Some(pos) = token.strip_prefix("hover:").and_then(point) {
        return vec![Event::PointerMoved(pos)];
    }
    // `cmd+enter`, `shift+cmd+n`: a key with modifiers held.
    if token.len() > 1 && token.contains('+') {
        let chord = Shortcut::parse(token).and_then(|shortcut| shortcut.chords().first().copied());
        return match chord {
            Some(Chord::Key(key, mods)) => vec![press(key, mods.into())],
            _ => Vec::new(),
        };
    }
    let named = match token {
        "esc" => Some(Key::Escape),
        "enter" => Some(Key::Enter),
        "down" => Some(Key::ArrowDown),
        "up" => Some(Key::ArrowUp),
        "space" => Some(Key::Space),
        _ => None,
    };
    if let Some(key) = named {
        return vec![press(key, Modifiers::NONE)];
    }
    match token {
        // Typed with Shift (or Option on a UK keyboard): text only.
        "#" => vec![Event::Text("#".into())],
        "?" => vec![press(Key::Slash, Modifiers::SHIFT), Event::Text("?".into())],
        _ => {
            let key = Key::from_name(&token.to_uppercase()).or_else(|| Key::from_name(token));
            let mut events: Vec<Event> = key
                .map(|key| press(key, Modifiers::NONE))
                .into_iter()
                .collect();
            events.push(Event::Text(token.to_owned()));
            events
        }
    }
}

fn point(text: &str) -> Option<Pos2> {
    let (x, y) = text.split_once(',')?;
    Some(Pos2::new(x.parse().ok()?, y.parse().ok()?))
}

fn click(pos: Pos2, button: PointerButton) -> Vec<Event> {
    let button = |pressed| Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    };
    vec![Event::PointerMoved(pos), button(true), button(false)]
}

fn press(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}
