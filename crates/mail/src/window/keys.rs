//! Keyboard shortcuts as data: Gmail's single letters and macOS ⌘ chords
//! side by side. Settings can change any of them; changes are saved in
//! `shortcuts.json` as the difference from the defaults.
//!
//! Esc, and the keys inside overlays (the label picker, the send
//! confirmation), stay fixed, so a rebinding can never trap you.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{Duration, Instant},
};

use egui::{Key, Modifiers};

use super::state::Action;

/// Where typing goes right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    List,
    Search,
    Picker,
    Help,
    Compose,
    /// The send confirmation is showing.
    ConfirmSend,
    Settings,
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
    OpenSettings,
}

/// Which keys a shortcut competes with: the mailbox's, or the compose
/// panel's, where plain keys type text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Mail,
    Compose,
}

/// Headings for the shortcuts overlay and Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Conversations,
    Writing,
    MovingAround,
}

impl Section {
    pub const ALL: [Section; 3] = [
        Section::Conversations,
        Section::Writing,
        Section::MovingAround,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Conversations => "Conversations",
            Section::Writing => "Writing",
            Section::MovingAround => "Moving around",
        }
    }
}

/// Everything a shortcut can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bindable {
    Next,
    Previous,
    Archive,
    Trash,
    ToggleUnread,
    ToggleStar,
    Label,
    Undo,
    OpenHtml,
    ToggleOriginal,
    ScrollDown,
    ScrollUp,
    Reply,
    ReplyAll,
    NewMessage,
    Send,
    SaveDraft,
    DiscardDraft,
    Search,
    GoInbox,
    GoStarred,
    GoSent,
    GoDrafts,
    GoAllMail,
    Sync,
    Shortcuts,
    Settings,
}

impl Bindable {
    /// In the order the overlay and Settings list them.
    pub const ALL: [Bindable; 27] = [
        Bindable::Next,
        Bindable::Previous,
        Bindable::Archive,
        Bindable::Trash,
        Bindable::ToggleUnread,
        Bindable::ToggleStar,
        Bindable::Label,
        Bindable::Undo,
        Bindable::OpenHtml,
        Bindable::ToggleOriginal,
        Bindable::ScrollDown,
        Bindable::ScrollUp,
        Bindable::Reply,
        Bindable::ReplyAll,
        Bindable::NewMessage,
        Bindable::Send,
        Bindable::SaveDraft,
        Bindable::DiscardDraft,
        Bindable::Search,
        Bindable::GoInbox,
        Bindable::GoStarred,
        Bindable::GoSent,
        Bindable::GoDrafts,
        Bindable::GoAllMail,
        Bindable::Sync,
        Bindable::Shortcuts,
        Bindable::Settings,
    ];

