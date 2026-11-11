//! What the window shows and what each action does, without any drawing.
//! Keeping this free of egui lets the tests drive the window like a person
//! at the keyboard.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
    sync::{Arc, mpsc::TryRecvError},
    time::{Duration, Instant},
};

use mail_core::{
    compose::Draft,
    gmail::inline,
    model::{Account, Attachment, Change, InlineImage, Label, LabelKind, Message, ThreadSummary},
    store::{Order, SearchQuery, Store, ThreadFilter},
};

use super::compose::Compose;
use originals::{Built, Loaded, Loader, Request};

/// How long a selected unread thread stays on screen before it counts as read.
pub const READ_AFTER: Duration = Duration::from_millis(1500);
/// How long a notice stays before it clears itself.
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

impl Action {
    /// Actions that must not fire again while a key is held down: they send,
    /// discard or remove mail, and the first press may already have been
    /// the one that asked.
    pub fn once_per_press(&self) -> bool {
        matches!(
            self,
            Action::RequestSend
                | Action::ConfirmSend
                | Action::Archive
                | Action::Trash
                | Action::DiscardDraft
        )
    }
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
    /// Write the new message from another of your accounts.
    SetFrom(String),
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
    /// Switches between a message's original layout and its text.
    ToggleOriginal,
    /// Lets the original fetch its remote images.
    LoadImages,
    /// Whether originals load their remote images as they open, from
    /// Settings.
    SetRemoteImages(bool),
    /// Whether emails show as designed or as text, from Settings.
    SetShowOriginal(bool),
    /// Shows one account's mail, or every account's with `None`.
    SetScope(Option<i64>),
    /// Starts Google sign-in for another account.
    AddAccount,
    /// Signs in to an iCloud account with an app-specific password.
    AddIcloudAccount {
        address: String,
        password: String,
    },
    /// Asks for an image to use as the photo of the account in the header.
    ChoosePhoto,
    /// Drops the chosen photo, going back to Google's.
    UseGooglePhoto,
    /// Pictures sent inside a message, read from Gmail, or why none came.
    InlineImagesArrived {
        account_id: i64,
        message_id: String,
        images: Result<Vec<InlineImage>, String>,
    },
}

/// Work for the window shell that the model can't do itself: open a link,
/// download an attachment, show HTML in the browser, save or discard a
/// draft. Each one follows a click or a key press.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    OpenLink(String),
    Download {
        /// The address of the account the message belongs to.
        account: String,
        message_id: String,
        attachment: Attachment,
    },
    OpenHtml {
        message_id: String,
        html: String,
    },
    /// Save to Gmail's Drafts, replacing the draft with this ID if there is one.
    /// `from` is the address of the account the draft belongs to.
    SaveDraft {
        from: String,
        draft: Draft,
        draft_id: Option<String>,
    },
    DiscardDraft {
        from: String,
        draft_id: String,
    },
    /// Save and send, after you confirmed. The only way Mail sends mail.
    SendDraft {
        from: String,
        draft: Draft,
        draft_id: Option<String>,
    },
    /// Google sign-in in the browser, for another account.
    AddAccount,
    /// Check an iCloud app-specific password and add the account.
    AddIcloudAccount {
        address: String,
        password: String,
    },
    /// Pick an image file for this account's photo.
    ChoosePhoto(String),
    UseGooglePhoto(String),
    /// Read the pictures with these Content-IDs, for the original. `account`
    /// is the address of the account the message belongs to.
    FetchInlineImages {
        account: String,
        account_id: i64,
        message_id: String,
        wanted: Vec<String>,
    },
}

/// A line about what just happened, and when it was set.
struct Notice {
    text: String,
    since: Instant,
    /// Triage, which the toast offers to undo.
    undoable: bool,
}

/// Who and what a send goes to, as the confirmation shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendSummary {
    /// The account it goes from, when you have more than one.
    pub from: Option<String>,
    pub to: String,
    pub cc: String,
    pub subject: String,
}

