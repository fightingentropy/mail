//! What the window shows and what each action does, without any drawing.
//! Keeping this free of egui lets the tests drive the window like a person
//! at the keyboard.

use std::{
    collections::{BTreeSet, HashMap},
    time::{Duration, Instant},
};

use mail_core::{
    compose::Draft,
    model::{Account, Attachment, Change, Label, LabelKind, Message, ThreadSummary},
    store::{Order, SearchQuery, Store, ThreadFilter},
};

use super::compose::Compose;

/// How long a selected unread thread stays on screen before it counts as read.
pub const READ_AFTER: Duration = Duration::from_millis(1500);
/// How long a status-bar notice stays before it clears itself.
pub const NOTICE_FOR: Duration = Duration::from_secs(5);

/// The mailboxes in the sidebar, in order, before the user's labels.
pub const MAILBOXES: &[(&str, &str)] = &[
    ("INBOX", "Inbox"),
    ("STARRED", "Starred"),
    ("SENT", "Sent"),
    ("DRAFT", "Drafts"),
    ("ALL", "All mail"),
    ("SPAM", "Spam"),
    ("TRASH", "Trash"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// A label ID, or `ALL` for all mail.
    Mailbox(String),
    Search(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Next,
    Previous,
    Select(usize),
    Archive,
    Trash,
    ToggleUnread,
    ToggleStar,
    Undo,
    Show(String),
    Search(String),
    ClearSearch,
    OpenPicker,
    ClosePicker,
    PickerFilter(String),
    PickerMove(isize),
    /// Toggles the highlighted label, or the given one when clicked.
    PickerToggle(Option<String>),
    ToggleHelp,
    CloseHelp,
    ToggleExpanded(usize),
    Reply,
    ReplyAll,
    NewMessage,
    SaveDraft,
    DiscardDraft,
    CloseCompose,
    /// Gmail saved the draft under this ID.
    DraftSaved(String),
    DraftFailed(String),
    /// ⌘Enter: shows what is about to be sent and asks to confirm.
    RequestSend,
    ConfirmSend,
    CancelSend,
    MessageSent,
    SendFailed(String),
    /// A link clicked in a body.
    OpenLink(String),
    /// An attachment chip clicked: the open thread's message and attachment.
    Download {
        message: usize,
        attachment: usize,
    },
    /// Shows the newest expanded message's HTML in the browser.
    OpenHtml,
}

/// Work for the window shell that the model can't do itself: open a link,
/// download an attachment, show HTML in the browser, save or discard a
/// draft. Each one follows a click or a key press.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    OpenLink(String),
    Download {
        message_id: String,
        attachment: Attachment,
    },
    OpenHtml {
        message_id: String,
        html: String,
    },
    /// Save to Gmail's Drafts, replacing the draft with this ID if there is one.
    SaveDraft {
        draft: Draft,
        draft_id: Option<String>,
    },
    DiscardDraft(String),
    /// Save and send, after you confirmed. The only way Mail sends mail.
    SendDraft {
        draft: Draft,
        draft_id: Option<String>,
    },
}

/// Who and what a send goes to, as the confirmation shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendSummary {
    pub to: String,
    pub cc: String,
    pub subject: String,
}

/// The thread in the reader.
pub struct OpenThread {
    pub thread_id: String,
    pub subject: String,
    pub messages: Vec<Message>,
    pub expanded: Vec<bool>,
}

pub struct Picker {
    pub filter: String,
    pub highlighted: usize,
}

/// Enough to put a thread back the way it was.
struct Undo {
    thread_id: String,
    labels_before: BTreeSet<String>,
    trashed: bool,
}

pub struct Model {
    store: Store,
    pub account: Account,
    pub labels: Vec<Label>,
    pub unread: HashMap<String, usize>,
    pub view: View,
    /// The mailbox to return to when a search is cleared.
    mailbox: String,
    pub threads: Vec<ThreadSummary>,
    pub selected: usize,
    /// True when the selection moved by keyboard and should scroll into view.
    pub reveal_selected: bool,
    pub open: Option<OpenThread>,
    pub picker: Option<Picker>,
    pub help: bool,
    /// One line for the status bar, such as "Archived. Press z to undo.",
    /// and when it was set. See [`Model::notify`].
    notice: Option<(String, Instant)>,
    /// The Undo shortcut for that line; the window keeps it current.
    pub undo_key: String,
    /// Set after a change, so the window can ask sync to send it.
    pub wants_sync: bool,
    /// The reply or new message being written, shown instead of the reader.
    pub compose: Option<Compose>,
    /// What ⌘Enter is about to send, shown for confirmation.
    pub confirm_send: Option<SendSummary>,
    /// Waiting for the shell, oldest first.
    pub effects: Vec<Effect>,
    undo: Option<Undo>,
    viewed_since: Option<Instant>,
}