    /// The name in `shortcuts.json`. Never rename one: saved files use it.
    pub fn id(self) -> &'static str {
        match self {
            Bindable::Next => "next",
            Bindable::Previous => "previous",
            Bindable::Archive => "archive",
            Bindable::Trash => "trash",
            Bindable::ToggleUnread => "toggle_unread",
            Bindable::ToggleStar => "toggle_star",
            Bindable::Label => "label",
            Bindable::Undo => "undo",
            Bindable::OpenHtml => "open_html",
            Bindable::ToggleOriginal => "toggle_original",
            Bindable::ScrollDown => "scroll_down",
            Bindable::ScrollUp => "scroll_up",
            Bindable::Reply => "reply",
            Bindable::ReplyAll => "reply_all",
            Bindable::NewMessage => "new_message",
            Bindable::Send => "send",
            Bindable::SaveDraft => "save_draft",
            Bindable::DiscardDraft => "discard_draft",
            Bindable::Search => "search",
            Bindable::GoInbox => "go_inbox",
            Bindable::GoStarred => "go_starred",
            Bindable::GoSent => "go_sent",
            Bindable::GoDrafts => "go_drafts",
            Bindable::GoAllMail => "go_all_mail",
            Bindable::Sync => "sync",
            Bindable::Shortcuts => "shortcuts",
            Bindable::Settings => "settings",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Bindable::Next => "Next conversation",
            Bindable::Previous => "Previous conversation",
            Bindable::Archive => "Archive",
            Bindable::Trash => "Move to Trash",
            Bindable::ToggleUnread => "Mark as read or unread",
            Bindable::ToggleStar => "Star or unstar",
            Bindable::Label => "Add or remove a label",
            Bindable::Undo => "Undo",
            Bindable::OpenHtml => "Open the HTML version in your browser",
            Bindable::ToggleOriginal => "Show the original or the text",
            Bindable::ScrollDown => "Scroll down",
            Bindable::ScrollUp => "Scroll up",
            Bindable::Reply => "Reply",
            Bindable::ReplyAll => "Reply all",
            Bindable::NewMessage => "New message",
            Bindable::Send => "Send, after you confirm",
            Bindable::SaveDraft => "Save to Drafts",
            Bindable::DiscardDraft => "Discard the draft",
            Bindable::Search => "Search",
            Bindable::GoInbox => "Go to Inbox",
            Bindable::GoStarred => "Go to Starred",
            Bindable::GoSent => "Go to Sent",
            Bindable::GoDrafts => "Go to Drafts",
            Bindable::GoAllMail => "Go to All mail",
            Bindable::Sync => "Sync now",
            Bindable::Shortcuts => "Show shortcuts",
            Bindable::Settings => "Settings",
        }
    }

    pub fn scope(self) -> Scope {
        match self {
            Bindable::Send | Bindable::SaveDraft | Bindable::DiscardDraft => Scope::Compose,
            _ => Scope::Mail,
        }
    }

    pub fn section(self) -> Section {
        match self {
            Bindable::Reply
            | Bindable::ReplyAll
            | Bindable::NewMessage
            | Bindable::Send
            | Bindable::SaveDraft
            | Bindable::DiscardDraft => Section::Writing,
            Bindable::Search
            | Bindable::GoInbox
            | Bindable::GoStarred
            | Bindable::GoSent
            | Bindable::GoDrafts
            | Bindable::GoAllMail
            | Bindable::Sync
            | Bindable::Shortcuts
            | Bindable::Settings => Section::MovingAround,
            _ => Section::Conversations,
        }
    }

    pub fn command(self) -> Command {
        let act = Command::Act;
        let show = |mailbox: &str| Command::Act(Action::Show(mailbox.into()));
        match self {
            Bindable::Next => act(Action::Next),
            Bindable::Previous => act(Action::Previous),
            Bindable::Archive => act(Action::Archive),
            Bindable::Trash => act(Action::Trash),
            Bindable::ToggleUnread => act(Action::ToggleUnread),
            Bindable::ToggleStar => act(Action::ToggleStar),
            Bindable::Label => act(Action::OpenPicker),
            Bindable::Undo => act(Action::Undo),
            Bindable::OpenHtml => act(Action::OpenHtml),
            Bindable::ToggleOriginal => act(Action::ToggleOriginal),
            Bindable::ScrollDown => Command::ScrollReader(1.0),
            Bindable::ScrollUp => Command::ScrollReader(-1.0),
            Bindable::Reply => act(Action::Reply),
            Bindable::ReplyAll => act(Action::ReplyAll),
            Bindable::NewMessage => act(Action::NewMessage),
            Bindable::Send => act(Action::RequestSend),
            Bindable::SaveDraft => act(Action::SaveDraft),
            Bindable::DiscardDraft => act(Action::DiscardDraft),
            Bindable::Search => Command::FocusSearch,
            Bindable::GoInbox => show("INBOX"),
            Bindable::GoStarred => show("STARRED"),
            Bindable::GoSent => show("SENT"),
            Bindable::GoDrafts => show("DRAFT"),
            Bindable::GoAllMail => show("ALL"),
            Bindable::Sync => Command::SyncNow,
            Bindable::Shortcuts => act(Action::ToggleHelp),
            Bindable::Settings => Command::OpenSettings,
        }
    }

    /// Gmail's letter first where it has one, then the macOS chord.
    fn defaults(self) -> &'static [&'static str] {
        match self {
            Bindable::Next => &["j", "down"],
            Bindable::Previous => &["k", "up"],
            Bindable::Archive => &["e", "ctrl+cmd+a"],
            Bindable::Trash => &["#", "cmd+backspace", "backspace", "delete"],
            Bindable::ToggleUnread => &["u", "shift+cmd+u"],
            Bindable::ToggleStar => &["s", "shift+cmd+l"],
            Bindable::Label => &["l"],
            Bindable::Undo => &["z", "cmd+z"],
            Bindable::OpenHtml => &["h"],
            Bindable::ToggleOriginal => &["v"],
            Bindable::ScrollDown => &["space"],
            Bindable::ScrollUp => &["shift+space"],
            Bindable::Reply => &["r", "cmd+r"],
            Bindable::ReplyAll => &["a", "shift+cmd+r"],
            Bindable::NewMessage => &["c", "cmd+n"],
            Bindable::Send => &["cmd+enter"],
            Bindable::SaveDraft => &["cmd+s"],
            Bindable::DiscardDraft => &["shift+cmd+d"],
            Bindable::Search => &["cmd+k"],
            Bindable::GoInbox => &["g i"],
            Bindable::GoStarred => &["g s"],
            Bindable::GoSent => &["g t"],
            Bindable::GoDrafts => &["g d"],
            Bindable::GoAllMail => &["g a"],
            Bindable::Sync => &["shift+cmd+n"],
            Bindable::Shortcuts => &["/", "?"],
            Bindable::Settings => &["cmd+,"],
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|bindable| bindable.id() == id)
    }
}

