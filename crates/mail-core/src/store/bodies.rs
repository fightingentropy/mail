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