impl Model {
    pub fn new(store: Store, account: Account) -> anyhow::Result<Self> {
        let mut model = Self {
            store,
            account,
            labels: Vec::new(),
            unread: HashMap::new(),
            view: View::Mailbox("INBOX".into()),
            mailbox: "INBOX".into(),
            threads: Vec::new(),
            selected: 0,
            reveal_selected: false,
            open: None,
            picker: None,
            help: false,
            notice: None,
            undo_key: "z".into(),
            wants_sync: false,
            compose: None,
            confirm_send: None,
            effects: Vec::new(),
            undo: None,
            viewed_since: None,
        };
        model.reload()?;
        Ok(model)
    }

    pub fn selected_thread(&self) -> Option<&ThreadSummary> {
        self.threads.get(self.selected)
    }

    pub fn pending_changes(&self) -> usize {
        self.store
            .pending_changes(self.account.id)
            .map(|pending| pending.len())
            .unwrap_or(0)
    }

    /// User labels matching the picker filter.
    pub fn picker_labels(&self) -> Vec<&Label> {
        let filter = self
            .picker
            .as_ref()
            .map(|picker| picker.filter.to_lowercase())
            .unwrap_or_default();
        self.labels
            .iter()
            .filter(|label| label.kind == LabelKind::User)
            .filter(|label| label.name.to_lowercase().contains(&filter))
            .collect()
    }

    /// Reads everything again from the database, keeping the selection on
    /// the same thread when it is still listed, or at the same position.
    pub fn reload(&mut self) -> anyhow::Result<()> {
        // Sync may have stored the account's first history ID meanwhile.
        if let Some(account) = self.store.account()? {
            self.account = account;
        }
        self.labels = self.store.labels(self.account.id)?;
        self.unread = self.store.unread_counts(self.account.id)?;
        let previous = self.selected_thread().map(|t| t.thread_id.clone());
        self.threads = match &self.view {
            View::Mailbox(id) if id == "ALL" => {
                self.store.threads(&self.account, &ThreadFilter::AllMail)?
            }
            View::Mailbox(id) => self
                .store
                .threads(&self.account, &ThreadFilter::Label(id.clone()))?,
            View::Search(text) => self.store.search_threads(
                &self.account,
                &SearchQuery {
                    text: text.clone(),
                    order: Order::Newest,
                    limit: 300,
                    ..SearchQuery::default()
                },
            )?,
        };
        if let Some(index) =
            previous.and_then(|id| self.threads.iter().position(|t| t.thread_id == id))
        {
            self.selected = index;
        }
        self.selected = self.selected.min(self.threads.len().saturating_sub(1));
        self.load_open_thread()
    }

