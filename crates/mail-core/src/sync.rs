//! Brings the local store up to date with Gmail.
//!
//! The first sync lists the last N days and downloads each message. Later
//! syncs replay Gmail's history from the saved history ID, which costs a
//! handful of requests when little has changed. If that history has expired,
//! sync falls back to a full pass.
//!
//! Before pulling, sync pushes triage made in Mail (archive, labels, trash),
//! so Gmail's answer already includes it.
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
    gmail::{GmailApi, Outcome, parse_message},
    model::{Account, Change, Label, LabelKind},
    store::Store,
};

#[derive(Clone, Debug)]
pub struct SyncOptions {
    /// How far back the first sync reaches.
    pub days: u32,
    /// Parallel message downloads. The Gmail client paces requests to the
    /// quota (about 1.6 full messages a second); a few workers keep that pace
    /// despite network latency, and more help if the quota is raised.
    pub workers: usize,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            days: 90,
            workers: 3,
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
    /// Local changes Gmail accepted.
    pub pushed: usize,
    /// Local changes Gmail refused for good and that were dropped.
    pub rejected: usize,
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
    /// Reading the labels of messages already stored, after expired history.
    Checking {
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
    let (pushed, rejected) = push_changes(store, api, account)?;
    let replayed = match &account.history_id {
        Some(history_id) => incremental(store, api, account, history_id, options, progress)?,
        None => None,
    };
    let mut report = match replayed {
        Some(report) => report,
        None => full(store, api, account, options, progress)?,
    };
    // Changes made in the window while this sync ran are not in Gmail's
    // view yet; keep showing them.
    store.reapply_pending(account.id)?;
    report.pushed = pushed;
    report.rejected = rejected;
    Ok(report)
}

/// Sends queued triage to Gmail, oldest first. A network or quota failure
/// stops here and leaves the rest queued; a change Gmail refuses for good is
/// dropped. Returns how many were accepted and how many refused.
pub fn push_changes(
    store: &mut Store,
    api: &dyn GmailApi,
    account: &Account,
) -> anyhow::Result<(usize, usize)> {
    let mut accepted = 0;
    let mut rejected = 0;
    for pending in store.pending_changes(account.id)? {
        let thread_id = pending.thread_id.as_str();
        let outcome = match &pending.change {
            Change::Labels { add, remove } => api.modify_thread(thread_id, add, remove),
            Change::Trash => api.trash_thread(thread_id),
            Change::Untrash => api.untrash_thread(thread_id),
        }
        .with_context(|| format!("Could not send a change for thread {thread_id}"))?;
        match outcome {
            Outcome::Done | Outcome::Gone => accepted += 1,
            Outcome::Rejected(_) => rejected += 1,
        }
        store.finish_change(pending.id)?;
    }
    Ok((accepted, rejected))
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

    // Gmail messages never change except for their labels, so only new ones
    // are downloaded. A first sync resuming after an interruption already
    // has current copies of the rest. After expired history their labels
    // may have changed, and reading just the labels costs a third of a
    // download.
    let known = store.message_ids(account.id)?;
    let (stored, fresh): (Vec<String>, Vec<String>) =
        listed.iter().cloned().partition(|id| known.contains(id));
    let downloaded = download(store, api, account, fresh, options, progress)?;
    let relabeled = match account.history_id {
        Some(_) => refresh_labels(store, api, account, stored, options, progress)?,
        None => 0,
    };

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
        relabeled,
        deleted,
        pushed: 0,
        rejected: 0,
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
        pushed: 0,
        rejected: 0,
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
    let mut batch = Vec::with_capacity(BATCH);
    let mut stored = 0;
    let mut done = 0;
    let fetched = in_parallel(
        ids,
        options.workers,
        &|id| {
            api.message(id)
                .with_context(|| format!("Could not download message {id}"))
                .map(|found| found.map(parse_message))
        },
        &mut |found| {
            done += 1;
            if let Some(message) = found {
                batch.push(message);
            }
            if batch.len() == BATCH {
                store.save_messages(account.id, &batch)?;
                stored += batch.len();
                batch.clear();
            }
            progress(Progress::Downloading { done, total });
            Ok(())
        },
    );
    // Keep what already arrived even after a failure: an interrupted first
    // sync resumes from the stored messages.
    store.save_messages(account.id, &batch)?;
    stored += batch.len();
    fetched.map(|()| stored)
}

/// Reads the current labels of messages Mail already has and stores them.
/// Returns how many were updated.
fn refresh_labels(
    store: &mut Store,
    api: &dyn GmailApi,
    account: &Account,
    ids: Vec<String>,
    options: &SyncOptions,
    progress: &mut dyn FnMut(Progress),
) -> anyhow::Result<usize> {
    let total = ids.len();
    let mut changes: Vec<(String, Vec<String>)> = Vec::with_capacity(total);
    let mut done = 0;
    in_parallel(
        ids,
        options.workers,
        &|id| {
            api.message_labels(id)
                .with_context(|| format!("Could not read the labels of message {id}"))
                .map(|labels| labels.map(|labels| (id.to_owned(), labels)))
        },
        &mut |found| {
            done += 1;
            // Gone since listing: the deletion pass below takes care of it.
            changes.extend(found);
            progress(Progress::Checking { done, total });
            Ok(())
        },
    )?;
    store.set_labels(
        account.id,
        changes
            .iter()
            .map(|(id, labels)| (id.as_str(), labels.as_slice())),
    )
}

/// Runs `fetch` for each ID on worker threads and gives each result to
/// `handle` on this thread, as they finish. Parsing and network waits happen
/// on the workers; the store is only touched here. Stops at the first error:
/// dropping the channel then stops the workers.
fn in_parallel<T: Send>(
    ids: Vec<String>,
    workers: usize,
    fetch: &(dyn Fn(&str) -> anyhow::Result<T> + Sync),
    handle: &mut dyn FnMut(T) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let queue = Mutex::new(ids.into_iter());
    let (sender, receiver) = mpsc::sync_channel::<anyhow::Result<T>>(BATCH);
    thread::scope(|scope| {
        for _ in 0..workers.clamp(1, 32) {
            let sender = sender.clone();
            let queue = &queue;
            scope.spawn(move || {
                loop {
                    let Some(id) = queue.lock().unwrap().next() else {
                        break;
                    };
                    // A closed channel means this thread's caller stopped on an error.
                    if sender.send(fetch(&id)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        for result in receiver {
            handle(result?)?;
        }
        Ok(())
    })
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests;
