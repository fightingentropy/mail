//! Runs the real sync logic against an in-memory mailbox.

use std::{collections::BTreeMap, sync::Mutex};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;

use super::*;
use crate::gmail::{
    ApiLabel, ApiMessage, HistoryMessage, HistoryPage, HistoryRecord, IdPage, MessageRef, Outcome,
    Profile,
};
use crate::store::SearchQuery;

const DAY: i64 = 86_400_000;

pub(crate) struct FakeGmail {
    state: Mutex<State>,
}

struct State {
    history_id: u64,
    messages: BTreeMap<String, ApiMessage>,
    /// Changes since `history_start`; `None` once Gmail has expired them.
    history: Option<Vec<HistoryRecord>>,
    fetches: usize,
    /// Attachment contents by the attachment ID Gmail currently accepts.
    attachments: BTreeMap<String, Vec<u8>>,
    /// A message whose download fails, like a quota error that outlasts retries.
    failing: Option<String>,
    /// Every call that changes the mailbox, in order, plus "history" and
    /// attachment reads.
    calls: Vec<String>,
    /// When set, thread changes fail as if the network were down.
    offline: bool,
    /// Saved drafts by ID: the raw RFC 5322 message and its thread.
    drafts: BTreeMap<String, (Vec<u8>, Option<String>)>,
    drafts_created: usize,
    /// Label-only reads, as opposed to full downloads in `fetches`.
    label_fetches: usize,
}

impl FakeGmail {
    pub(crate) fn new(messages: Vec<ApiMessage>) -> Self {
        Self {
            state: Mutex::new(State {
                history_id: 100,
                messages: messages.into_iter().map(|m| (m.id.clone(), m)).collect(),
                history: Some(Vec::new()),
                fetches: 0,
                attachments: BTreeMap::new(),
                failing: None,
                calls: Vec::new(),
                offline: false,
                drafts: BTreeMap::new(),
                drafts_created: 0,
                label_fetches: 0,
            }),
        }
    }

    fn receive(&self, message: ApiMessage) {
        let mut state = self.state.lock().unwrap();
        state.history_id += 1;
        record(&mut state, |record| {
            record.messages_added.push(entry(&message))
        });
        state.messages.insert(message.id.clone(), message);
    }

    fn relabel(&self, id: &str, label_ids: &[&str]) {
        let mut state = self.state.lock().unwrap();
        state.history_id += 1;
        let message = state.messages.get_mut(id).unwrap();
        message.label_ids = label_ids.iter().map(|l| l.to_string()).collect();
        let changed = entry(message);
        record(&mut state, |record| record.labels_removed.push(changed));
    }

    fn delete(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        state.history_id += 1;
        let message = state.messages.remove(id).unwrap();
        record(&mut state, |record| {
            record.messages_deleted.push(entry(&message))
        });
    }

    fn expire_history(&self) {
        self.state.lock().unwrap().history = None;
    }

    pub(crate) fn fetches(&self) -> usize {
        self.state.lock().unwrap().fetches
    }

    fn label_fetches(&self) -> usize {
        self.state.lock().unwrap().label_fetches
    }

    pub(crate) fn add_attachment(&self, attachment_id: &str, contents: &[u8]) {
        let mut state = self.state.lock().unwrap();
        state
            .attachments
            .insert(attachment_id.to_owned(), contents.to_vec());
    }

    /// Issues a new ID for an attachment, as Gmail may, and stops accepting
    /// the old one.
    pub(crate) fn rotate_attachment(&self, old_id: &str, new_id: &str) {
        let mut state = self.state.lock().unwrap();
        let contents = state.attachments.remove(old_id).unwrap();
        state.attachments.insert(new_id.to_owned(), contents);
        for message in state.messages.values_mut() {
            for part in message.payload.iter_mut().flat_map(|root| &mut root.parts) {
                if let Some(body) = &mut part.body
                    && body.attachment_id.as_deref() == Some(old_id)
                {
                    body.attachment_id = Some(new_id.to_owned());
                }
            }
        }
    }

    fn fail_on(&self, id: Option<&str>) {
        self.state.lock().unwrap().failing = id.map(str::to_owned);
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }

    pub(crate) fn set_offline(&self, offline: bool) {
        self.state.lock().unwrap().offline = offline;
    }

    fn labels_of(&self, id: &str) -> Vec<String> {
        self.state.lock().unwrap().messages[id].label_ids.clone()
    }