/// The modifier keys a shortcut needs, as macOS names them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub control: bool,
    pub option: bool,
    pub shift: bool,
    pub command: bool,
}

impl Mods {
    /// ⌘, ⌃ or ⌥: keys that don't type anything in a text field.
    pub fn commanding(self) -> bool {
        self.command || self.control || self.option
    }
}

impl From<Modifiers> for Mods {
    fn from(modifiers: Modifiers) -> Self {
        Self {
            control: modifiers.ctrl,
            option: modifiers.alt,
            shift: modifiers.shift,
            command: modifiers.command,
        }
    }
}

impl From<Mods> for Modifiers {
    fn from(mods: Mods) -> Self {
        Self {
            alt: mods.option,
            ctrl: mods.control,
            shift: mods.shift,
            mac_cmd: mods.command,
            command: mods.command,
        }
    }
}

/// One step of a shortcut. A symbol such as `#` or `?` is matched as the
/// character typed, since keyboards reach it in different ways (Shift+3 in
/// the US, Option+3 in the UK); everything else is a key with modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chord {
    Key(Key, Mods),
    Text(char),
}

impl Chord {
    /// The chord a press is, if it can be one. A symbol arrives twice, as a
    /// key and as text; only the text counts, unless ⌘, ⌃ or ⌥ is held.
    pub fn from_press(press: &Press) -> Option<Self> {
        match press {
            Press::Key(key, modifiers) => {
                let mods = Mods::from(*modifiers);
                let typed_symbol = symbol(*key).is_some() && !mods.commanding();
                (!typed_symbol).then_some(Chord::Key(*key, mods))
            }
            Press::Text(text) => {
                let mut characters = text.chars();
                let character = characters.next()?;
                let is_symbol = characters.next().is_none()
                    && !character.is_alphanumeric()
                    && !character.is_whitespace()
                    && !character.is_control();
                is_symbol.then_some(Chord::Text(character))
            }
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let mut parts: Vec<&str> = text.split('+').collect();
        let name = parts.pop()?;
        let mut mods = Mods::default();
        for part in parts {
            match part {
                "ctrl" => mods.control = true,
                "alt" | "opt" => mods.option = true,
                "shift" => mods.shift = true,
                "cmd" => mods.command = true,
                _ => return None,
            }
        }
        let mut characters = name.chars();
        if let (Some(character), None) = (characters.next(), characters.next())
            && !character.is_alphanumeric()
            && !mods.commanding()
        {
            // A typed symbol; Shift is part of how it's typed.
            return (!mods.shift).then_some(Chord::Text(character));
        }
        let key = match name {
            "enter" => Key::Enter,
            "backspace" => Key::Backspace,
            "delete" => Key::Delete,
            "space" => Key::Space,
            "tab" => Key::Tab,
            "escape" => Key::Escape,
            "up" => Key::ArrowUp,
            "down" => Key::ArrowDown,
            "left" => Key::ArrowLeft,
            "right" => Key::ArrowRight,
            // "cmd++" wouldn't split cleanly. Bare "plus" is the typed `+`.
            "plus" if mods == Mods::default() => return Some(Chord::Text('+')),
            "plus" => Key::Plus,
            other => Key::from_name(other)?,
        };
        Some(Chord::Key(key, mods))
    }

    fn to_config(self) -> String {
        let (key, mods) = match self {
            // A bare `+` would not split back into parts.
            Chord::Text('+') => return "plus".to_owned(),
            Chord::Text(character) => return character.to_string(),
            Chord::Key(key, mods) => (key, mods),
        };
        let mut text = String::new();
        for (held, name) in [
            (mods.control, "ctrl+"),
            (mods.option, "alt+"),
            (mods.shift, "shift+"),
            (mods.command, "cmd+"),
        ] {
            if held {
                text.push_str(name);
            }
        }
        let name = match key {
            Key::Enter => "enter",
            Key::Backspace => "backspace",
            Key::Delete => "delete",
            Key::Space => "space",
            Key::Tab => "tab",
            Key::Escape => "escape",
            Key::ArrowUp => "up",
            Key::ArrowDown => "down",
            Key::ArrowLeft => "left",
            Key::ArrowRight => "right",
            Key::Plus => "plus",
            other => {
                text.push_str(&key_text(other));
                return text;
            }
        };
        text.push_str(name);
        text
    }

    /// As macOS menus write it: ⌃⌥⇧⌘, then the key.
    pub fn display(self) -> String {
        let (key, mods) = match self {
            Chord::Text(character) => return character.to_string(),
            Chord::Key(key, mods) => (key, mods),
        };
        let mut text = String::new();
        for (held, symbol) in [
            (mods.control, "⌃"),
            (mods.option, "⌥"),
            (mods.shift, "⇧"),
            (mods.command, "⌘"),
        ] {
            if held {
                text.push_str(symbol);
            }
        }
        let name = match key {
            Key::Enter => "↩",
            Key::Backspace => "⌫",
            Key::Delete => "⌦",
            Key::Space => "Space",
            Key::Tab => "⇥",
            Key::Escape => "Esc",
            Key::ArrowUp => "↑",
            Key::ArrowDown => "↓",
            Key::ArrowLeft => "←",
            Key::ArrowRight => "→",
            other => {
                // A bare letter as you type it; with modifiers, as menus show it.
                let name = key_text(other);
                if mods == Mods::default() {
                    text.push_str(&name);
                } else {
                    text.push_str(&name.to_uppercase());
                }
                return text;
            }
        };
        text.push_str(name);
        text
    }
}

/// One shortcut: a chord, or two in a row such as `g i`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut(Vec<Chord>);

impl Shortcut {
    pub fn parse(text: &str) -> Option<Self> {
        let chords: Vec<Chord> = text
            .split_whitespace()
            .map(Chord::parse)
            .collect::<Option<_>>()?;
        (1..=2).contains(&chords.len()).then_some(Shortcut(chords))
    }