    pub fn apply(&mut self, action: Action) -> anyhow::Result<()> {
        match action {
            Action::Next => self.move_selection(1)?,
            Action::Previous => self.move_selection(-1)?,
            Action::Select(index) if index < self.threads.len() => {
                self.selected = index;
                self.load_open_thread()?;
            }
            Action::Select(_) => {}
            Action::Archive => {
                let in_inbox = self
                    .selected_thread()
                    .is_some_and(|t| t.label_ids.iter().any(|id| id == "INBOX"));
                if in_inbox {
                    self.triage(Change::archive(), "Archived")?;
                } else {
                    self.notify("Not in the inbox.".into());
                }
            }
            Action::Trash => self.triage(Change::Trash, "Moved to Trash")?,
            Action::ToggleUnread => {
                let Some(thread) = self.selected_thread() else {
                    return Ok(());
                };
                let (change, done) = if thread.unread {
                    (Change::remove("UNREAD"), "Marked as read")
                } else {
                    (Change::add("UNREAD"), "Marked as unread")
                };
                self.triage(change, done)?;
                // Don't mark it read again straight away.
                self.viewed_since = None;
            }
            Action::ToggleStar => {
                let Some(thread) = self.selected_thread() else {
                    return Ok(());
                };
                let (change, done) = if thread.starred {
                    (Change::remove("STARRED"), "Unstarred")
                } else {
                    (Change::add("STARRED"), "Starred")
                };
                self.triage(change, done)?;
            }
            Action::Undo => self.undo()?,
            Action::Show(mailbox) => {
                self.mailbox = mailbox.clone();
                self.view = View::Mailbox(mailbox);
                self.selected = 0;
                self.reload()?;
            }
            Action::Search(text) if text.trim().is_empty() => self.apply(Action::ClearSearch)?,
            // One letter would match most of the mailbox; wait for a second.
            Action::Search(text) if text.trim().chars().count() < 2 => {}
            Action::Search(text) => {
                self.view = View::Search(text);
                self.selected = 0;
                self.reload()?;
            }
            Action::ClearSearch => {
                if matches!(self.view, View::Search(_)) {
                    self.view = View::Mailbox(self.mailbox.clone());
                    self.selected = 0;
                    self.reload()?;
                }
            }
            Action::OpenPicker => {
                if self.selected_thread().is_some() {
                    self.picker = Some(Picker {
                        filter: String::new(),
                        highlighted: 0,
                    });
                }
            }
            Action::ClosePicker => self.picker = None,
            Action::PickerFilter(filter) => {
                if let Some(picker) = &mut self.picker {
                    picker.filter = filter;
                    picker.highlighted = 0;
                }
            }
            Action::PickerMove(step) => {
                let count = self.picker_labels().len();
                if let Some(picker) = &mut self.picker
                    && count > 0
                {
                    let next = picker.highlighted as isize + step;
                    picker.highlighted = next.clamp(0, count as isize - 1) as usize;
                }
            }
            Action::PickerToggle(label_id) => self.toggle_label(label_id)?,
            Action::ToggleHelp => self.help = !self.help,
            Action::CloseHelp => self.help = false,
            Action::Reply | Action::ReplyAll => {
                let all = action == Action::ReplyAll;
                match self.reply_target() {
                    Some(message) => {
                        self.compose = Some(Compose::reply(message, &self.account.email, all));
                    }
                    None => self.notify("Nothing to reply to.".into()),
                }
            }
            Action::NewMessage => self.compose = Some(Compose::new_message()),
            Action::SaveDraft => {
                if let Some(compose) = &mut self.compose
                    && let Some(draft) = compose.begin_save()
                {
                    self.effects.push(Effect::SaveDraft {
                        draft,
                        draft_id: compose.draft_id.clone(),
                    });
                    self.notify("Saving to Drafts…".into());
                }
            }
            Action::DiscardDraft => {
                if let Some(compose) = self.compose.take() {
                    if let Some(draft_id) = compose.draft_id {
                        self.effects.push(Effect::DiscardDraft(draft_id));
                    }
                    self.notify("Draft discarded.".into());
                }
            }
            Action::CloseCompose => match &self.compose {
                Some(compose) if compose.is_dirty() => {
                    self.notify("Not saved yet: save it to Drafts or discard it.".into());
                }
                Some(_) => self.compose = None,
                None => {}
            },
            Action::DraftSaved(draft_id) => {
                if let Some(compose) = &mut self.compose {
                    if compose.is_sending() {
                        // The save half of a send: remember the draft so a
                        // failed send can be retried without a second one.
                        compose.draft_id = Some(draft_id);
                        return Ok(());
                    }
                    compose.saved(draft_id);
                }
                self.notify("Saved to Drafts.".into());
                // Bring the draft into the local copy.
                self.wants_sync = true;
            }
            Action::RequestSend => self.request_send(),
            Action::ConfirmSend => {
                if self.confirm_send.take().is_some()
                    && let Some(compose) = &mut self.compose
                    && let Some(draft) = compose.begin_send()
                {
                    self.effects.push(Effect::SendDraft {
                        draft,
                        draft_id: compose.draft_id.clone(),
                    });
                    self.notify("Sending…".into());
                }
            }
            Action::CancelSend => self.confirm_send = None,
            Action::MessageSent => {
                self.compose = None;
                self.notify("Sent.".into());
                self.wants_sync = true;
            }
            Action::SendFailed(error) => {
                if let Some(compose) = &mut self.compose {
                    compose.send_failed();
                }
                self.notify(error);
            }
            Action::DraftFailed(error) => {
                if let Some(compose) = &mut self.compose {
                    compose.save_failed();
                }
                self.notify(error);
            }
            Action::ToggleExpanded(index) => {
                if let Some(expanded) = self
                    .open
                    .as_mut()
                    .and_then(|open| open.expanded.get_mut(index))
                {
                    *expanded = !*expanded;
                }
            }
            Action::OpenLink(url) => self.effects.push(Effect::OpenLink(url)),
            Action::Download {
                message,
                attachment,
            } => {
                let found = self
                    .open
                    .as_ref()
                    .and_then(|open| open.messages.get(message))
                    .and_then(|message| {
                        let attachment = message.attachments.get(attachment)?.clone();
                        Some((message.gmail_id.clone(), attachment))
                    });
                if let Some((message_id, attachment)) = found {
                    self.effects.push(Effect::Download {
                        message_id,
                        attachment,
                    });
                }
            }
            Action::OpenHtml => self.open_html()?,
        }
        Ok(())
    }

