//! The local mail store: one SQLite file per profile.
//!
//! Gmail stays the source of truth. Everything here can be rebuilt by a full
//! sync, which is what makes offline reading and instant search safe.

mod bodies;
mod changes;
mod schema;
mod search;
mod threads;

use std::{collections::HashSet, fs, path::Path, time::Duration};

use anyhow::{Context, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};

use crate::model::{Account, Attachment, BodyLink, Label, LabelKind, Message};
pub use search::{Order, SearchHit, SearchQuery};
pub use threads::ThreadFilter;

pub struct Store {
    connection: Connection,
}

/// Counts for `mail status`.
#[derive(Debug, Default, PartialEq)]
pub struct Stats {
    pub messages: i64,
    pub threads: i64,
    /// Unix milliseconds of the oldest and newest stored message.
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
}

impl Store {
    /// Opens the database for reading and writing, creating and migrating it
    /// as needed. Used by sync and, later, the desktop app.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Could not create {}", parent.display()))?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        schema::migrate(&mut connection)?;
        bodies::compress_moved_html(&mut connection)?;
        bodies::refresh_body_text(&mut connection)?;
        Ok(Self { connection })
    }

    /// Opens a database that must already exist, migrating it if needed. The
    /// reading commands use this, so they never create an empty database.
    pub fn open_existing(path: &Path) -> anyhow::Result<Self> {
        ensure!(
            path.exists(),
            "No mail downloaded yet. Run `mail auth`, then `mail sync`."
        );
        Self::open(path)
    }

    /// Opens an existing database without the ability to change it, not even
    /// to migrate it. The agent CLI uses this, so no agent command can alter
    /// local mail.
    pub fn open_read_only(path: &Path) -> anyhow::Result<Self> {
        ensure!(
            path.exists(),
            "No mail downloaded yet. Run `mail auth`, then `mail sync`."
        );
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "query_only", true)?;
        let version = schema::version(&connection)?;
        ensure!(
            version <= schema::LATEST,
            "This mail database was updated by a newer version of Mail. Update Mail."
        );
        ensure!(
            version == schema::LATEST,
            "The mail database needs an update. Run `mail status` or open Mail once."
        );
        Ok(Self { connection })
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        schema::migrate(&mut connection).unwrap();
        bodies::refresh_body_text(&mut connection).unwrap();
        Self { connection }
    }

    // Accounts

    /// The first account signed in, if `mail auth` has run. The command line
    /// and the agent read this one.
    pub fn account(&self) -> anyhow::Result<Option<Account>> {
        let account = self
            .connection
            .query_row(
                "SELECT id, email, history_id, synced_at FROM accounts ORDER BY id LIMIT 1",
                [],
                account_from_row,
            )
            .optional()?;
        Ok(account)
    }

    /// Every signed-in account, in the order they were added.
    pub fn accounts(&self) -> anyhow::Result<Vec<Account>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, email, history_id, synced_at FROM accounts ORDER BY id")?;
        let accounts = statement
            .query_map([], account_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(accounts)
    }

    /// Adds an account, or returns it if it is already here (in any case).
    pub fn add_account(&self, email: &str) -> anyhow::Result<Account> {
        let existing = self
            .accounts()?
            .into_iter()
            .find(|account| account.email.eq_ignore_ascii_case(email));
        if let Some(existing) = existing {
            return Ok(existing);
        }
        self.connection
            .execute("INSERT INTO accounts (email) VALUES (?1)", [email])?;
        let account = self.connection.query_row(
            "SELECT id, email, history_id, synced_at FROM accounts WHERE email = ?1",
            [email],
            account_from_row,
        )?;
        Ok(account)
    }

    /// The name on the newest mail sent from the account: how you sign your
    /// mail. `None` until some is stored.
    pub fn own_name(&self, account: &Account) -> anyhow::Result<Option<String>> {
        let name = self
            .connection
            .query_row(
                "SELECT from_name FROM messages
                 WHERE account_id = ?1 AND from_addr = ?2 COLLATE NOCASE AND from_name <> ''
                 ORDER BY internal_date DESC LIMIT 1",
                params![account.id, account.email],
                |row| row.get(0),
            )
            .optional()?;
        Ok(name)
    }

    /// Records how far sync got. Call only after every change up to
    /// `history_id` is stored.
    pub fn save_sync_point(
        &self,
        account_id: i64,
        history_id: &str,
        synced_at: i64,
    ) -> anyhow::Result<()> {
        self.connection.execute(
            "UPDATE accounts SET history_id = ?2, synced_at = ?3 WHERE id = ?1",
            params![account_id, history_id, synced_at],
        )?;
        Ok(())
    }

    // Labels

    pub fn replace_labels(&mut self, account_id: i64, labels: &[Label]) -> anyhow::Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM labels WHERE account_id = ?1", [account_id])?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO labels (account_id, id, name, kind) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for label in labels {
                insert.execute(params![
                    account_id,
                    label.id,
                    label.name,
                    label.kind.as_str()
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// System labels first, then the user's, alphabetically.
    pub fn labels(&self, account_id: i64) -> anyhow::Result<Vec<Label>> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, kind FROM labels WHERE account_id = ?1
             ORDER BY kind = 'user', name COLLATE NOCASE",
        )?;
        let labels = statement
            .query_map([account_id], |row| {
                let kind: String = row.get(2)?;
                Ok(Label {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: if kind == "user" {
                        LabelKind::User
                    } else {
                        LabelKind::System
                    },
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(labels)
    }

    /// Finds a label by its ID (`INBOX`, `Label_42`) or its name, ignoring case.
    pub fn resolve_label(
        &self,
        account_id: i64,
        id_or_name: &str,
    ) -> anyhow::Result<Option<String>> {
        let id = self
            .connection
            .query_row(
                "SELECT id FROM labels WHERE account_id = ?1
                 AND (id = ?2 OR name = ?2 COLLATE NOCASE)
                 ORDER BY id = ?2 DESC LIMIT 1",
                params![account_id, id_or_name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(id)
    }

    // Messages

    pub fn message_ids(&self, account_id: i64) -> anyhow::Result<HashSet<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT gmail_id FROM messages WHERE account_id = ?1")?;
        let ids = statement
            .query_map([account_id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// Inserts or replaces messages, with their labels and attachments, in one
    /// transaction.
    pub fn save_messages(&mut self, account_id: i64, messages: &[Message]) -> anyhow::Result<()> {
        let transaction = self.connection.transaction()?;
        {
            let mut upsert = transaction.prepare(
                "INSERT INTO messages (
                    account_id, gmail_id, thread_id, internal_date, subject, from_name,
                    from_addr, to_addrs, cc_addrs, snippet, body_text,
                    message_id_header, in_reply_to, references_header, size_estimate,
                    body_pending, body_links
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                 ON CONFLICT (account_id, gmail_id) DO UPDATE SET
                    thread_id = excluded.thread_id,
                    internal_date = excluded.internal_date,
                    subject = excluded.subject,
                    from_name = excluded.from_name,
                    from_addr = excluded.from_addr,
                    to_addrs = excluded.to_addrs,
                    cc_addrs = excluded.cc_addrs,
                    snippet = excluded.snippet,
                    body_text = excluded.body_text,
                    message_id_header = excluded.message_id_header,
                    in_reply_to = excluded.in_reply_to,
                    references_header = excluded.references_header,
                    size_estimate = excluded.size_estimate,
                    body_pending = excluded.body_pending,
                    body_links = excluded.body_links
                 RETURNING id",
            )?;
            let mut clear_labels =
                transaction.prepare("DELETE FROM message_labels WHERE message_id = ?1")?;
            let mut insert_label = transaction
                .prepare("INSERT INTO message_labels (message_id, label_id) VALUES (?1, ?2)")?;
            let mut clear_attachments =
                transaction.prepare("DELETE FROM attachments WHERE message_id = ?1")?;
            let mut insert_attachment = transaction.prepare(
                "INSERT INTO attachments (message_id, part_id, filename, mime_type, size, gmail_attachment_id, shown_in_html)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;

            for message in messages {
                let row_id: i64 = upsert.query_row(
                    params![
                        account_id,
                        message.gmail_id,
                        message.thread_id,
                        message.internal_date,
                        message.subject,
                        message.from_name,
                        message.from_addr,
                        message.to,
                        message.cc,
                        message.snippet,
                        message.body_text,
                        message.message_id_header,
                        message.in_reply_to,
                        message.references,
                        message.size_estimate,
                        message.body_pending,
                        links_to_json(&message.body_links)?,
                    ],
                    |row| row.get(0),
                )?;
                bodies::save_html(&transaction, row_id, message.body_html.as_deref())?;
                clear_labels.execute([row_id])?;
                for label_id in unique(&message.label_ids) {
                    insert_label.execute(params![row_id, label_id])?;
                }
                clear_attachments.execute([row_id])?;
                for attachment in &message.attachments {
                    insert_attachment.execute(params![
                        row_id,
                        attachment.part_id,
                        attachment.filename,
                        attachment.mime_type,
                        attachment.size,
                        attachment.gmail_attachment_id,
                        attachment.shown_in_html,
                    ])?;
                }
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Replaces the labels of messages already stored. Unknown message IDs are
    /// ignored; returns how many messages changed.
    pub fn set_labels<'a>(
        &mut self,
        account_id: i64,
        changes: impl IntoIterator<Item = (&'a str, &'a [String])>,
    ) -> anyhow::Result<usize> {
        let transaction = self.connection.transaction()?;
        let mut changed = 0;
        {
            let mut find = transaction
                .prepare("SELECT id FROM messages WHERE account_id = ?1 AND gmail_id = ?2")?;
            let mut clear =
                transaction.prepare("DELETE FROM message_labels WHERE message_id = ?1")?;
            let mut insert = transaction
                .prepare("INSERT INTO message_labels (message_id, label_id) VALUES (?1, ?2)")?;
            for (gmail_id, label_ids) in changes {
                let row_id: Option<i64> = find
                    .query_row(params![account_id, gmail_id], |row| row.get(0))
                    .optional()?;
                let Some(row_id) = row_id else { continue };
                clear.execute([row_id])?;
                for label_id in unique(label_ids) {
                    insert.execute(params![row_id, label_id])?;
                }
                changed += 1;
            }
        }
        transaction.commit()?;
        Ok(changed)
    }

    /// Deletes messages by Gmail ID; returns how many were stored.
    pub fn delete_messages<'a>(
        &mut self,
        account_id: i64,
        gmail_ids: impl IntoIterator<Item = &'a str>,
    ) -> anyhow::Result<usize> {
        let transaction = self.connection.transaction()?;
        let mut deleted = 0;
        {
            let mut delete = transaction
                .prepare("DELETE FROM messages WHERE account_id = ?1 AND gmail_id = ?2")?;
            for gmail_id in gmail_ids {
                deleted += delete.execute(params![account_id, gmail_id])?;
            }
        }
        transaction.commit()?;
        Ok(deleted)
    }

    /// Gmail IDs of stored messages received at or after `since` (Unix ms).
    pub fn message_ids_since(&self, account_id: i64, since: i64) -> anyhow::Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT gmail_id FROM messages WHERE account_id = ?1 AND internal_date >= ?2",
        )?;
        let ids = statement
            .query_map(params![account_id, since], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// Gmail IDs of messages stored from headers only, newest first.
    pub fn pending_bodies(&self, account_id: i64, limit: usize) -> anyhow::Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT gmail_id FROM messages WHERE account_id = ?1 AND body_pending = 1
             ORDER BY internal_date DESC LIMIT ?2",
        )?;
        let ids = statement
            .query_map(params![account_id, limit as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    pub fn pending_body_count(&self, account_id: i64) -> anyhow::Result<usize> {
        let count: i64 = self.connection.query_row(
            "SELECT count(*) FROM messages WHERE account_id = ?1 AND body_pending = 1",
            [account_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn message(&self, account_id: i64, gmail_id: &str) -> anyhow::Result<Option<Message>> {
        let found = self
            .connection
            .query_row(
                &format!("{SELECT_MESSAGE} WHERE account_id = ?1 AND gmail_id = ?2"),
                params![account_id, gmail_id],
                message_from_row,
            )
            .optional()?;
        match found {
            Some((row_id, message)) => Ok(Some(self.with_details(row_id, message)?)),
            None => Ok(None),
        }
    }

    /// Every stored message in a thread, oldest first.
    pub fn thread(&self, account_id: i64, thread_id: &str) -> anyhow::Result<Vec<Message>> {
        let mut statement = self.connection.prepare(&format!(
            "{SELECT_MESSAGE} WHERE account_id = ?1 AND thread_id = ?2 ORDER BY internal_date, id"
        ))?;
        let rows = statement
            .query_map(params![account_id, thread_id], message_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(row_id, message)| self.with_details(row_id, message))
            .collect()
    }

    pub fn stats(&self, account_id: i64) -> anyhow::Result<Stats> {
        let stats = self.connection.query_row(
            "SELECT count(*), count(DISTINCT thread_id), min(internal_date), max(internal_date)
             FROM messages WHERE account_id = ?1",
            [account_id],
            |row| {
                Ok(Stats {
                    messages: row.get(0)?,
                    threads: row.get(1)?,
                    oldest: row.get(2)?,
                    newest: row.get(3)?,
                })
            },
        )?;
        Ok(stats)
    }

    fn with_details(&self, row_id: i64, mut message: Message) -> anyhow::Result<Message> {
        let mut labels = self.connection.prepare_cached(
            "SELECT label_id FROM message_labels WHERE message_id = ?1 ORDER BY label_id",
        )?;
        message.label_ids = labels
            .query_map([row_id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;

        let mut attachments = self.connection.prepare_cached(
            "SELECT part_id, filename, mime_type, size, gmail_attachment_id, shown_in_html
             FROM attachments WHERE message_id = ?1 ORDER BY part_id",
        )?;
        message.attachments = attachments
            .query_map([row_id], |row| {
                Ok(Attachment {
                    part_id: row.get(0)?,
                    filename: row.get(1)?,
                    mime_type: row.get(2)?,
                    size: row.get(3)?,
                    gmail_attachment_id: row.get(4)?,
                    shown_in_html: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(message)
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }
}

const SELECT_MESSAGE: &str = "SELECT id, gmail_id, thread_id, internal_date, subject, from_name,
    from_addr, to_addrs, cc_addrs, snippet, body_text,
    EXISTS (SELECT 1 FROM message_bodies b WHERE b.message_id = messages.id), message_id_header,
    in_reply_to, references_header, size_estimate, body_pending, body_links FROM messages";

fn message_from_row(row: &Row) -> rusqlite::Result<(i64, Message)> {
    let message = Message {
        gmail_id: row.get(1)?,
        thread_id: row.get(2)?,
        internal_date: row.get(3)?,
        subject: row.get(4)?,
        from_name: row.get(5)?,
        from_addr: row.get(6)?,
        to: row.get(7)?,
        cc: row.get(8)?,
        snippet: row.get(9)?,
        body_text: row.get(10)?,
        body_links: links_from_json(row.get(17)?),
        // HTML stays in the database until asked for: see `message_html`.
        body_html: None,
        has_html: row.get(11)?,
        message_id_header: row.get(12)?,
        in_reply_to: row.get(13)?,
        references: row.get(14)?,
        size_estimate: row.get(15)?,
        body_pending: row.get(16)?,
        label_ids: Vec::new(),
        attachments: Vec::new(),
    };
    Ok((row.get(0)?, message))
}

/// Links as compact JSON triples, or NULL when there are none.
fn links_to_json(links: &[BodyLink]) -> anyhow::Result<Option<String>> {
    if links.is_empty() {
        return Ok(None);
    }
    let triples: Vec<(usize, usize, &str)> = links
        .iter()
        .map(|link| (link.start, link.end, link.url.as_str()))
        .collect();
    Ok(Some(serde_json::to_string(&triples)?))
}

/// Unreadable JSON gives no links rather than no message.
fn links_from_json(json: Option<String>) -> Vec<BodyLink> {
    let triples: Vec<(usize, usize, String)> = json
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    triples
        .into_iter()
        .map(|(start, end, url)| BodyLink { start, end, url })
        .collect()
}

fn account_from_row(row: &Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get(0)?,
        email: row.get(1)?,
        history_id: row.get(2)?,
        synced_at: row.get(3)?,
    })
}

/// Gmail never repeats a label on a message, but the primary key would reject
/// it if a fake or future API did.
fn unique(label_ids: &[String]) -> impl Iterator<Item = &String> {
    let mut seen = HashSet::new();
    label_ids.iter().filter(move |id| seen.insert(id.as_str()))
}

#[cfg(test)]
mod tests;
