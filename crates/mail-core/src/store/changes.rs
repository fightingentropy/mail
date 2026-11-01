//! Triage made in Mail. Each change updates local labels at once, so the
//! window reacts instantly and works offline, and waits in `pending_changes`
//! until sync sends it to Gmail.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

use super::Store;
use crate::model::{Change, PendingChange};

impl Store {
    /// Applies `change` to every stored message in the thread and queues it
    /// for Gmail.
    pub fn apply_change(
        &mut self,
        account_id: i64,
        thread_id: &str,
        change: &Change,
    ) -> anyhow::Result<()> {
        let (kind, add, remove) = match change {
            Change::Labels { add, remove } => ("labels", add.join(" "), remove.join(" ")),
            Change::Trash => ("trash", String::new(), String::new()),
            Change::Untrash => ("untrash", String::new(), String::new()),
        };
        let transaction = self.connection.transaction()?;
        apply_locally(&transaction, account_id, thread_id, change)?;
        transaction.execute(
            "INSERT INTO pending_changes (account_id, thread_id, kind, add_labels, remove_labels, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![account_id, thread_id, kind, add, remove, now_millis()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Changes not yet accepted by Gmail, oldest first.
    pub fn pending_changes(&self, account_id: i64) -> anyhow::Result<Vec<PendingChange>> {
        let mut statement = self.connection.prepare(
            "SELECT id, thread_id, kind, add_labels, remove_labels FROM pending_changes
             WHERE account_id = ?1 ORDER BY id",
        )?;
        let changes = statement
            .query_map([account_id], |row| {
                let kind: String = row.get(2)?;
                let add: String = row.get(3)?;
                let remove: String = row.get(4)?;
                let change = match kind.as_str() {
                    "trash" => Change::Trash,
                    "untrash" => Change::Untrash,
                    _ => Change::Labels {
                        add: split_labels(&add),
                        remove: split_labels(&remove),
                    },
                };
                Ok(PendingChange {
                    id: row.get(0)?,
                    thread_id: row.get(1)?,
                    change,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(changes)
    }

    /// Removes a change from the queue once Gmail has it.
    pub fn finish_change(&self, id: i64) -> anyhow::Result<()> {
        self.connection
            .execute("DELETE FROM pending_changes WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Re-applies queued changes to local labels. Sync calls this after
    /// writing Gmail's view of the mailbox, which does not include changes
    /// made in the window while sync was running.
    pub fn reapply_pending(&mut self, account_id: i64) -> anyhow::Result<()> {
        let pending = self.pending_changes(account_id)?;
        let transaction = self.connection.transaction()?;
        for change in &pending {
            apply_locally(&transaction, account_id, &change.thread_id, &change.change)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

fn apply_locally(
    connection: &Connection,
    account_id: i64,
    thread_id: &str,
    change: &Change,
) -> rusqlite::Result<()> {
    let (add, remove): (Vec<&str>, Vec<&str>) = match change {
        Change::Labels { add, remove } => (
            add.iter().map(String::as_str).collect(),
            remove.iter().map(String::as_str).collect(),
        ),
        // Gmail drops a trashed thread from the inbox.
        Change::Trash => (vec!["TRASH"], vec!["INBOX"]),
        Change::Untrash => (Vec::new(), vec!["TRASH"]),
    };
    for label_id in remove {
        connection.execute(
            "DELETE FROM message_labels WHERE label_id = ?3 AND message_id IN (
                 SELECT id FROM messages WHERE account_id = ?1 AND thread_id = ?2)",
            params![account_id, thread_id, label_id],
        )?;
    }
    for label_id in add {
        connection.execute(
            "INSERT OR IGNORE INTO message_labels (message_id, label_id)
             SELECT id, ?3 FROM messages WHERE account_id = ?1 AND thread_id = ?2",
            params![account_id, thread_id, label_id],
        )?;
    }
    Ok(())
}

fn split_labels(text: &str) -> Vec<String> {
    text.split(' ')
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect()
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}