/// A message's HTML, for showing it as it was designed.
pub struct OriginalHtml {
    pub message_id: String,
    /// With the pictures sent inside it in place, as far as Mail has them.
    pub html: Arc<str>,
    /// Remote images may load: they tell the sender you opened it.
    pub images: bool,
    /// How many of the pictures sent inside it are in place, so the view
    /// shows them when they arrive.
    pub inline: usize,
    /// The Content-IDs of pictures it shows that Mail doesn't have yet.
    missing: Vec<String>,
}

/// The thread in the reader.
pub struct OpenThread {
    pub account_id: i64,
    pub thread_id: String,
    /// The HTML of a thread that is one message, as newsletters and
    /// receipts are. Conversations read as text.
    /// `None` while it is first built, `Some` once it can be shown.
    pub original: Option<OriginalHtml>,
    /// The original being built away from the window thread. Set with no
    /// `original` while it first loads, when the reader shows the headers
    /// and text; set with one while pictures that arrived are put in,
    /// when the old page stays.
    loading: Option<Pending>,
    pub subject: String,
    pub messages: Vec<Message>,
    pub expanded: Vec<bool>,
}

impl OpenThread {
    /// Whether its original is still being built.
    pub fn loading_original(&self) -> bool {
        self.loading.is_some()
    }
}

/// The original asked of the loader, and the generation its answer must
/// carry to be used.
struct Pending {
    generation: u64,
    message_id: String,
}

/// An original ready to show, from what was built.
fn original_html(built: Built, message_id: &str, images: bool) -> OriginalHtml {
    OriginalHtml {
        message_id: message_id.to_owned(),
        html: built.html,
        images,
        inline: built.inline,
        missing: built.missing,
    }
}

pub struct Picker {
    pub filter: String,
    pub highlighted: usize,
}

/// Enough to put a thread back the way it was.
struct Undo {
    account_id: i64,
    thread_id: String,
    /// The changes that reverse the one just applied, in order. Only what
    /// that change did is reversed, so labels sync added since stay.
    inverse: Vec<Change>,
}

/// The changes that put a thread whose labels were `before` back after
/// `change` was applied to it.
fn inverse_of(change: &Change, before: &BTreeSet<String>) -> Vec<Change> {
    match change {
        Change::Labels { add, remove } => {
            let restore: Vec<String> = remove
                .iter()
                .filter(|label| before.contains(*label))
                .cloned()
                .collect();
            let drop: Vec<String> = add
                .iter()
                .filter(|label| !before.contains(*label))
                .cloned()
                .collect();
            if restore.is_empty() && drop.is_empty() {
                Vec::new()
            } else {
                vec![Change::Labels {
                    add: restore,
                    remove: drop,
                }]
            }
        }
        // Trashing also takes the thread out of the inbox.
        Change::Trash => {
            let mut undo = Vec::new();
            if !before.contains("TRASH") {
                undo.push(Change::Untrash);
            }
            if before.contains("INBOX") {
                undo.push(Change::add("INBOX"));
            }
            undo
        }
        Change::Untrash if before.contains("TRASH") => vec![Change::add("TRASH")],
        Change::Untrash => Vec::new(),
    }
}

pub struct Model {
    store: Store,
    /// Every signed-in account, in the order they were added.
    pub accounts: Vec<Account>,
    /// The account whose mail is shown, or `None` for all of them.
    pub scope: Option<i64>,
    /// Each account's labels.
    labels: HashMap<i64, Vec<Label>>,
    /// Unread threads per label, for each account.
    unread: HashMap<i64, HashMap<String, usize>>,
    /// The name each account signs its mail with, once one is stored.
    names: HashMap<i64, String>,
    pub view: View,
    /// Show a message's original layout when it has one, rather than text.
    pub show_original: bool,
    /// Originals load their remote images as they open. See
    /// [`super::preferences::Preferences::remote_images`].
    pub remote_images: bool,
    /// The mailbox to return to when a search is cleared.
    mailbox: String,
    pub threads: Vec<ThreadSummary>,
    pub selected: usize,
    /// True when the selection moved by keyboard and should scroll into view.
    pub reveal_selected: bool,
    pub open: Option<OpenThread>,
    pub picker: Option<Picker>,
    pub help: bool,
    /// What just happened, shown as a toast. See [`Model::notify`].
    notice: Option<Notice>,
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
    /// Messages whose inline pictures were asked of Gmail since Mail
    /// started, so a picture Gmail doesn't have isn't asked for again.
    inline_requested: HashSet<(i64, String)>,
    /// Builds originals away from the window thread once started. `None`
    /// builds them on the spot, as tests and screenshots do.
    originals: Option<Loader>,
    /// The last few originals built.
    cache: originals::Cache,
    /// Counts originals asked for, so an answer for a message no longer
    /// open is told apart and dropped.
    generation: u64,
}

