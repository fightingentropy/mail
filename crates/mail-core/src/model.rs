//! Mail as Mail stores it, independent of the Gmail wire format.

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
    /// Plain text body. Converted from HTML when the message has no text part.
    pub body_text: String,
    pub body_html: Option<String>,
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
