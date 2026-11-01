//! Mail as Mail stores it, independent of the Gmail wire format.

/// A conversation as the thread list shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadSummary {
    /// The account the thread belongs to.
    pub account_id: i64,
    pub thread_id: String,
    /// Unix milliseconds of the newest message.
    pub last_date: i64,
    /// The first message's subject.
    pub subject: String,
    /// Distinct senders, oldest first; your own address shows as "me".
    pub senders: Vec<String>,
    /// The newest message's snippet.
    pub snippet: String,
    pub message_count: usize,
    pub unread: bool,
    pub starred: bool,
    /// Every label on any message in the thread.
    pub label_ids: Vec<String>,
}

/// One triage action on a whole thread. Mail applies it locally at once and
/// sends it to Gmail on the next sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Labels {
        add: Vec<String>,
        remove: Vec<String>,
    },
    Trash,
    Untrash,
}

impl Change {
    pub fn archive() -> Self {
        Self::remove("INBOX")
    }

    pub fn add(label_id: &str) -> Self {
        Self::Labels {
            add: vec![label_id.to_owned()],
            remove: Vec::new(),
        }
    }

    pub fn remove(label_id: &str) -> Self {
        Self::Labels {
            add: Vec::new(),
            remove: vec![label_id.to_owned()],
        }
    }

    /// The change that undoes this one.
    pub fn inverse(&self) -> Self {
        match self {
            Change::Labels { add, remove } => Change::Labels {
                add: remove.clone(),
                remove: add.clone(),
            },
            Change::Trash => Change::Untrash,
            Change::Untrash => Change::Trash,
        }
    }
}

/// A change waiting to be sent to Gmail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingChange {
    pub id: i64,
    pub thread_id: String,
    pub change: Change,
}

/// One Gmail account. Version 1 supports a single account per database.
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub id: i64,
    pub email: String,
    /// Gmail history ID reached by the last completed sync. `None` until the
    /// first full sync finishes, so an interrupted first sync resumes.
    pub history_id: Option<String>,
    /// Unix milliseconds of the last completed sync.
    pub synced_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LabelKind {
    /// Built into Gmail: INBOX, UNREAD, STARRED, SENT, TRASH, ...
    System,
    /// Created by the user. The ID looks like `Label_42`.
    User,
}

impl LabelKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LabelKind::System => "system",
            LabelKind::User => "user",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub id: String,
    pub name: String,
    pub kind: LabelKind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    /// Gmail's message ID. Stable for the life of the message.
    pub gmail_id: String,
    pub thread_id: String,
    /// When Gmail received the message, in Unix milliseconds. More reliable
    /// for sorting than the sender's Date header.
    pub internal_date: i64,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    /// The To and Cc headers as written, e.g. `Ana <ana@example.com>, bo@example.com`.
    pub to: String,
    pub cc: String,
    pub snippet: String,
    /// Plain text body: the text part, or text made from the HTML when there
    /// is none or it is a poor copy of it.
    pub body_text: String,
    /// Words in `body_text` that link somewhere, from the HTML.
    pub body_links: Vec<BodyLink>,
    /// The HTML version, when parsed from Gmail. Messages read back from the
    /// store leave it empty and set `has_html`; `Store::message_html` loads it.
    pub body_html: Option<String>,
    pub has_html: bool,
    /// Stored from headers only during a big first sync; the body follows
    /// in a later pass.
    pub body_pending: bool,
    /// RFC 5322 threading headers, needed to write replies.
    pub message_id_header: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub size_estimate: i64,
    pub label_ids: Vec<String>,
    pub attachments: Vec<Attachment>,
}

impl Message {
    pub fn has_label(&self, label_id: &str) -> bool {
        self.label_ids.iter().any(|id| id == label_id)
    }
}

/// Words in a body that link somewhere, as an HTML "Track your order"
/// button does. Byte offsets into `body_text`; only web and email links.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodyLink {
    pub start: usize,
    pub end: usize,
    pub url: String,
}

/// Attachment metadata. Contents are downloaded only when opened.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attachment {
    /// MIME part path from Gmail, e.g. `1.2`.
    pub part_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    /// Gmail may issue a new ID on every fetch, so re-read the message if a
    /// download with this one fails.
    pub gmail_attachment_id: Option<String>,
}
