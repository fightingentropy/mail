//! Brings the local store up to date with Gmail.
//!
//! The first sync lists the last N days and downloads each message. Later
//! syncs replay Gmail's history from the saved history ID, which costs a
//! handful of requests when little has changed. If that history has expired,
//! sync falls back to a full pass.
//!
//! The sync point is saved only after every change is stored, so an
//! interrupted sync simply repeats some work next time.

use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, mpsc},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;

use crate::{
    gmail::{GmailApi, parse_message},
    model::{Account, Label, LabelKind, Message},
    store::Store,
};

#[derive(Clone, Debug)]
pub struct SyncOptions {
    /// How far back the first sync reaches.
    pub days: u32,
    /// Parallel message downloads. Gmail allows about 50 message fetches per
    /// second per user; 8 stays near that without constant throttling.
    pub workers: usize,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            days: 90,
            workers: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncKind {
    Full,
    Incremental,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncReport {
    pub kind: SyncKind,
    pub downloaded: usize,
    pub relabeled: usize,
    pub deleted: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Message IDs found so far while listing.
    Listing {
        found: usize,
    },
    Downloading {
        done: usize,
        total: usize,
    },
}

/// Messages are written in batches: one transaction per batch keeps SQLite
/// fast without holding a long write lock.
const BATCH: usize = 200;

pub fn sync(
    store: &mut Store,
    api: &dyn GmailApi,
    account: &Account,
    options: &SyncOptions,
    progress: &mut dyn FnMut(Progress),
) -> anyhow::Result<SyncReport> {
    if let Some(history_id) = &account.history_id
        && let Some(report) = incremental(store, api, account, history_id, options, progress)?
    {
        return Ok(report);
    }
    full(store, api, account, options, progress)
}

fn full(
    store: &mut Store,
    api: &dyn GmailApi,
    account: &Account,
    options: &SyncOptions,
    progress: &mut dyn FnMut(Progress),
) -> anyhow::Result<SyncReport> {
    // Take the history ID before listing: anything that changes while this
    // sync runs is replayed by the next one.
    let profile = api.profile()?;
    save_labels(store, api, account)?;

    let mut listed = Vec::new();
    let mut page_token = None;
    loop {
        let page = api.message_ids(
            &format!("newer_than:{}d", options.days),
            page_token.as_deref(),
        )?;
        listed.extend(page.messages.into_iter().map(|message| message.id));
        progress(Progress::Listing {
            found: listed.len(),
        });
        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => break,
        }
    }

    // A first sync that was interrupted skips what it already stored. A
    // fallback after expired history downloads everything again, because
    // labels may have changed on messages it already has.
    let known = store.message_ids(account.id)?;
    let resuming_first_sync = account.history_id.is_none();
    let to_download: Vec<String> = listed
        .iter()
        .filter(|id| !(resuming_first_sync && known.contains(*id)))
        .cloned()
        .collect();
    let downloaded = download(store, api, account, to_download, options, progress)?;

    // Messages inside the window that Gmail no longer lists were deleted,
    // trashed or marked as spam while Mail was away. A day of margin keeps
    // messages at the window's edge.
    let listed: HashSet<&str> = listed.iter().map(String::as_str).collect();
    let window_start = now_millis() - (i64::from(options.days) - 1) * 86_400_000;
    let gone: Vec<String> = store
        .message_ids_since(account.id, window_start)?
        .into_iter()
        .filter(|id| !listed.contains(id.as_str()))
        .collect();
    let deleted = store.delete_messages(account.id, gone.iter().map(String::as_str))?;

    store.save_sync_point(account.id, &profile.history_id, now_millis())?;
    Ok(SyncReport {
        kind: SyncKind::Full,
        downloaded,
        relabeled: 0,
        deleted,
    })
}

/// Replays Gmail history. Returns `None` when the history has expired.
fn incremental(
    store: &mut Store,
    api: &dyn GmailApi,
    account: &Account,
    start_history_id: &str,
    options: &SyncOptions,
    progress: &mut dyn FnMut(Progress),
) -> anyhow::Result<Option<SyncReport>> {
    let mut added: HashSet<String> = HashSet::new();
    let mut deleted: HashSet<String> = HashSet::new();
    let mut labels: HashMap<String, Vec<String>> = HashMap::new();
    let mut latest_history_id;
    let mut page_token = None;

    loop {
        let Some(page) = api.history(start_history_id, page_token.as_deref())? else {
            return Ok(None);
        };
        latest_history_id = page.history_id;
        for record in page.history {
            for entry in record.messages_added {
                deleted.remove(&entry.message.id);
                added.insert(entry.message.id);
            }
            for entry in record.messages_deleted {
                added.remove(&entry.message.id);
                labels.remove(&entry.message.id);
                deleted.insert(entry.message.id);
            }
            for entry in record.labels_added.into_iter().chain(record.labels_removed) {
                labels.insert(entry.message.id, entry.message.label_ids);
            }
        }
        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => break,
        }
    }