impl Model {
    pub fn new(store: Store) -> anyhow::Result<Self> {
        let mut model = Self {
            store,
            accounts: Vec::new(),
            scope: None,
            labels: HashMap::new(),
            unread: HashMap::new(),
            names: HashMap::new(),
            view: View::Mailbox("INBOX".into()),
            show_original: true,
            remote_images: true,
            mailbox: "INBOX".into(),
            threads: Vec::new(),
            selected: 0,
            reveal_selected: false,
            open: None,
            picker: None,
            help: false,
            notice: None,
            wants_sync: false,
            compose: None,
            confirm_send: None,
            effects: Vec::new(),
            undo: None,
            viewed_since: None,
            inline_requested: HashSet::new(),
            originals: None,
            cache: originals::Cache::default(),
            generation: 0,
        };
        model.reload()?;
        Ok(model)
    }

    pub fn selected_thread(&self) -> Option<&ThreadSummary> {
        self.threads.get(self.selected)
    }

    pub fn pending_changes(&self) -> usize {
        self.accounts
            .iter()
            .filter_map(|account| self.store.pending_changes(account.id).ok())
            .map(|pending| pending.len())
            .sum()
    }

    /// The original the reader shows instead of text, if any.
    pub fn showing_original(&self) -> Option<&OriginalHtml> {
        if !self.show_original || self.compose.is_some() {
            return None;
        }
        self.open.as_ref()?.original.as_ref()
    }

    pub fn account(&self, id: i64) -> Option<&Account> {
        self.accounts.iter().find(|account| account.id == id)
    }

    /// Your address in an account, for "me" and for replies.
    pub fn own_address(&self, account_id: i64) -> &str {
        self.account(account_id)
            .map_or("", |account| account.email.as_str())
    }

    /// The account the sidebar header stands for: the one shown, or the
    /// first when all are.
    pub fn header_account(&self) -> Option<&Account> {
        self.scope
            .and_then(|id| self.account(id))
            .or(self.accounts.first())
    }

    /// The first name an account signs its mail with, such as "Erlin".
    pub fn first_name(&self, account_id: i64) -> Option<&str> {
        let name = self.names.get(&account_id)?;
        // A bare address as the name says nothing new.
        name.split_whitespace()
            .next()
            .filter(|word| !word.contains('@'))
    }

    /// The accounts the list shows: the chosen one, or all of them.
    pub fn shown_accounts(&self) -> impl Iterator<Item = &Account> {
        self.accounts
            .iter()
            .filter(|account| self.scope.is_none_or(|id| id == account.id))
    }

    pub fn labels(&self, account_id: i64) -> &[Label] {
        self.labels.get(&account_id).map_or(&[], Vec::as_slice)
    }

    /// Unread threads with `label` in the accounts shown.
    pub fn unread(&self, label: &str) -> usize {
        self.shown_accounts()
            .map(|account| self.unread_in(account.id, label))
            .sum()
    }

    pub fn unread_in(&self, account_id: i64, label: &str) -> usize {
        self.unread
            .get(&account_id)
            .and_then(|counts| counts.get(label))
            .copied()
            .unwrap_or(0)
    }

    /// When the shown accounts last synced: the oldest of them, or `None`
    /// while any has never synced.
    pub fn synced_at(&self) -> Option<i64> {
        self.shown_accounts()
            .map(|account| account.synced_at)
            .collect::<Option<Vec<i64>>>()?
            .into_iter()
            .min()
    }