    pub fn new(chords: Vec<Chord>) -> Self {
        Shortcut(chords)
    }

    pub fn chords(&self) -> &[Chord] {
        &self.0
    }

    pub fn to_config(&self) -> String {
        let chords: Vec<String> = self.0.iter().map(|chord| chord.to_config()).collect();
        chords.join(" ")
    }

    pub fn display(&self) -> String {
        let chords: Vec<String> = self.0.iter().map(|chord| chord.display()).collect();
        chords.join(" ")
    }

    /// Two shortcuts can't share a scope when one would always fire first:
    /// the same keys, or one chord that starts the other's sequence.
    fn clashes_with(&self, other: &Shortcut) -> bool {
        self == other || (self.0[0] == other.0[0] && (self.0.len() == 1 || other.0.len() == 1))
    }
}

enum Lookup {
    Exact(Bindable),
    /// The first chord of a two-chord shortcut.
    Prefix,
    None,
}

/// The shortcuts in effect: the defaults, with Settings' changes on top.
#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    bindings: BTreeMap<Bindable, Vec<Shortcut>>,
}

impl Keymap {
    pub fn defaults() -> Self {
        let bindings = Bindable::ALL
            .into_iter()
            .map(|bindable| (bindable, default_shortcuts(bindable)))
            .collect();
        Self { bindings }
    }