    /// Applies a thread change the way Gmail would, recording it in history.
    fn change_thread(
        &self,
        call: String,
        thread_id: &str,
        edit: impl Fn(&mut Vec<String>),
    ) -> anyhow::Result<Outcome> {
        let mut state = self.state.lock().unwrap();
        if state.offline {
            anyhow::bail!("network unreachable");
        }
        state.calls.push(call);
        let ids: Vec<String> = state
            .messages
            .values()
            .filter(|m| m.thread_id == thread_id)
            .map(|m| m.id.clone())
            .collect();
        if ids.is_empty() {
            return Ok(Outcome::Gone);
        }
        for id in ids {
            state.history_id += 1;
            let message = state.messages.get_mut(&id).unwrap();
            edit(&mut message.label_ids);
            let changed = entry(message);
            record(&mut state, |record| record.labels_added.push(changed));
        }
        Ok(Outcome::Done)
    }
}

fn record(state: &mut State, change: impl FnOnce(&mut HistoryRecord)) {
    if let Some(history) = &mut state.history {
        let mut record = HistoryRecord::default();
        change(&mut record);
        history.push(record);
    }
}

fn entry(message: &ApiMessage) -> HistoryMessage {
    HistoryMessage {
        message: MessageRef {
            id: message.id.clone(),
            thread_id: message.thread_id.clone(),
            label_ids: message.label_ids.clone(),
        },
    }
}

impl GmailApi for FakeGmail {
    fn profile(&self) -> anyhow::Result<Profile> {
        Ok(Profile {
            email_address: "me@example.com".into(),
            history_id: self.state.lock().unwrap().history_id.to_string(),
            messages_total: 0,
        })
    }

    fn labels(&self) -> anyhow::Result<Vec<ApiLabel>> {
        Ok(vec![
            ApiLabel {
                id: "INBOX".into(),
                name: "INBOX".into(),
                kind: "system".into(),
            },
            ApiLabel {
                id: "Label_1".into(),
                name: "Receipts".into(),
                kind: "user".into(),
            },
        ])
    }

    /// Pages of two, to exercise paging.
    fn message_ids(&self, query: &str, page_token: Option<&str>) -> anyhow::Result<IdPage> {
        assert_eq!(query, "newer_than:90d");
        let state = self.state.lock().unwrap();
        let start: usize = page_token.map_or(0, |token| token.parse().unwrap());
        let ids: Vec<&String> = state.messages.keys().collect();
        Ok(IdPage {
            messages: ids
                .iter()
                .skip(start)
                .take(2)
                .map(|id| MessageRef {
                    id: id.to_string(),
                    ..MessageRef::default()
                })
                .collect(),
            next_page_token: (start + 2 < ids.len()).then(|| (start + 2).to_string()),
        })
    }

    fn message(&self, id: &str) -> anyhow::Result<Option<ApiMessage>> {
        let mut state = self.state.lock().unwrap();
        state.fetches += 1;
        if state.failing.as_deref() == Some(id) {
            anyhow::bail!("quota exceeded");
        }
        Ok(state.messages.get(id).cloned())
    }

    fn attachment(&self, message_id: &str, attachment_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let mut state = self.state.lock().unwrap();
        if state.offline {
            anyhow::bail!("network unreachable");
        }
        state.calls.push(format!("attachment {attachment_id}"));
        if !state.messages.contains_key(message_id) {
            return Ok(None);
        }
        Ok(state.attachments.get(attachment_id).cloned())
    }

    fn message_labels(&self, id: &str) -> anyhow::Result<Option<Vec<String>>> {
        let mut state = self.state.lock().unwrap();
        state.label_fetches += 1;
        Ok(state
            .messages
            .get(id)
            .map(|message| message.label_ids.clone()))
    }

    fn history(
        &self,
        _start: &str,
        _page_token: Option<&str>,
    ) -> anyhow::Result<Option<HistoryPage>> {
        let mut state = self.state.lock().unwrap();
        state.calls.push("history".into());
        Ok(state.history.clone().map(|history| HistoryPage {
            history,
            next_page_token: None,
            history_id: state.history_id.to_string(),
        }))
    }

    fn modify_thread(
        &self,
        thread_id: &str,
        add: &[String],
        remove: &[String],
    ) -> anyhow::Result<Outcome> {
        if add.iter().any(|label| label == "Label_deleted") {
            return Ok(Outcome::Rejected("Invalid label".into()));
        }
        self.change_thread(
            format!("modify {thread_id} +{add:?} -{remove:?}"),
            thread_id,
            |labels| {
                labels.retain(|label| !remove.contains(label));
                for label in add {
                    if !labels.contains(label) {
                        labels.push(label.clone());
                    }
                }
            },
        )
    }

