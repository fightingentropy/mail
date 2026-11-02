//! The Gmail REST API: the calls Mail makes and the JSON they return.
//!
//! Sync talks to [`GmailApi`] rather than to HTTP directly, so tests run the
//! real sync logic against an in-memory mailbox. Sending is one call,
//! `send_draft`, used only by the window after you confirm.

pub mod attachments;
mod client;
mod html;
pub mod inline;
mod message;

use serde::Deserialize;

pub use client::{GmailClient, http_agent};
#[cfg(test)]
pub(crate) use message::decode_words;
pub(crate) use message::split_address;
pub use message::{ApiMessage, parse_message};
pub(crate) use {html::to_text as html_text, message::body};

pub trait GmailApi: Sync {
    fn profile(&self) -> anyhow::Result<Profile>;
    fn labels(&self) -> anyhow::Result<Vec<ApiLabel>>;
    /// One page of message IDs matching a Gmail search such as `newer_than:90d`.
    /// Spam and trash are excluded.
    fn message_ids(&self, query: &str, page_token: Option<&str>) -> anyhow::Result<IdPage>;
    /// A full message without attachment contents. `None` if it no longer exists.
    fn message(&self, id: &str) -> anyhow::Result<Option<ApiMessage>>;
    /// A message's headers, snippet and labels without its body, at a third
    /// of the quota cost. `None` if it no longer exists.
    fn message_headers(&self, id: &str) -> anyhow::Result<Option<ApiMessage>>;
    /// Just a message's current labels, at a third of the quota cost of the
    /// full message. `None` if it no longer exists.
    fn message_labels(&self, id: &str) -> anyhow::Result<Option<Vec<String>>>;
    /// An attachment's contents. `None` when Gmail no longer accepts this
    /// attachment ID or the message is gone: Gmail can issue new attachment
    /// IDs, so the caller reads the message again for a fresh one.
    fn attachment(&self, message_id: &str, attachment_id: &str) -> anyhow::Result<Option<Vec<u8>>>;
    /// Changes since `start_history_id`. `None` when Gmail no longer has
    /// history that old (usually after about a week), which needs a full sync.
    fn history(
        &self,
        start_history_id: &str,
        page_token: Option<&str>,
    ) -> anyhow::Result<Option<HistoryPage>>;
    /// Adds and removes labels on every message in a thread: archive, read,
    /// star, label.
    fn modify_thread(
        &self,
        thread_id: &str,
        add: &[String],
        remove: &[String],
    ) -> anyhow::Result<Outcome>;
    fn trash_thread(&self, thread_id: &str) -> anyhow::Result<Outcome>;
    fn untrash_thread(&self, thread_id: &str) -> anyhow::Result<Outcome>;
    /// Saves an RFC 5322 message to Drafts and returns the draft ID.
    fn create_draft(&self, raw: &[u8], thread_id: Option<&str>) -> anyhow::Result<String>;
    /// Replaces a draft's message. `Gone` if the draft was sent or deleted.
    fn update_draft(
        &self,
        draft_id: &str,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> anyhow::Result<Outcome>;
    fn delete_draft(&self, draft_id: &str) -> anyhow::Result<Outcome>;
    /// Sends a saved draft and returns the sent message's ID. Only the window
    /// calls this, after you confirm; agents and the command line can't.
    fn send_draft(&self, draft_id: &str) -> anyhow::Result<String>;
}

/// How Gmail answered a change. Transient failures are errors instead, so
/// the change stays queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// The thread no longer exists; nothing to do.
    Gone,
    /// Gmail refused the change for good, for example a label that was deleted.
    Rejected(String),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub email_address: String,
    pub history_id: String,
    #[serde(default)]
    pub messages_total: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ApiLabel {
    pub id: String,
    pub name: String,
    /// `system` or `user`.
    #[serde(rename = "type", default)]
    pub kind: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdPage {
    #[serde(default)]
    pub messages: Vec<MessageRef>,
    pub next_page_token: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRef {
    pub id: String,
    #[serde(default)]
    pub thread_id: String,
    /// Present in history records, absent in search results.
    #[serde(default)]
    pub label_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    #[serde(default)]
    pub history: Vec<HistoryRecord>,
    pub next_page_token: Option<String>,
    /// The mailbox's current history ID: the next sync starts here.
    pub history_id: String,
}

/// One change to the mailbox. Records arrive oldest first.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    #[serde(default)]
    pub messages_added: Vec<HistoryMessage>,
    #[serde(default)]
    pub messages_deleted: Vec<HistoryMessage>,
    #[serde(default)]
    pub labels_added: Vec<HistoryMessage>,
    #[serde(default)]
    pub labels_removed: Vec<HistoryMessage>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct HistoryMessage {
    /// After a label change, `label_ids` holds the message's complete label
    /// set, so the newest record for a message is all sync needs.
    pub message: MessageRef,
}