    /// User labels matching the picker filter, from the selected thread's
    /// account.
    pub fn picker_labels(&self) -> Vec<&Label> {
        let filter = self
            .picker
            .as_ref()
            .map(|picker| picker.filter.to_lowercase())
            .unwrap_or_default();
        let account = self.selected_thread().map_or(0, |thread| thread.account_id);
        self.labels(account)
            .iter()
            .filter(|label| label.kind == LabelKind::User)
            .filter(|label| label.name.to_lowercase().contains(&filter))
            .collect()
    }

    /// Reads everything again from the database, keeping the selection on
    /// the same thread when it is still listed, or at the same position.
    pub fn reload(&mut self) -> anyhow::Result<()> {
        // Sync moves sync points, and accounts may have been added.
        self.accounts = self.store.accounts()?;
        if self.scope.is_some_and(|id| self.account(id).is_none()) {
            self.scope = None;
        }
        self.labels.clear();
        self.unread.clear();
        for account in &self.accounts {
            self.labels
                .insert(account.id, self.store.labels(account.id)?);
            self.unread
                .insert(account.id, self.store.unread_counts(account.id)?);
            // A name rarely changes; look until the first sent mail arrives.
            if !self.names.contains_key(&account.id)
                && let Some(name) = self.store.own_name(account)?
            {
                self.names.insert(account.id, name);
            }
        }
        self.reload_threads()
    }

