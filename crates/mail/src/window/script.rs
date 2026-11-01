//! Scripted key presses for demo screenshots, so the window's keyboard
//! handling can be checked without a person at the keyboard.
//!
//! Steps are separated by spaces: a key name (`j`, `esc`, `enter`, `down`,
//! `/`, `#`, `?`, or with Command held as `cmd+enter`), `text:` followed by
//! characters to type (`_` for a space),
//! or `click:x,y` and `hover:x,y` in points from the window's top left.

use std::collections::VecDeque;

use egui::{Event, Key, Modifiers, PointerButton, Pos2};

pub fn parse(script: &str) -> VecDeque<Vec<Event>> {
    script.split_whitespace().map(step).collect()
}

fn step(token: &str) -> Vec<Event> {
    if let Some(text) = token.strip_prefix("text:") {
        return vec![Event::Text(text.replace('_', " "))];
    }
    if let Some(pos) = token.strip_prefix("click:").and_then(point) {
        return click(pos);
    }
    if let Some(pos) = token.strip_prefix("hover:").and_then(point) {
        return vec![Event::PointerMoved(pos)];
    }
    // `cmd+enter`, `cmd+s`: a key with Command held.
    if let Some(rest) = token.strip_prefix("cmd+") {
        let key = match rest {
            "enter" => Some(Key::Enter),
            other => Key::from_name(&other.to_uppercase()),
        };
        return key
            .map(|key| vec![press(key, Modifiers::COMMAND)])
            .unwrap_or_default();
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

fn click(pos: Pos2) -> Vec<Event> {
    let button = |pressed| Event::PointerButton {
        pos,
        button: PointerButton::Primary,
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
