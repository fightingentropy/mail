//! Thread summaries for the list view, and unread counts for the sidebar.

use std::collections::{BTreeSet, HashMap, HashSet};

use rusqlite::params;

use super::{SearchQuery, Store};
use crate::model::{Account, ThreadSummary};

/// Which threads the list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadFilter {
    /// Threads with a message carrying this label: `INBOX`, `STARRED`,
    /// `Label_42`. Trashed and spam messages count only for `TRASH` and `SPAM`.
    Label(String),
    /// Everything except trash and spam.
    AllMail,
}

/// Columns read for each message when building summaries.
const MESSAGE_COLUMNS: &str = "m.thread_id, m.internal_date, m.subject, m.from_name, m.from_addr,
    m.snippet, (SELECT group_concat(label_id, ' ') FROM message_labels l WHERE l.message_id = m.id)";

const NOT_HIDDEN: &str = "NOT EXISTS (SELECT 1 FROM message_labels h
    WHERE h.message_id = t.id AND h.label_id IN ('TRASH', 'SPAM'))";

impl Store {
    /// Threads matching `filter`, newest first.
    pub fn threads(
        &self,
        account: &Account,
        filter: &ThreadFilter,
    ) -> anyhow::Result<Vec<ThreadSummary>> {
        let (condition, label) = match filter {
            ThreadFilter::Label(id) if id == "TRASH" || id == "SPAM" => (
                "EXISTS (SELECT 1 FROM message_labels l WHERE l.message_id = t.id AND l.label_id = ?2)"
                    .to_owned(),
                Some(id.as_str()),
            ),
            ThreadFilter::Label(id) => (
                format!(
                    "EXISTS (SELECT 1 FROM message_labels l WHERE l.message_id = t.id AND l.label_id = ?2)
                     AND {NOT_HIDDEN}"
                ),
                Some(id.as_str()),
            ),
            // ?2 is always NULL here; it keeps parameter numbers the same.
            ThreadFilter::AllMail => (format!("?2 IS NULL AND {NOT_HIDDEN}"), None),
        };
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages m
             WHERE m.account_id = ?1 AND m.thread_id IN (
                 SELECT t.thread_id FROM messages t WHERE t.account_id = ?1 AND {condition})
             ORDER BY m.thread_id, m.internal_date, m.id"
        );
        let mut statement = self.connection().prepare(&sql)?;
        let rows = statement
            .query_map(params![account.id, label], message_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut threads = summarize(rows, account);
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.last_date));
        Ok(threads)
    }

    /// Threads containing messages that match `query`, in the order of each
    /// thread's best match.
    pub fn search_threads(
        &self,
        account: &Account,
        query: &SearchQuery,
    ) -> anyhow::Result<Vec<ThreadSummary>> {
        let mut seen = HashSet::new();
        let order: Vec<String> = self
            .matching_thread_ids(account.id, query)?
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .collect();

        // One query for every matching thread, then back into match order.
        let ids = serde_json::to_string(&order)?;
        let mut statement = self.connection().prepare_cached(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages m
             WHERE m.account_id = ?1 AND m.thread_id IN (SELECT value FROM json_each(?2))
             ORDER BY m.thread_id, m.internal_date, m.id"
        ))?;
        let rows = statement
            .query_map(params![account.id, ids], message_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut by_id: HashMap<String, ThreadSummary> = summarize(rows, account)
            .into_iter()
            .map(|thread| (thread.thread_id.clone(), thread))
            .collect();
        Ok(order.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// Unread threads per label, for the sidebar.
    pub fn unread_counts(&self, account_id: i64) -> anyhow::Result<HashMap<String, usize>> {
        let mut statement = self.connection().prepare(
            "SELECT l.label_id, count(DISTINCT m.thread_id)
             FROM message_labels u
             JOIN messages m ON m.id = u.message_id
             JOIN message_labels l ON l.message_id = m.id
             WHERE u.label_id = 'UNREAD' AND m.account_id = ?1
             GROUP BY l.label_id",
        )?;
        let counts = statement
            .query_map([account_id], |row| {
                let count: i64 = row.get(1)?;
                Ok((row.get(0)?, count as usize))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(counts)
    }
}

struct MessageRow {
    thread_id: String,
    internal_date: i64,
    subject: String,
    from_name: String,
    from_addr: String,
    snippet: String,
    label_ids: Vec<String>,
}

fn message_row(row: &rusqlite::Row) -> rusqlite::Result<MessageRow> {
    let labels: Option<String> = row.get(6)?;
    Ok(MessageRow {
        thread_id: row.get(0)?,
        internal_date: row.get(1)?,
        subject: row.get(2)?,
        from_name: row.get(3)?,
        from_addr: row.get(4)?,
        snippet: row.get(5)?,
        label_ids: labels
            .unwrap_or_default()
            .split(' ')
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect(),
    })
}

/// Folds messages, grouped by thread and oldest first, into summaries.
fn summarize(rows: Vec<MessageRow>, account: &Account) -> Vec<ThreadSummary> {
    let mut threads: Vec<ThreadSummary> = Vec::new();
    let mut labels: Vec<BTreeSet<String>> = Vec::new();
    for row in rows {
        let starts_thread = threads.last().is_none_or(|t| t.thread_id != row.thread_id);
        if starts_thread {
            threads.push(ThreadSummary {
                account_id: account.id,
                thread_id: row.thread_id.clone(),
                subject: row.subject.clone(),
                ..ThreadSummary::default()
            });
            labels.push(BTreeSet::new());
        }
        let thread = threads.last_mut().expect("a thread was just pushed");
        let sender = sender_name(&row.from_name, &row.from_addr, &account.email);
        if !thread.senders.contains(&sender) {
            thread.senders.push(sender);
        }
        thread.last_date = thread.last_date.max(row.internal_date);
        thread.snippet = row.snippet;
        thread.message_count += 1;
        thread.unread |= row.label_ids.iter().any(|id| id == "UNREAD");
        thread.starred |= row.label_ids.iter().any(|id| id == "STARRED");
        labels
            .last_mut()
            .expect("labels follow threads")
            .extend(row.label_ids);
    }
    for (thread, labels) in threads.iter_mut().zip(labels) {
        thread.label_ids = labels.into_iter().collect();
    }
    threads
}

fn sender_name(name: &str, address: &str, own_address: &str) -> String {
    if address.eq_ignore_ascii_case(own_address) {
        "me".to_owned()
    } else if !name.trim().is_empty() {
        name.trim().to_owned()
    } else {
        address.split('@').next().unwrap_or(address).to_owned()
    }
}