    /// The defaults with a saved file's changes. A missing or unreadable file
    /// gives the defaults; unknown actions and shortcuts in it are skipped.
    pub fn load(path: &Path) -> Self {
        let mut keymap = Self::defaults();
        let saved: BTreeMap<String, Vec<String>> = fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        for (id, shortcuts) in saved {
            if let Some(bindable) = Bindable::from_id(&id) {
                let parsed = shortcuts.iter().filter_map(|text| Shortcut::parse(text));
                keymap.bindings.insert(bindable, parsed.collect());
            }
        }
        keymap
    }

    /// Saves only what differs from the defaults, so new defaults in later
    /// versions still reach actions you never changed.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let changed: BTreeMap<&str, Vec<String>> = self
            .bindings
            .iter()
            .filter(|(bindable, shortcuts)| **shortcuts != default_shortcuts(**bindable))
            .map(|(bindable, shortcuts)| {
                let texts = shortcuts.iter().map(Shortcut::to_config).collect();
                (bindable.id(), texts)
            })
            .collect();
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)?;
        }
        fs::write(path, serde_json::to_string_pretty(&changed)? + "\n")?;
        Ok(())
    }

    pub fn shortcuts(&self, bindable: Bindable) -> &[Shortcut] {
        self.bindings.get(&bindable).map_or(&[], Vec::as_slice)
    }

    /// The first shortcut as menus and hints show it, or "" if it has none.
    pub fn hint(&self, bindable: Bindable) -> String {
        self.shortcuts(bindable)
            .first()
            .map(Shortcut::display)
            .unwrap_or_default()
    }

    /// The shortcut a menu shows: the first with ⌘, ⌃ or ⌥ as macOS menus
    /// do, else the first of any kind.
    pub fn menu_hint(&self, bindable: Bindable) -> String {
        let shortcuts = self.shortcuts(bindable);
        let chord = |shortcut: &&Shortcut| matches!(shortcut.0.as_slice(), [Chord::Key(_, mods)] if mods.commanding());
        shortcuts
            .iter()
            .find(chord)
            .or(shortcuts.first())
            .map(Shortcut::display)
            .unwrap_or_default()
    }

    pub fn is_default(&self) -> bool {
        *self == Self::defaults()
    }

    /// Adds `shortcut` to `bindable`, taking it from any other action in the
    /// same scope it clashes with. Returns the actions that lost it.
    pub fn assign(&mut self, bindable: Bindable, shortcut: Shortcut) -> Vec<Bindable> {
        if self.shortcuts(bindable).contains(&shortcut) {
            return Vec::new();
        }
        let mut losers = Vec::new();
        for (&other, shortcuts) in &mut self.bindings {
            if other.scope() != bindable.scope() {
                continue;
            }
            let before = shortcuts.len();
            shortcuts.retain(|existing| !existing.clashes_with(&shortcut));
            if shortcuts.len() < before && other != bindable {
                losers.push(other);
            }
        }
        self.bindings.entry(bindable).or_default().push(shortcut);
        losers
    }

    pub fn remove(&mut self, bindable: Bindable, index: usize) {
        if let Some(shortcuts) = self.bindings.get_mut(&bindable)
            && index < shortcuts.len()
        {
            shortcuts.remove(index);
        }
    }

    fn lookup(&self, scope: Scope, steps: &[Chord]) -> Lookup {
        let mut prefix = false;
        for (&bindable, shortcuts) in &self.bindings {
            if bindable.scope() != scope {
                continue;
            }
            for shortcut in shortcuts {
                if shortcut.0 == steps {
                    return Lookup::Exact(bindable);
                }
                prefix |= shortcut.0.len() > steps.len() && shortcut.0.starts_with(steps);
            }
        }
        if prefix { Lookup::Prefix } else { Lookup::None }
    }
}

fn default_shortcuts(bindable: Bindable) -> Vec<Shortcut> {
    bindable
        .defaults()
        .iter()
        .map(|text| Shortcut::parse(text).expect("default shortcuts parse"))
        .collect()
}