    /// Marks the selected thread read once it has been on screen for
    /// [`READ_AFTER`]. Returns how long to wait before checking again.
    /// The status bar's notice, if one is showing.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_ref().map(|(text, _)| text.as_str())
    }

    /// Shows a notice in the status bar. It clears itself after
    /// [`NOTICE_FOR`], unless it reports work still going ("Sending…"),
    /// which stays until the result replaces it.
    pub fn notify(&mut self, text: String) {
        self.notice = Some((text, Instant::now()));
    }

    /// Clears an old notice and marks the open thread read once it has been
    /// on screen long enough. Returns how long until the next of those.
    pub fn tick(&mut self, now: Instant) -> anyhow::Result<Option<Duration>> {
        let waits = [self.expire_notice(now), self.mark_read_when_due(now)?];
        Ok(waits.into_iter().flatten().min())
    }

    fn expire_notice(&mut self, now: Instant) -> Option<Duration> {
        let (text, since) = self.notice.as_ref()?;
        if text.ends_with('…') {
            return None;
        }
        let shown = now.saturating_duration_since(*since);
        if shown < NOTICE_FOR {
            return Some(NOTICE_FOR - shown);
        }
        self.notice = None;
        None
    }

    fn mark_read_when_due(&mut self, now: Instant) -> anyhow::Result<Option<Duration>> {
        let unread = self.selected_thread().is_some_and(|thread| thread.unread);
        let Some(since) = self.viewed_since.filter(|_| unread) else {
            return Ok(None);
        };
        let shown = now.saturating_duration_since(since);
        if shown < READ_AFTER {
            return Ok(Some(READ_AFTER - shown));
        }
        let thread_id = self.threads[self.selected].thread_id.clone();
        self.store
            .apply_change(self.account.id, &thread_id, &Change::remove("UNREAD"))?;
        self.viewed_since = None;
        self.wants_sync = true;
        self.reload()?;
        Ok(None)
    }

    /// Checks the draft can go, then asks for confirmation. Nothing is sent
    /// until `ConfirmSend`.
    fn request_send(&mut self) {
        let Some(compose) = &self.compose else {
            return;
        };
        if compose.is_sending() {
            return;
        }
        if compose.is_saving() {
            self.notify("Wait for the draft to finish saving.".into());
            return;
        }
        let draft = compose.draft();
        if !draft.has_recipients() {
            self.notify("Add a recipient first.".into());
            return;
        }
        let invalid = draft.invalid_addresses();
        if !invalid.is_empty() {
            self.notify(format!("Not an email address: {}", invalid.join(", ")));
            return;
        }
        self.confirm_send = Some(SendSummary {
            to: draft.to,
            cc: draft.cc,
            subject: draft.subject,
        });
    }

    /// The message a reply answers: the newest one in the open thread that
    /// isn't itself a draft.
    fn reply_target(&self) -> Option<&Message> {
        self.open
            .as_ref()?
            .messages
            .iter()
            .rev()
            .find(|message| !message.has_label("DRAFT"))
    }

    /// The newest expanded message with an HTML version goes to the browser:
    /// that is the message on screen, or the one you just expanded.
    fn open_html(&mut self) -> anyhow::Result<()> {
        let Some(open) = &self.open else {
            return Ok(());
        };
        let newest = open
            .messages
            .iter()
            .zip(&open.expanded)
            .rev()
            .find(|(message, expanded)| **expanded && message.has_html)
            .map(|(message, _)| message.gmail_id.clone());
        // HTML stays compressed in the database until it is asked for.
        let html = match &newest {
            Some(message_id) => self.store.message_html(self.account.id, message_id)?,
            None => None,
        };
        match newest.zip(html) {
            Some((message_id, html)) => self.effects.push(Effect::OpenHtml { message_id, html }),
            None => self.notify("This message has no HTML version.".into()),
        }
        Ok(())
    }

    fn move_selection(&mut self, step: isize) -> anyhow::Result<()> {
        if self.threads.is_empty() {
            return Ok(());
        }
        let last = self.threads.len() as isize - 1;
        let next = (self.selected as isize + step).clamp(0, last) as usize;
        if next != self.selected {
            self.selected = next;
            self.reveal_selected = true;
            self.load_open_thread()?;
        }
        Ok(())
    }

    /// Applies a change to the selected thread, remembering how to undo it.
    /// The selection stays at the same position, which is the next thread
    /// when this one leaves the list.
    fn triage(&mut self, change: Change, done: &str) -> anyhow::Result<()> {
        let Some(thread) = self.selected_thread() else {
            return Ok(());
        };
        let thread_id = thread.thread_id.clone();
        self.undo = Some(Undo {
            labels_before: thread.label_ids.iter().cloned().collect(),
            trashed: change == Change::Trash,
            thread_id: thread_id.clone(),
        });
        self.store
            .apply_change(self.account.id, &thread_id, &change)?;
        self.notify(match self.undo_key.as_str() {
            "" => format!("{done}."),
            key => format!("{done}. Press {key} to undo."),
        });
        self.wants_sync = true;
        self.forget_selection_if_gone(&thread_id)?;
        Ok(())
    }

    fn forget_selection_if_gone(&mut self, thread_id: &str) -> anyhow::Result<()> {
        let position = self.selected;
        self.reload()?;
        // reload() follows the thread if it is still listed; if it left, keep
        // the position so the next thread is selected.
        if self
            .selected_thread()
            .is_none_or(|t| t.thread_id != thread_id)
        {
            self.selected = position.min(self.threads.len().saturating_sub(1));
            self.load_open_thread()?;
        }
        Ok(())
    }

    fn undo(&mut self) -> anyhow::Result<()> {
        let Some(undo) = self.undo.take() else {
            self.notify("Nothing to undo.".into());
            return Ok(());
        };
        let account = self.account.id;
        if undo.trashed {
            self.store
                .apply_change(account, &undo.thread_id, &Change::Untrash)?;
        }
        let now: BTreeSet<String> = self
            .store
            .thread(account, &undo.thread_id)?
            .into_iter()
            .flat_map(|message| message.label_ids)
            .collect();
        let add: Vec<String> = undo.labels_before.difference(&now).cloned().collect();
        let remove: Vec<String> = now.difference(&undo.labels_before).cloned().collect();
        if !add.is_empty() || !remove.is_empty() {
            self.store
                .apply_change(account, &undo.thread_id, &Change::Labels { add, remove })?;
        }
        self.notify("Undone.".into());
        self.wants_sync = true;
        self.reload()?;
        if let Some(index) = self
            .threads
            .iter()
            .position(|t| t.thread_id == undo.thread_id)
        {
            self.selected = index;
            self.reveal_selected = true;
            self.load_open_thread()?;
        }
        Ok(())
    }

    fn toggle_label(&mut self, clicked: Option<String>) -> anyhow::Result<()> {
        let label = match clicked {
            Some(id) => self.labels.iter().find(|label| label.id == id).cloned(),
            None => {
                let highlighted = self.picker.as_ref().map_or(0, |p| p.highlighted);
                self.picker_labels()
                    .get(highlighted)
                    .map(|label| (*label).clone())
            }
        };
        let (Some(label), Some(thread)) = (label, self.selected_thread()) else {
            return Ok(());
        };
        let has_label = thread.label_ids.contains(&label.id);
        let (change, done) = if has_label {
            (Change::remove(&label.id), format!("Removed {}", label.name))
        } else {
            (Change::add(&label.id), format!("Labeled {}", label.name))
        };
        self.picker = None;
        self.triage(change, &done)
    }

    fn load_open_thread(&mut self) -> anyhow::Result<()> {
        let Some(summary) = self.threads.get(self.selected) else {
            self.open = None;
            self.viewed_since = None;
            return Ok(());
        };
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.thread_id == summary.thread_id)
        {
            // Same thread: refresh messages and labels but keep what is expanded.
            let messages = self.store.thread(self.account.id, &summary.thread_id)?;
            if let Some(open) = &mut self.open
                && open.messages.len() == messages.len()
            {
                open.messages = messages;
                return Ok(());
            }
        }
        let messages = self.store.thread(self.account.id, &summary.thread_id)?;
        let last = messages.len().saturating_sub(1);
        let expanded = messages
            .iter()
            .enumerate()
            .map(|(index, message)| index == last || message.has_label("UNREAD"))
            .collect();
        self.open = Some(OpenThread {
            thread_id: summary.thread_id.clone(),
            subject: summary.subject.clone(),
            messages,
            expanded,
        });
        self.viewed_since = Some(Instant::now());
        Ok(())
    }
}

#[cfg(test)]
mod tests;
