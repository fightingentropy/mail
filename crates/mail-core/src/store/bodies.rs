//! HTML versions of messages, kept apart from `messages` and compressed.
//!
//! HTML is most of the bytes in a mailbox but is only read when you open a
//! message in the browser. Keeping it in its own table keeps `messages`
//! narrow, so lists and searches read fewer pages; deflate shrinks it several
//! times over.

use std::io::{Read, Write};

use flate2::{Compression, read::DeflateDecoder, write::DeflateEncoder};
use rusqlite::{Connection, OptionalExtension, params, types::ValueRef};

use super::Store;
use crate::{
    gmail,
    model::{BodyLink, InlineImage},
};

impl Store {
    /// The HTML version of a stored message, if it has one.
    pub fn message_html(&self, account_id: i64, gmail_id: &str) -> anyhow::Result<Option<String>> {
        let html = self
            .connection
            .query_row(
                "SELECT b.html FROM message_bodies b JOIN messages m ON m.id = b.message_id
                 WHERE m.account_id = ?1 AND m.gmail_id = ?2",
                params![account_id, gmail_id],
                |row| read_html(row.get_ref(0)?),
            )
            .optional()?;
        Ok(html)
    }

    /// The pictures kept for a message's HTML.
    pub fn inline_images(
        &self,
        account_id: i64,
        gmail_id: &str,
    ) -> anyhow::Result<Vec<InlineImage>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT i.content_id, i.mime_type, i.data FROM inline_images i
             JOIN messages m ON m.id = i.message_id
             WHERE m.account_id = ?1 AND m.gmail_id = ?2 ORDER BY i.content_id",
        )?;
        let images = statement
            .query_map(params![account_id, gmail_id], |row| {
                Ok(InlineImage {
                    content_id: row.get(0)?,
                    mime_type: row.get(1)?,
                    data: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(images)
    }

    /// Keeps pictures read from Gmail for a message's HTML. They go when the
    /// message does.
    pub fn save_inline_images(
        &mut self,
        account_id: i64,
        gmail_id: &str,
        images: &[InlineImage],
    ) -> anyhow::Result<()> {
        let transaction = self.connection.transaction()?;
        {
            let mut insert = transaction.prepare(
                "INSERT OR REPLACE INTO inline_images (message_id, content_id, mime_type, data)
                 SELECT id, ?3, ?4, ?5 FROM messages WHERE account_id = ?1 AND gmail_id = ?2",
            )?;
            for image in images {
                insert.execute(params![
                    account_id,
                    gmail_id,
                    image.content_id,
                    image.mime_type,
                    image.data
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

/// Stores or clears a message's HTML.
pub(super) fn save_html(
    connection: &Connection,
    message_id: i64,
    html: Option<&str>,
) -> anyhow::Result<()> {
    match html.filter(|html| !html.trim().is_empty()) {
        Some(html) => connection.execute(
            "INSERT INTO message_bodies (message_id, html) VALUES (?1, ?2)
             ON CONFLICT (message_id) DO UPDATE SET html = excluded.html",
            params![message_id, compress(html)?],
        )?,
        None => connection.execute(
            "DELETE FROM message_bodies WHERE message_id = ?1",
            [message_id],
        )?,
    };
    Ok(())
}

/// Migration 3 moved existing HTML over as plain text, because SQL can't
/// compress. This compresses it in batches and then reclaims the freed
/// space. After the first run there is nothing left to do.
pub(super) fn compress_moved_html(connection: &mut Connection) -> anyhow::Result<()> {
    let mut compressed = 0;
    loop {
        let transaction = connection.transaction()?;
        let batch: Vec<(i64, String)> = {
            let mut select = transaction.prepare(
                "SELECT message_id, html FROM message_bodies WHERE typeof(html) = 'text' LIMIT 500",
            )?;
            select
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        if batch.is_empty() {
            break;
        }
        {
            let mut update =
                transaction.prepare("UPDATE message_bodies SET html = ?2 WHERE message_id = ?1")?;
            for (message_id, html) in &batch {
                update.execute(params![message_id, compress(html)?])?;
            }
        }
        transaction.commit()?;
        compressed += batch.len();
    }
    if compressed > 0 {
        connection.execute_batch("VACUUM")?;
    }
    Ok(())
}

/// Migration 5 queued every stored message to have its text made again,
/// with the better newsletter handling and the links among its words. The
/// text part itself isn't stored, but the old body text is that part, only
/// lightly cleaned, unless it was made from the HTML; so each message is
/// redone from what is here. Works in batches and drops the queue at the end.
pub(super) fn refresh_body_text(connection: &mut Connection) -> anyhow::Result<()> {
    let queued: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'body_refresh')",
        [],
        |row| row.get(0),
    )?;
    if !queued {
        return Ok(());
    }
    loop {
        let transaction = connection.transaction()?;
        let batch: Vec<(i64, Option<String>, Option<String>)> = {
            let mut select = transaction.prepare(
                "SELECT r.message_id, m.body_text, b.html FROM body_refresh r
                 LEFT JOIN messages m ON m.id = r.message_id
                 LEFT JOIN message_bodies b ON b.message_id = r.message_id
                 LIMIT 200",
            )?;
            select
                .query_map([], |row| {
                    let html = match row.get_ref(2)? {
                        ValueRef::Null => None,
                        value => Some(read_html(value)?),
                    };
                    Ok((row.get(0)?, row.get(1)?, html))
                })?
                .collect::<rusqlite::Result<_>>()?
        };
        if batch.is_empty() {
            break;
        }
        {
            let mut update = transaction
                .prepare("UPDATE messages SET body_text = ?2, body_links = ?3 WHERE id = ?1")?;
            let mut done = transaction.prepare("DELETE FROM body_refresh WHERE message_id = ?1")?;
            for (message_id, old, html) in &batch {
                // A message deleted since it was queued has nothing to redo.
                if let Some(old) = old {
                    let (text, links) = refreshed(old, html.as_deref());
                    if text != *old || !links.is_empty() {
                        update.execute(params![message_id, text, super::links_to_json(&links)?])?;
                    }
                }
                done.execute([message_id])?;
            }
        }
        transaction.commit()?;
    }
    connection.execute_batch("DROP TABLE body_refresh")?;
    Ok(())
}

/// Text made from the HTML before is made again, now with its links; text
/// from a text part is judged again, and kept or replaced by the HTML's.
/// Line breaks changed since, so the words alone tell which it was.
fn refreshed(old: &str, html: Option<&str>) -> (String, Vec<BodyLink>) {
    let from_html = html.is_some_and(|html| {
        gmail::html_text(html)
            .split_whitespace()
            .eq(old.split_whitespace())
    });
    let plain = (!from_html).then_some(old);
    gmail::body(plain, html)
}

fn compress(html: &str) -> std::io::Result<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(html.as_bytes())?;
    encoder.finish()
}

/// Compressed HTML is a blob; HTML not yet compressed is still text.
fn read_html(value: ValueRef) -> rusqlite::Result<String> {
    match value {
        ValueRef::Text(text) => Ok(String::from_utf8_lossy(text).into_owned()),
        ValueRef::Blob(bytes) => {
            let mut html = String::new();
            DeflateDecoder::new(bytes)
                .read_to_string(&mut html)
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(0, value.data_type(), Box::new(error))
                })?;
            Ok(html)
        }
        other => Err(rusqlite::Error::InvalidColumnType(
            0,
            "html".into(),
            other.data_type(),
        )),
    }
}