    save_labels(store, api, account)?;
    // New messages arrive with their current labels, so only relabel the rest.
    labels.retain(|id, _| !added.contains(id));
    let downloaded = download(
        store,
        api,
        account,
        added.into_iter().collect(),
        options,
        progress,
    )?;
    let relabeled = store.set_labels(
        account.id,
        labels
            .iter()
            .map(|(id, label_ids)| (id.as_str(), label_ids.as_slice())),
    )?;
    let deleted = store.delete_messages(account.id, deleted.iter().map(String::as_str))?;

    store.save_sync_point(account.id, &latest_history_id, now_millis())?;
    Ok(Some(SyncReport {
        kind: SyncKind::Incremental,
        downloaded,
        relabeled,
        deleted,
    }))
}

fn save_labels(store: &mut Store, api: &dyn GmailApi, account: &Account) -> anyhow::Result<()> {
    let labels: Vec<Label> = api
        .labels()?
        .into_iter()
        .map(|label| Label {
            kind: if label.kind == "user" {
                LabelKind::User
            } else {
                LabelKind::System
            },
            id: label.id,
            name: label.name,
        })
        .collect();
    store.replace_labels(account.id, &labels)
}

/// Downloads messages on worker threads and stores them in batches on this
/// one. Parsing happens on the workers too. Returns how many were stored;
/// messages deleted before they could be fetched are skipped.
fn download(
    store: &mut Store,
    api: &dyn GmailApi,
    account: &Account,
    ids: Vec<String>,
    options: &SyncOptions,
    progress: &mut dyn FnMut(Progress),
) -> anyhow::Result<usize> {
    let total = ids.len();
    if total == 0 {
        return Ok(0);
    }
    let queue = Mutex::new(ids.into_iter());
    let (sender, receiver) = mpsc::sync_channel::<anyhow::Result<Option<Message>>>(BATCH);

    thread::scope(|scope| {
        for _ in 0..options.workers.clamp(1, 32) {
            let sender = sender.clone();
            let queue = &queue;
            scope.spawn(move || {
                loop {
                    let Some(id) = queue.lock().unwrap().next() else {
                        break;
                    };
                    let result = api
                        .message(&id)
                        .with_context(|| format!("Could not download message {id}"))
                        .map(|found| found.map(parse_message));
                    // A closed channel means the main thread stopped on an error.
                    if sender.send(result).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);

        // Returning early drops the receiver, which stops the workers.
        let mut batch = Vec::with_capacity(BATCH);
        let mut stored = 0;
        for (index, result) in receiver.into_iter().enumerate() {
            let done = index + 1;
            if let Some(message) = result? {
                batch.push(message);
            }
            if batch.len() == BATCH {
                store.save_messages(account.id, &batch)?;
                stored += batch.len();
                batch.clear();
            }
            progress(Progress::Downloading { done, total });
        }
        store.save_messages(account.id, &batch)?;
        stored += batch.len();
        Ok(stored)
    })
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