    fn trash_thread(&self, thread_id: &str) -> anyhow::Result<Outcome> {
        self.change_thread(format!("trash {thread_id}"), thread_id, |labels| {
            labels.retain(|label| label != "INBOX");
            labels.push("TRASH".into());
        })
    }

    fn untrash_thread(&self, thread_id: &str) -> anyhow::Result<Outcome> {
        self.change_thread(format!("untrash {thread_id}"), thread_id, |labels| {
            labels.retain(|label| label != "TRASH");
        })
    }

    fn create_draft(&self, raw: &[u8], thread_id: Option<&str>) -> anyhow::Result<String> {
        let mut state = self.state.lock().unwrap();
        state.drafts_created += 1;
        let id = format!("draft-{}", state.drafts_created);
        state.calls.push(format!("create_draft {id}"));
        state
            .drafts
            .insert(id.clone(), (raw.to_vec(), thread_id.map(str::to_owned)));
        Ok(id)
    }

    fn update_draft(
        &self,
        draft_id: &str,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> anyhow::Result<Outcome> {
        let mut state = self.state.lock().unwrap();
        if !state.drafts.contains_key(draft_id) {
            return Ok(Outcome::Gone);
        }
        state.calls.push(format!("update_draft {draft_id}"));
        state.drafts.insert(
            draft_id.to_owned(),
            (raw.to_vec(), thread_id.map(str::to_owned)),
        );
        Ok(Outcome::Done)
    }

    fn delete_draft(&self, draft_id: &str) -> anyhow::Result<Outcome> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(format!("delete_draft {draft_id}"));
        Ok(match state.drafts.remove(draft_id) {
            Some(_) => Outcome::Done,
            None => Outcome::Gone,
        })
    }
}

impl FakeGmail {
    /// The raw message and thread of a saved draft.
    pub(crate) fn draft(&self, id: &str) -> Option<(Vec<u8>, Option<String>)> {
        self.state.lock().unwrap().drafts.get(id).cloned()
    }
}

fn mail(id: &str, days_ago: i64, subject: &str, body: &str, labels: &[&str]) -> ApiMessage {
    serde_json::from_value(json!({
        "id": id,
        "threadId": format!("thread-{id}"),
        "labelIds": labels,
        "internalDate": (now_millis() - days_ago * DAY).to_string(),
        "payload": {
            "mimeType": "text/plain",
            "headers": [
                {"name": "From", "value": "Ana <ana@example.org>"},
                {"name": "Subject", "value": subject}
            ],
            "body": {"data": URL_SAFE_NO_PAD.encode(body)}
        }
    }))
    .unwrap()
}

fn mailbox() -> FakeGmail {
    FakeGmail::new(vec![
        mail(
            "a",
            1,
            "Dinner on Friday",
            "Shall we book the Thai place?",
            &["INBOX", "UNREAD"],
        ),
        mail(
            "b",
            5,
            "Your receipt",
            "Order 7731, total £18",
            &["INBOX", "Label_1"],
        ),
        mail(
            "c",
            30,
            "Flight confirmation",
            "LHR to LIS, seat 14C",
            &["INBOX"],
        ),
        mail("d", 60, "Weekly digest", "Top stories this week", &[]),
        mail("e", 80, "Old thread", "Archived long ago", &["Label_1"]),
    ])
}

fn run(store: &mut Store, gmail: &FakeGmail) -> SyncReport {
    let account = store.add_account("me@example.com").unwrap();
    let options = SyncOptions {
        days: 90,
        workers: 3,
    };
    sync(store, gmail, &account, &options, &mut |_| {}).unwrap()
}

fn search(store: &Store, text: &str) -> Vec<String> {
    let account = store.account().unwrap().unwrap();
    let query = SearchQuery {
        text: text.into(),
        ..SearchQuery::default()
    };
    store
        .search(account.id, &query)
        .unwrap()
        .into_iter()
        .map(|hit| hit.gmail_id)
        .collect()
}

fn labels(store: &Store, id: &str) -> Vec<String> {
    let account = store.account().unwrap().unwrap();
    store.message(account.id, id).unwrap().unwrap().label_ids
}

