//! Schema migrations, applied in order and tracked with `PRAGMA user_version`.
//!
//! Append new migrations; never edit one that has shipped. Gmail is the source
//! of truth, so a migration may rebuild derived data such as the search index.

use anyhow::{Context, bail};
use rusqlite::Connection;

pub(super) const MIGRATIONS: &[&str] = &[
    // 1. Accounts, labels, messages, attachments and the full-text index.
    r#"
    CREATE TABLE accounts (
        id INTEGER PRIMARY KEY,
        email TEXT NOT NULL UNIQUE,
        -- Where the next incremental sync starts. NULL until a full sync completes.
        history_id TEXT,
        -- Unix milliseconds of the last completed sync.
        synced_at INTEGER
    );

    CREATE TABLE labels (
        account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        id TEXT NOT NULL,
        name TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('system', 'user')),
        PRIMARY KEY (account_id, id)
    ) WITHOUT ROWID;

    CREATE TABLE messages (
        id INTEGER PRIMARY KEY,
        account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        gmail_id TEXT NOT NULL,
        thread_id TEXT NOT NULL,
        -- Unix milliseconds when Gmail received the message.
        internal_date INTEGER NOT NULL,
        subject TEXT NOT NULL DEFAULT '',
        from_name TEXT NOT NULL DEFAULT '',
        from_addr TEXT NOT NULL DEFAULT '',
        to_addrs TEXT NOT NULL DEFAULT '',
        cc_addrs TEXT NOT NULL DEFAULT '',
        snippet TEXT NOT NULL DEFAULT '',
        body_text TEXT NOT NULL DEFAULT '',
        -- Moved to message_bodies by migration 3.
        body_html TEXT,
        message_id_header TEXT,
        in_reply_to TEXT,
        references_header TEXT,
        size_estimate INTEGER NOT NULL DEFAULT 0,
        UNIQUE (account_id, gmail_id)
    );
    CREATE INDEX messages_by_thread ON messages (account_id, thread_id, internal_date);
    CREATE INDEX messages_by_date ON messages (account_id, internal_date DESC);

    -- Gmail labels are per message; a thread shows the union of its messages'.
    -- Label IDs are not foreign keys: a message can arrive before its label.
    CREATE TABLE message_labels (
        message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
        label_id TEXT NOT NULL,
        PRIMARY KEY (message_id, label_id)
    ) WITHOUT ROWID;
    CREATE INDEX message_labels_by_label ON message_labels (label_id, message_id);

    CREATE TABLE attachments (
        message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
        part_id TEXT NOT NULL,
        filename TEXT NOT NULL,
        mime_type TEXT NOT NULL,
        size INTEGER NOT NULL,
        gmail_attachment_id TEXT,
        PRIMARY KEY (message_id, part_id)
    ) WITHOUT ROWID;

    -- Word search over the fields people search mail by. External content:
    -- the text lives once, in `messages`, and triggers keep the index current.
    -- unicode61 with prefix indexes gives type-ahead matching and bm25
    -- ranking at a fraction of a trigram index's size on long bodies.
    CREATE VIRTUAL TABLE message_search USING fts5 (
        subject, from_name, from_addr, to_addrs, cc_addrs, body_text,
        content = 'messages',
        content_rowid = 'id',
        tokenize = 'unicode61 remove_diacritics 2',
        prefix = '2 3'
    );

    CREATE TRIGGER message_search_insert AFTER INSERT ON messages BEGIN
        INSERT INTO message_search (rowid, subject, from_name, from_addr, to_addrs, cc_addrs, body_text)
        VALUES (new.id, new.subject, new.from_name, new.from_addr, new.to_addrs, new.cc_addrs, new.body_text);
    END;

    CREATE TRIGGER message_search_delete AFTER DELETE ON messages BEGIN
        INSERT INTO message_search (message_search, rowid, subject, from_name, from_addr, to_addrs, cc_addrs, body_text)
        VALUES ('delete', old.id, old.subject, old.from_name, old.from_addr, old.to_addrs, old.cc_addrs, old.body_text);
    END;

    CREATE TRIGGER message_search_update
    AFTER UPDATE OF subject, from_name, from_addr, to_addrs, cc_addrs, body_text ON messages BEGIN
        INSERT INTO message_search (message_search, rowid, subject, from_name, from_addr, to_addrs, cc_addrs, body_text)
        VALUES ('delete', old.id, old.subject, old.from_name, old.from_addr, old.to_addrs, old.cc_addrs, old.body_text);
        INSERT INTO message_search (rowid, subject, from_name, from_addr, to_addrs, cc_addrs, body_text)
        VALUES (new.id, new.subject, new.from_name, new.from_addr, new.to_addrs, new.cc_addrs, new.body_text);
    END;
    "#,
    // 2. Triage done in Mail and not yet sent to Gmail, oldest first.
    r#"
    CREATE TABLE pending_changes (
        id INTEGER PRIMARY KEY,
        account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        thread_id TEXT NOT NULL,
        -- 'labels', 'trash' or 'untrash'.
        kind TEXT NOT NULL CHECK (kind IN ('labels', 'trash', 'untrash')),
        -- Space-separated label IDs, for 'labels'.
        add_labels TEXT NOT NULL DEFAULT '',
        remove_labels TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL
    );
    "#,
    // 3. HTML moves to its own table, compressed, so `messages` stays narrow.
    // SQL can't compress, so the HTML arrives here as text and Store::open
    // compresses it once.
    r#"
    CREATE TABLE message_bodies (
        message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
        -- Deflate-compressed UTF-8.
        html BLOB NOT NULL
    );
    INSERT INTO message_bodies (message_id, html)
        SELECT id, body_html FROM messages WHERE trim(coalesce(body_html, '')) != '';
    ALTER TABLE messages DROP COLUMN body_html;
    "#,
];

pub(super) const LATEST: i64 = MIGRATIONS.len() as i64;

pub(super) fn version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

pub(super) fn migrate(connection: &mut Connection) -> anyhow::Result<()> {
    let current = version(connection)?;
    if current > LATEST {
        bail!("This mail database was created by a newer version of Mail.");
    }
    for (number, sql) in (1..).zip(MIGRATIONS).skip(current as usize) {
        let transaction = connection.transaction()?;
        transaction
            .execute_batch(sql)
            .with_context(|| format!("Database migration {number} failed"))?;
        transaction.pragma_update(None, "user_version", number)?;
        transaction.commit()?;
    }
    Ok(())
}
