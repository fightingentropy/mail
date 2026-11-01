//! Runs the real sync logic against an in-memory mailbox.

use std::{collections::BTreeMap, sync::Mutex};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;

use super::*;
use crate::gmail::{
    ApiLabel, ApiMessage, HistoryMessage, HistoryPage, HistoryRecord, IdPage, MessageRef, Profile,
};
use crate::store::SearchQuery;

const DAY: i64 = 86_400_000;

struct FakeGmail {
    state: Mutex<State>,
}

struct State {
    history_id: u64,
    messages: BTreeMap<String, ApiMessage>,
    /// Changes since `history_start`; `None` once Gmail has expired them.
    history: Option<Vec<HistoryRecord>>,
    fetches: usize,
    /// A message whose download fails, like a quota error that outlasts retries.
    failing: Option<String>,
}

impl FakeGmail {
    fn new(messages: Vec<ApiMessage>) -> Self {
        Self {
            state: Mutex::new(State {
                history_id: 100,
                messages: messages.into_iter().map(|m| (m.id.clone(), m)).collect(),
                history: Some(Vec::new()),
                fetches: 0,
                failing: None,
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

    fn fetches(&self) -> usize {
        self.state.lock().unwrap().fetches
    }

    fn fail_on(&self, id: Option<&str>) {
        self.state.lock().unwrap().failing = id.map(str::to_owned);
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

    fn history(
        &self,
        _start: &str,
        _page_token: Option<&str>,
    ) -> anyhow::Result<Option<HistoryPage>> {
        let state = self.state.lock().unwrap();
        Ok(state.history.clone().map(|history| HistoryPage {
            history,
            next_page_token: None,
            history_id: state.history_id.to_string(),
        }))
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
            deleted: 0
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
            deleted: 1
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
    gmail.expire_history();

    let report = run(&mut store, &gmail);
    assert_eq!(report.kind, SyncKind::Full);
    assert_eq!(report.deleted, 1);
    assert!(search(&store, "receipt").is_empty());
    assert_eq!(labels(&store, "c"), ["Label_1"]);
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