#[test]
fn first_sync_downloads_the_window_and_records_where_it_stopped() {
    let gmail = mailbox();
    let mut store = Store::in_memory();
    let report = run(&mut store, &gmail);

    assert_eq!(
        report,
        SyncReport {
            kind: SyncKind::Full,
            downloaded: 5,
            relabeled: 0,
            deleted: 0,
            pushed: 0,
            rejected: 0,
        }
    );
    let account = store.account().unwrap().unwrap();
    assert_eq!(account.history_id.as_deref(), Some("100"));
    assert!(account.synced_at.is_some());
    assert_eq!(store.labels(account.id).unwrap().len(), 2);
    assert_eq!(search(&store, "seat 14c"), ["c"]);
    // The tokenizer splits on symbols, so "£18" finds "total £18".
    assert_eq!(search(&store, "£18"), ["b"]);
    assert_eq!(search(&store, "7731"), ["b"]);
}

#[test]
fn later_syncs_replay_history() {
    let gmail = mailbox();
    let mut store = Store::in_memory();
    run(&mut store, &gmail);
    let fetches_after_first_sync = gmail.fetches();

    gmail.receive(mail(
        "f",
        0,
        "Tickets",
        "Gate opens at 18:30",
        &["INBOX", "UNREAD"],
    ));
    gmail.relabel("a", &["UNREAD"]); // archived
    gmail.delete("d");

    let report = run(&mut store, &gmail);
    assert_eq!(
        report,
        SyncReport {
            kind: SyncKind::Incremental,
            downloaded: 1,
            relabeled: 1,
            deleted: 1,
            pushed: 0,
            rejected: 0,
        }
    );
    assert_eq!(
        gmail.fetches() - fetches_after_first_sync,
        1,
        "only the new message is downloaded"
    );
    assert_eq!(search(&store, "gate"), ["f"]);
    assert_eq!(labels(&store, "a"), ["UNREAD"]);
    assert!(search(&store, "digest").is_empty());
    assert_eq!(
        store.account().unwrap().unwrap().history_id.as_deref(),
        Some("103")
    );
}

#[test]
fn a_new_message_relabeled_before_sync_keeps_its_latest_labels() {
    let gmail = mailbox();
    let mut store = Store::in_memory();
    run(&mut store, &gmail);

    gmail.receive(mail("f", 0, "Tickets", "Gate opens", &["INBOX", "UNREAD"]));
    gmail.relabel("f", &["INBOX"]);
    run(&mut store, &gmail);
    assert_eq!(labels(&store, "f"), ["INBOX"]);
}

#[test]
fn expired_history_falls_back_to_a_full_sync() {
    let gmail = mailbox();
    let mut store = Store::in_memory();
    run(&mut store, &gmail);

    gmail.delete("b");
    gmail.relabel("c", &["Label_1"]);
    gmail.receive(mail("f", 0, "Tickets", "Gate opens", &["INBOX"]));
    gmail.expire_history();
    let full_downloads = gmail.fetches();

    let report = run(&mut store, &gmail);
    assert_eq!(report.kind, SyncKind::Full);
    assert_eq!(report.deleted, 1);
    assert!(search(&store, "receipt").is_empty());
    assert_eq!(labels(&store, "c"), ["Label_1"]);
    assert_eq!(search(&store, "gate"), ["f"]);
    // Only the new message is downloaded in full; the four Mail already has
    // get a label-only read at a third of the quota cost.
    assert_eq!(gmail.fetches() - full_downloads, 1);
    assert_eq!(gmail.label_fetches(), 4);
    assert_eq!(report.relabeled, 4);
}

#[test]
fn an_interrupted_first_sync_resumes() {
    let gmail = mailbox();
    let mut store = Store::in_memory();
    let account = store.add_account("me@example.com").unwrap();
    store
        .save_messages(
            account.id,
            &[parse_message(mail("a", 1, "Dinner", "Thai", &["INBOX"]))],
        )
        .unwrap();

    let report = run(&mut store, &gmail);
    assert_eq!(report.downloaded, 4);
    assert_eq!(gmail.fetches(), 4);
}

#[test]
fn a_failed_first_sync_keeps_what_it_downloaded() {
    let gmail = mailbox();
    gmail.fail_on(Some("e"));
    let mut store = Store::in_memory();
    let account = store.add_account("me@example.com").unwrap();
    // One worker downloads in listing order, so "e" fails last.
    let options = SyncOptions {
        days: 90,
        workers: 1,
    };
    assert!(sync(&mut store, &gmail, &account, &options, &mut |_| {}).is_err());
    assert_eq!(store.stats(account.id).unwrap().messages, 4);
    assert!(store.account().unwrap().unwrap().history_id.is_none());

    gmail.fail_on(None);
    let fetches_before = gmail.fetches();
    let report = run(&mut store, &gmail);
    assert_eq!(report.downloaded, 1);
    assert_eq!(gmail.fetches() - fetches_before, 1);
}

