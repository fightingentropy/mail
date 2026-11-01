//! Scripted key presses for demo screenshots, so the window's keyboard
//! handling can be checked without a person at the keyboard.
//!
//! Steps are separated by spaces: a key name (`j`, `esc`, `enter`, `down`,
//! `/`, `#`, `?`) or `text:` followed by characters to type, with `_` for a
//! space.

use std::collections::VecDeque;

use egui::{Event, Key, Modifiers};

pub fn parse(script: &str) -> VecDeque<Vec<Event>> {
    script.split_whitespace().map(step).collect()
}

fn step(token: &str) -> Vec<Event> {
    if let Some(text) = token.strip_prefix("text:") {
        return vec![Event::Text(text.replace('_', " "))];
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

fn press(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}