/// How long the first chord of a two-chord shortcut waits for its second.
const SEQUENCE_WAIT: Duration = Duration::from_secs(1);

/// Reads presses against the keymap, remembering the first chord of a
/// two-chord shortcut.
pub struct Keys {
    pub keymap: Keymap,
    pending: Option<(Chord, Instant)>,
    focus: Option<Focus>,
}

impl Keys {
    pub fn new(keymap: Keymap) -> Self {
        Self {
            keymap,
            pending: None,
            focus: None,
        }
    }

    pub fn command(&mut self, press: &Press, focus: Focus) -> Option<Command> {
        self.command_at(press, focus, Instant::now())
    }

    fn command_at(&mut self, press: &Press, focus: Focus, now: Instant) -> Option<Command> {
        // A first key left over from before the focus moved, or from long
        // ago, is not the start of this press's shortcut.
        if self.focus.replace(focus) != Some(focus) {
            self.pending = None;
        }
        if let Some((_, at)) = self.pending
            && now.saturating_duration_since(at) >= SEQUENCE_WAIT
        {
            self.pending = None;
        }
        match focus {
            Focus::Search => search_key(press),
            Focus::Picker => picker_key(press),
            Focus::ConfirmSend => match press {
                Press::Key(Key::Enter, _) => Some(Command::Act(Action::ConfirmSend)),
                Press::Key(Key::Escape, _) => Some(Command::Act(Action::CancelSend)),
                _ => None,
            },
            // The overlay closes itself on Esc or a click outside, and on
            // its own shortcut.
            Focus::Help => match self.mapped(press, Scope::Mail, now) {
                Some(Bindable::Shortcuts) => Some(Command::Act(Action::ToggleHelp)),
                _ => None,
            },
            // Settings reads keys itself while it records a shortcut.
            Focus::Settings => None,
            Focus::Compose => match press {
                Press::Key(Key::Escape, _) => Some(Command::Act(Action::CloseCompose)),
                _ => self
                    .mapped(press, Scope::Compose, now)
                    .map(Bindable::command),
            },
            Focus::List => match press {
                Press::Key(Key::Escape, _) => Some(Command::Act(Action::ClearSearch)),
                _ => self.mapped(press, Scope::Mail, now).map(Bindable::command),
            },
        }
    }

    fn mapped(&mut self, press: &Press, scope: Scope, now: Instant) -> Option<Bindable> {
        let chord = Chord::from_press(press)?;
        let first = self.pending.take();
        if let Some((first, _)) = first {
            match self.keymap.lookup(scope, &[first, chord]) {
                Lookup::Exact(bindable) => return Some(bindable),
                // A sequence that went nowhere: the second key may still
                // mean something on its own.
                Lookup::Prefix | Lookup::None => {}
            }
        }
        match self.keymap.lookup(scope, &[chord]) {
            Lookup::Exact(bindable) => Some(bindable),
            Lookup::Prefix => {
                self.pending = Some((chord, now));
                None
            }
            Lookup::None => None,
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

/// The character a punctuation key types, such as `/` for [`Key::Slash`].
fn symbol(key: Key) -> Option<char> {
    let character = match key {
        Key::Colon => ':',
        Key::Comma => ',',
        Key::Minus => '-',
        Key::Period => '.',
        Key::Plus => '+',
        Key::Equals => '=',
        Key::Semicolon => ';',
        Key::Backslash => '\\',
        Key::Slash => '/',
        Key::Pipe => '|',
        Key::Questionmark => '?',
        Key::Exclamationmark => '!',
        Key::OpenBracket => '[',
        Key::CloseBracket => ']',
        Key::OpenCurlyBracket => '{',
        Key::CloseCurlyBracket => '}',
        Key::Backtick => '`',
        Key::Quote => '\'',
        _ => return None,
    };
    Some(character)
}

/// A key without a name of its own in shortcuts: `k`, `3`, `,` or `F5`.
fn key_text(key: Key) -> String {
    match symbol(key) {
        Some(character) => character.to_string(),
        None if key.name().len() == 1 => key.name().to_lowercase(),
        None => key.name().to_owned(),
    }
}

#[cfg(test)]
mod tests;