#[test]
fn reports_progress() {
    let gmail = mailbox();
    let mut store = Store::in_memory();
    let account = store.add_account("me@example.com").unwrap();
    let mut seen = Vec::new();
    sync(
        &mut store,
        &gmail,
        &account,
        &SyncOptions::default(),
        &mut |p| seen.push(p),
    )
    .unwrap();
    assert_eq!(seen.first(), Some(&Progress::Listing { found: 2 }));
    assert!(seen.contains(&Progress::Listing { found: 5 }));
    assert_eq!(
        seen.last(),
        Some(&Progress::Downloading { done: 5, total: 5 })
    );
}

#[test]
fn local_triage_is_pushed_before_pulling() {
    let gmail = mailbox();
    let mut store = Store::in_memory();
    run(&mut store, &gmail);
    let account = store.account().unwrap().unwrap();

    store
        .apply_change(account.id, "thread-a", &Change::archive())
        .unwrap();
    store
        .apply_change(account.id, "thread-b", &Change::Trash)
        .unwrap();
    assert!(
        !labels(&store, "a").contains(&"INBOX".to_string()),
        "applied locally at once"
    );

    let report = run(&mut store, &gmail);
    assert_eq!((report.pushed, report.rejected), (2, 0));
    let calls = gmail.calls();
    let history_at = calls.iter().position(|call| call == "history").unwrap();
    assert!(
        calls[..history_at]
            .iter()
            .any(|call| call.starts_with("modify thread-a"))
    );
    assert!(
        calls[..history_at]
            .iter()
            .any(|call| call == "trash thread-b")
    );
    assert_eq!(gmail.labels_of("a"), ["UNREAD"]);
    assert!(store.pending_changes(account.id).unwrap().is_empty());
    // Gmail's view, read back from history, matches what Mail showed.
    assert_eq!(labels(&store, "a"), ["UNREAD"]);
    assert!(labels(&store, "b").contains(&"TRASH".to_string()));
}

#[test]
fn offline_changes_stay_queued_and_sync_does_not_pull() {
    let gmail = mailbox();
    let mut store = Store::in_memory();
    run(&mut store, &gmail);
    let account = store.account().unwrap().unwrap();
    store
        .apply_change(account.id, "thread-a", &Change::archive())
        .unwrap();

    gmail.set_offline(true);
    let options = SyncOptions::default();
    assert!(sync(&mut store, &gmail, &account, &options, &mut |_| {}).is_err());
    assert_eq!(store.pending_changes(account.id).unwrap().len(), 1);
    assert!(!gmail.calls().contains(&"history".to_string()));

    gmail.set_offline(false);
    assert_eq!(run(&mut store, &gmail).pushed, 1);
    assert!(store.pending_changes(account.id).unwrap().is_empty());
}

#[test]
fn refused_changes_are_dropped_so_the_queue_moves_on() {
    let gmail = mailbox();
    let mut store = Store::in_memory();
    run(&mut store, &gmail);
    let account = store.account().unwrap().unwrap();
    store
        .apply_change(account.id, "thread-a", &Change::add("Label_deleted"))
        .unwrap();
    store
        .apply_change(account.id, "thread-c", &Change::add("STARRED"))
        .unwrap();

    let report = run(&mut store, &gmail);
    assert_eq!((report.pushed, report.rejected), (1, 1));
    assert!(gmail.labels_of("c").contains(&"STARRED".to_string()));
}

#[test]
fn changes_made_during_a_sync_survive_it() {
    let gmail = mailbox();
    let mut store = Store::in_memory();
    run(&mut store, &gmail);
    let account = store.account().unwrap().unwrap();

    // Gmail relabels "a"; meanwhile the window archives it before pushing.
    gmail.relabel("a", &["INBOX", "UNREAD", "STARRED"]);
    store
        .apply_change(account.id, "thread-a", &Change::archive())
        .unwrap();
    gmail.set_offline(true);
    // Simulate the pull half alone: it writes Gmail's labels, then reapplies.
    let replayed = incremental(
        &mut store,
        &gmail,
        &account,
        "100",
        &SyncOptions::default(),
        &mut |_| {},
    )
    .unwrap()
    .unwrap();
    assert_eq!(replayed.relabeled, 1);
    store.reapply_pending(account.id).unwrap();
    assert_eq!(labels(&store, "a"), ["STARRED", "UNREAD"]);
}