    /// Lists the threads again for the current view and scope, leaving the
    /// sidebar's labels and unread counts as they are. Enough when only the
    /// view changed (Show, search), which happens on every keystroke.
    fn reload_threads(&mut self) -> anyhow::Result<()> {
        let previous = self
            .selected_thread()
            .map(|t| (t.account_id, t.thread_id.clone()));
        let shown: Vec<Account> = self.shown_accounts().cloned().collect();
        let mut threads = Vec::new();
        for account in &shown {
            threads.extend(match &self.view {
                View::Mailbox(id) if id == "ALL" => {
                    self.store.threads(account, &ThreadFilter::AllMail)?
                }
                View::Mailbox(id) => self
                    .store
                    .threads(account, &ThreadFilter::Label(id.clone()))?,
                View::Search(text) => self.store.search_threads(
                    account,
                    &SearchQuery {
                        text: text.clone(),
                        order: Order::Newest,
                        limit: 300,
                        ..SearchQuery::default()
                    },
                )?,
            });
        }
        // Every list is newest first, so the accounts merge by date.
        if shown.len() > 1 {
            threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_date));
        }
        self.threads = threads;
        if let Some(index) = previous.and_then(|(account, id)| {
            self.threads
                .iter()
                .position(|t| t.account_id == account && t.thread_id == id)
        }) {
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
                self.reload_threads()?;
            }
            Action::Search(text) if text.trim().is_empty() => self.apply(Action::ClearSearch)?,
            // One letter would match most of the mailbox; wait for a second.
            Action::Search(text) if text.trim().chars().count() < 2 => {}
            Action::Search(text) => {
                self.view = View::Search(text);
                self.selected = 0;
                self.reload_threads()?;
            }
            Action::ClearSearch => {
                if matches!(self.view, View::Search(_)) {
                    self.view = View::Mailbox(self.mailbox.clone());
                    self.selected = 0;
                    self.reload_threads()?;
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
                if self.compose_blocks_new() {
                    return Ok(());
                }
                let all = action == Action::ReplyAll;
                let own = self
                    .open
                    .as_ref()
                    .map(|open| self.own_address(open.account_id));
                match self.reply_target().zip(own) {
                    Some((message, own)) => {
                        self.compose = Some(Compose::reply(message, own, all));
                    }
                    None => self.notify("Nothing to reply to.".into()),
                }
            }
            Action::NewMessage => {
                if self.compose_blocks_new() {
                    return Ok(());
                }
                // From the account shown, or the first when all are.
                if let Some(from) = self.header_account() {
                    self.compose = Some(Compose::new_message(&from.email));
                }
            }
            Action::SetFrom(address) => self.set_from(&address),
            Action::SaveDraft => {
                if let Some(compose) = &mut self.compose
                    && let Some(draft) = compose.begin_save()
                {
                    self.effects.push(Effect::SaveDraft {
                        from: compose.from.clone(),
                        draft,
                        draft_id: compose.draft_id.clone(),
                    });
                    self.notify("Saving to Drafts…".into());
                }
            }
            Action::DiscardDraft => {
                // A save or send in flight would still land, so discarding
                // now would orphan a draft or lose a sent message's record.
                if self
                    .compose
                    .as_ref()
                    .is_some_and(|compose| compose.is_saving() || compose.is_sending())
                {
                    self.notify("Wait for the draft to finish saving or sending.".into());
                    return Ok(());
                }
                if let Some(compose) = self.compose.take() {
                    if let Some(draft_id) = compose.draft_id {
                        self.effects.push(Effect::DiscardDraft {
                            from: compose.from,
                            draft_id,
                        });
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
                        from: compose.from.clone(),
                        draft,
                        draft_id: compose.draft_id.clone(),
                    });
                    self.notify("Sending…".into());
                }
            }
            Action::CancelSend => self.confirm_send = None,
            Action::MessageSent => {
                // Only the message that was sent closes; a new one the user
                // has started since is left alone.
                if self.compose.as_ref().is_none_or(Compose::is_sending) {
                    self.compose = None;
                }
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
                let found = self.open.as_ref().and_then(|open| {
                    let message = open.messages.get(message)?;
                    let attachment = message.attachments.get(attachment)?.clone();
                    Some((open.account_id, message.gmail_id.clone(), attachment))
                });
                if let Some((account, message_id, attachment)) = found {
                    self.effects.push(Effect::Download {
                        account: self.own_address(account).to_owned(),
                        message_id,
                        attachment,
                    });
                }
            }
            Action::OpenHtml => self.open_html()?,
            Action::SetScope(scope) => {
                self.scope = scope.filter(|id| self.account(*id).is_some());
                // Labels belong to one account; the shared mailboxes carry over.
                let shared = |id: &str| MAILBOXES.iter().any(|(mailbox, _)| *mailbox == id);
                if !shared(&self.mailbox) {
                    self.mailbox = "INBOX".into();
                }
                if matches!(&self.view, View::Mailbox(id) if !shared(id)) {
                    self.view = View::Mailbox(self.mailbox.clone());
                }
                self.selected = 0;
                self.reload()?;
            }
            Action::AddAccount => self.effects.push(Effect::AddAccount),
            Action::AddIcloudAccount { address, password } => self
                .effects
                .push(Effect::AddIcloudAccount { address, password }),
            Action::ToggleOriginal => {
                // While it first loads, v still switches: the page shows
                // once it is ready. If it turns out to read as text after
                // all, the setting stays switched; that is rare enough.
                if self
                    .open
                    .as_ref()
                    .is_some_and(|open| open.original.is_some() || open.loading_original())
                {
                    self.show_original = !self.show_original;
                    self.request_inline_images();
                } else {
                    self.notify("This one reads as text. h opens its HTML in your browser.".into());
                }
            }
            Action::LoadImages => {
                if let Some(original) = self.open.as_mut().and_then(|open| open.original.as_mut()) {
                    original.images = true;
                }
            }
            Action::SetShowOriginal(show) => {
                self.show_original = show;
                self.request_inline_images();
            }
            Action::SetRemoteImages(load) => {
                self.remote_images = load;
                // The email on screen follows at once.
                if let Some(original) = self.open.as_mut().and_then(|open| open.original.as_mut()) {
                    original.images = load;
                }
            }
            Action::InlineImagesArrived {
                account_id,
                message_id,
                images,
            } => {
                let images = match images {
                    Ok(images) => images,
                    Err(error) => {
                        // Try again the next time it is opened.
                        self.inline_requested.remove(&(account_id, message_id));
                        self.notify(format!(
                            "Could not load the pictures in this email: {error}"
                        ));
                        return Ok(());
                    }
                };
                self.store
                    .save_inline_images(account_id, &message_id, &images)?;
                // A kept copy lacks them, open or not.
                self.cache.remove(account_id, &message_id);
                // Put them in place if the message is still open, shown or
                // read as text for now, or still loading: what that load
                // read may be from before they were saved.
                let open_here = self.open.as_ref().is_some_and(|open| {
                    open.account_id == account_id
                        && (open
                            .original
                            .as_ref()
                            .is_some_and(|original| original.message_id == message_id)
                            || open
                                .loading
                                .as_ref()
                                .is_some_and(|pending| pending.message_id == message_id))
                });
                if open_here && !images.is_empty() {
                    self.start_original(account_id, message_id)?;
                }
            }
            Action::ChoosePhoto | Action::UseGooglePhoto => {
                if let Some(account) = self.header_account() {
                    let email = account.email.clone();
                    self.effects.push(match action {
                        Action::ChoosePhoto => Effect::ChoosePhoto(email),
                        _ => Effect::UseGooglePhoto(email),
                    });
                }
            }
        }
        Ok(())
    }

    /// Marks the selected thread read once it has been on screen for
    /// [`READ_AFTER`]. Returns how long to wait before checking again.
    /// The toast's line, if one is showing.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_ref().map(|notice| notice.text.as_str())
    }

    /// Whether the toast reports triage that can still be undone, and so
    /// offers Undo.
    pub fn notice_offers_undo(&self) -> bool {
        self.notice.as_ref().is_some_and(|notice| notice.undoable) && self.undo.is_some()
    }

    /// Shows a notice as a toast. It clears itself after [`NOTICE_FOR`],
    /// unless it reports work still going ("Sending…"), which stays until
    /// the result replaces it.
    pub fn notify(&mut self, text: String) {
        self.notice = Some(Notice {
            text,
            since: Instant::now(),
            undoable: false,
        });
    }

    /// Clears an old notice and marks the open thread read once it has been
    /// on screen long enough. Returns how long until the next of those.
    pub fn tick(&mut self, now: Instant) -> anyhow::Result<Option<Duration>> {
        let waits = [self.expire_notice(now), self.mark_read_when_due(now)?];
        Ok(waits.into_iter().flatten().min())
    }

    fn expire_notice(&mut self, now: Instant) -> Option<Duration> {
        let notice = self.notice.as_ref()?;
        if notice.text.ends_with('…') {
            return None;
        }
        let shown = now.saturating_duration_since(notice.since);
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
        let thread = &self.threads[self.selected];
        let (account, thread_id) = (thread.account_id, thread.thread_id.clone());
        self.store
            .apply_change(account, &thread_id, &Change::remove("UNREAD"))?;
        self.viewed_since = None;
        self.wants_sync = true;
        self.reload()?;
        Ok(None)
    }

    /// True, with a notice, when starting another message would throw away
    /// the one being written: unsaved edits, or a save or send in flight.
    fn compose_blocks_new(&mut self) -> bool {
        let busy = self.compose.as_ref().is_some_and(|compose| {
            compose.is_dirty() || compose.is_saving() || compose.is_sending()
        });
        if busy {
            self.notify("Save or discard the message you are writing first.".into());
        }
        busy
    }

    /// Writes the new message from another account. A copy already in the
    /// old account's Drafts is discarded there; the next save puts it in the
    /// new account's.
    fn set_from(&mut self, address: &str) {
        let known = self.accounts.iter().any(|account| account.email == address);
        let Some(compose) = &mut self.compose else {
            return;
        };
        if !known || compose.from == address || !compose.is_new_message() {
            return;
        }
        if compose.is_saving() || compose.is_sending() {
            self.notify("Wait for the draft to finish saving.".into());
            return;
        }
        let old = compose.from.clone();
        if let Some(draft_id) = compose.change_from(address) {
            self.effects.push(Effect::DiscardDraft {
                from: old,
                draft_id,
            });
        }
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
        let from = (self.accounts.len() > 1).then(|| compose.from.clone());
        self.confirm_send = Some(SendSummary {
            from,
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
            Some(message_id) => self
                .store
                .message_html(open.account_id, message_id)?
                .map(|html| -> anyhow::Result<String> {
                    let images = self.store.inline_images(open.account_id, message_id)?;
                    Ok(inline::embed(&html, &images).into_owned())
                })
                .transpose()?,
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
        let (account, thread_id) = (thread.account_id, thread.thread_id.clone());
        let before: BTreeSet<String> = thread.label_ids.iter().cloned().collect();
        self.undo = Some(Undo {
            inverse: inverse_of(&change, &before),
            account_id: account,
            thread_id: thread_id.clone(),
        });
        self.store.apply_change(account, &thread_id, &change)?;
        self.notice = Some(Notice {
            text: done.to_owned(),
            since: Instant::now(),
            undoable: true,
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
        let account = undo.account_id;
        for change in &undo.inverse {
            self.store.apply_change(account, &undo.thread_id, change)?;
        }
        self.notify("Undone.".into());
        self.wants_sync = true;
        self.reload()?;
        if let Some(index) = self
            .threads
            .iter()
            .position(|t| t.account_id == account && t.thread_id == undo.thread_id)
        {
            self.selected = index;
            self.reveal_selected = true;
            self.load_open_thread()?;
        }
        Ok(())
    }

    fn toggle_label(&mut self, clicked: Option<String>) -> anyhow::Result<()> {
        let label = match clicked {
            Some(id) => self
                .picker_labels()
                .into_iter()
                .find(|label| label.id == id)
                .cloned(),
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
        let account = summary.account_id;
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.account_id == account && open.thread_id == summary.thread_id)
        {
            // Same thread: refresh messages and labels but keep what is expanded.
            let messages = self.store.thread(account, &summary.thread_id)?;
            if let Some(open) = &mut self.open
                && open.messages.len() == messages.len()
            {
                open.messages = messages;
                return Ok(());
            }
        }
        let messages = self.store.thread(account, &summary.thread_id)?;
        let last = messages.len().saturating_sub(1);
        let expanded = messages
            .iter()
            .enumerate()
            .map(|(index, message)| index == last || message.has_label("UNREAD"))
            .collect();
        let original_id = match messages.as_slice() {
            [message] if message.has_html => Some(message.gmail_id.clone()),
            _ => None,
        };
        self.open = Some(OpenThread {
            account_id: account,
            original: None,
            loading: None,
            thread_id: summary.thread_id.clone(),
            subject: summary.subject.clone(),
            messages,
            expanded,
        });
        if let Some(message_id) = original_id {
            self.start_original(account, message_id)?;
        }
        self.viewed_since = Some(Instant::now());
        self.request_inline_images();
        Ok(())
    }

    /// Builds originals on a thread of their own from now on, reading
    /// `database` through a read-only connection. `wake` runs as each one is
    /// ready. On an error originals keep being built on the spot.
    pub fn start_original_loader(
        &mut self,
        database: &Path,
        wake: impl Fn() + Send + 'static,
    ) -> anyhow::Result<()> {
        let store = Store::open_read_only(database)?;
        self.originals = Some(Loader::start(store, wake)?);
        Ok(())
    }

    /// Shows the open thread's original of `message_id`: at once when it
    /// is kept, otherwise once the loader has built it, or built on the
    /// spot when there is no loader.
    fn start_original(&mut self, account_id: i64, message_id: String) -> anyhow::Result<()> {
        self.generation += 1;
        let generation = self.generation;
        if self.open.is_none() {
            return Ok(());
        }
        if let Some(built) = self.cache.get(account_id, &message_id) {
            let images = self.images_for(&message_id);
            if let Some(open) = &mut self.open {
                open.original = Some(original_html(built, &message_id, images));
                open.loading = None;
            }
            return Ok(());
        }
        if let Some(loader) = &self.originals {
            let request = Request {
                generation,
                account_id,
                message_id: message_id.clone(),
            };
            if loader.send(request).is_ok() {
                if let Some(open) = &mut self.open {
                    // The page on screen, if any, stays until the new one.
                    open.loading = Some(Pending {
                        generation,
                        message_id,
                    });
                }
                return Ok(());
            }
            // The loader has gone; build them here from now on.
            self.originals = None;
        }
        self.build_original_now(account_id, &message_id)
    }

    /// Builds an original on the window thread, as Mail did before the
    /// loader, and shows it.
    fn build_original_now(&mut self, account_id: i64, message_id: &str) -> anyhow::Result<()> {
        let built = originals::build(&self.store, account_id, message_id)?;
        let images = self.images_for(message_id);
        if let Some(built) = &built {
            self.cache.insert(account_id, message_id, built.clone());
        }
        if let Some(open) = &mut self.open {
            open.loading = None;
            open.original = built.map(|built| original_html(built, message_id, images));
        }
        Ok(())
    }

    /// Whether a new page of `message_id` may load remote images: as the
    /// one on screen did, or as Settings say for a message just opened.
    fn images_for(&self, message_id: &str) -> bool {
        self.open
            .as_ref()
            .and_then(|open| open.original.as_ref())
            .filter(|original| original.message_id == message_id)
            .map_or(self.remote_images, |original| original.images)
    }

    /// Shows the originals the loader has built. If it has gone, the one
    /// still waiting is built here, and so is every one after it.
    pub fn receive_originals(&mut self) {
        loop {
            let Some(loader) = &self.originals else {
                return;
            };
            match loader.results.try_recv() {
                Ok(loaded) => self.finish_original(loaded),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.originals = None;
                    self.retry_pending_on_the_spot();
                    return;
                }
            }
        }
    }

    fn retry_pending_on_the_spot(&mut self) {
        let Some(open) = &mut self.open else {
            return;
        };
        let Some(pending) = open.loading.take() else {
            return;
        };
        let account_id = open.account_id;
        if let Err(error) = self.build_original_now(account_id, &pending.message_id) {
            self.notify(format!("Could not show the original: {error:#}"));
        }
        self.request_inline_images();
    }

    /// Shows a built original if it is the one the open thread waits for.
    /// Anything else answers a thread no longer open, or a page since
    /// replaced, and is dropped without being kept.
    fn finish_original(&mut self, loaded: Loaded) {
        let Some(open) = &mut self.open else {
            return;
        };
        let current = open.account_id == loaded.account_id
            && open.loading.as_ref().is_some_and(|pending| {
                pending.generation == loaded.generation && pending.message_id == loaded.message_id
            });
        if !current {
            return;
        }
        open.loading = None;
        match loaded.built {
            Ok(Some(built)) => {
                let images = self.images_for(&loaded.message_id);
                self.cache
                    .insert(loaded.account_id, &loaded.message_id, built.clone());
                if let Some(open) = &mut self.open {
                    open.original = Some(original_html(built, &loaded.message_id, images));
                }
            }
            // It reads as text, as it would have before.
            Ok(None) => {}
            Err(error) => self.notify(format!("Could not show the original: {error}")),
        }
        self.request_inline_images();
    }

    /// Waits for the loader's next answer and handles it. False when none
    /// came in time.
    #[cfg(test)]
    pub(crate) fn receive_one_original(&mut self, timeout: Duration) -> bool {
        let Some(loader) = &self.originals else {
            return false;
        };
        match loader.results.recv_timeout(timeout) {
            Ok(loaded) => {
                self.finish_original(loaded);
                true
            }
            Err(_) => false,
        }
    }

    /// Waits until the open thread's original is no longer loading. False
    /// when it still is after `timeout`.
    #[cfg(test)]
    pub(crate) fn wait_for_original(&mut self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        while self.open.as_ref().is_some_and(OpenThread::loading_original) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() || !self.receive_one_original(left) {
                return false;
            }
        }
        true
    }

    /// Asks Gmail for the pictures the original on screen lacks, once.
    fn request_inline_images(&mut self) {
        let Some(open) = &self.open else {
            return;
        };
        let Some(original) = self.showing_original() else {
            return;
        };
        let key = (open.account_id, original.message_id.clone());
        if original.missing.is_empty() || self.inline_requested.contains(&key) {
            return;
        }
        let Some(account) = self.account(open.account_id) else {
            return;
        };
        self.effects.push(Effect::FetchInlineImages {
            account: account.email.clone(),
            account_id: open.account_id,
            message_id: original.message_id.clone(),
            wanted: original.missing.clone(),
        });
        self.inline_requested.insert(key);
    }
}

mod originals;
#[cfg(test)]
mod tests;
