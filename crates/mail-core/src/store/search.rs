//! Offline search over stored mail with SQLite FTS5.

use anyhow::ensure;
use rusqlite::{params, types::Value};

use super::Store;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    /// Best matches first (bm25), newest first among equals.
    #[default]
    Relevance,
    Newest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchQuery {
    /// Words to find. Each word also matches as a prefix ("inv" finds
    /// "invoice"), and every word must match. Empty lists the newest mail.
    pub text: String,
    /// Part of the sender's name or address, ignoring ASCII case.
    pub from: Option<String>,
    /// A label ID such as `INBOX` or `Label_42`. Resolve names with
    /// [`Store::resolve_label`] first.
    pub label_id: Option<String>,
    /// Received at or after, in Unix milliseconds.
    pub since: Option<i64>,
    /// Received strictly before, in Unix milliseconds.
    pub before: Option<i64>,
    pub order: Order,
    pub limit: u32,
    pub offset: u32,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            from: None,
            label_id: None,
            since: None,
            before: None,
            order: Order::Relevance,
            limit: 20,
            offset: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub gmail_id: String,
    pub thread_id: String,
    pub internal_date: i64,
    pub from_name: String,
    pub from_addr: String,
    pub subject: String,
    /// A short passage of the body around the match, with matched words
    /// wrapped in « and ». Gmail's snippet when the query has no words.
    pub excerpt: String,
    pub label_ids: Vec<String>,
}

/// Field weights for bm25, in `message_search` column order: a word in the
/// subject or sender counts for more than the same word deep in a body.
const RANK: &str = "bm25(message_search, 8.0, 4.0, 4.0, 2.0, 1.0, 1.0)";

/// Filters shared by both query shapes. Parameters ?2 to ?7 are bound in
/// [`Store::search`]. Trash and spam stay hidden unless asked for by label.
const FILTERS: &str = "m.account_id = ?2
    AND (?3 IS NULL OR m.from_name LIKE ?3 ESCAPE '!' OR m.from_addr LIKE ?3 ESCAPE '!')
    AND (?4 IS NULL OR EXISTS (
        SELECT 1 FROM message_labels l WHERE l.message_id = m.id AND l.label_id = ?4))
    AND (?5 IS NULL OR m.internal_date >= ?5)
    AND (?6 IS NULL OR m.internal_date < ?6)
    AND (?7 OR NOT EXISTS (
        SELECT 1 FROM message_labels l WHERE l.message_id = m.id AND l.label_id IN ('TRASH', 'SPAM')))";

const LABELS: &str =
    "(SELECT group_concat(label_id, ' ') FROM message_labels l WHERE l.message_id = m.id)";

impl Store {
    pub fn search(&self, account_id: i64, query: &SearchQuery) -> anyhow::Result<Vec<SearchHit>> {
        ensure!(
            (1..=500).contains(&query.limit),
            "limit must be between 1 and 500"
        );
        let expression = fts_expression(&query.text);
        ensure!(
            expression.is_some() || query.text.trim().is_empty(),
            "Search for words or numbers; punctuation alone matches nothing."
        );

        let sql = match expression {
            Some(_) => {
                let order = match query.order {
                    Order::Relevance => format!("{RANK}, m.internal_date DESC"),
                    Order::Newest => "m.internal_date DESC".to_owned(),
                };
                format!(
                    "SELECT m.gmail_id, m.thread_id, m.internal_date, m.from_name, m.from_addr,
                            m.subject, snippet(message_search, 5, '«', '»', '…', 16), {LABELS}
                     FROM message_search JOIN messages m ON m.id = message_search.rowid
                     WHERE message_search MATCH ?1 AND {FILTERS}
                     ORDER BY {order} LIMIT ?8 OFFSET ?9"
                )
            }
            // ?1 is always NULL here; it keeps parameter numbers the same in
            // both shapes.
            None => format!(
                "SELECT m.gmail_id, m.thread_id, m.internal_date, m.from_name, m.from_addr,
                        m.subject, m.snippet, {LABELS}
                 FROM messages m
                 WHERE ?1 IS NULL AND {FILTERS}
                 ORDER BY m.internal_date DESC LIMIT ?8 OFFSET ?9"
            ),
        };

        let show_hidden = matches!(query.label_id.as_deref(), Some("TRASH" | "SPAM"));
        let from = query
            .from
            .as_ref()
            .map(|from| format!("%{}%", escape_like(from)));
        let mut statement = self.connection().prepare(&sql)?;
        let hits = statement
            .query_map(
                params![
                    expression.map(Value::Text).unwrap_or(Value::Null),
                    account_id,
                    from,
                    query.label_id,
                    query.since,
                    query.before,
                    show_hidden,
                    query.limit,
                    query.offset,
                ],
                |row| {
                    let labels: Option<String> = row.get(7)?;
                    Ok(SearchHit {
                        gmail_id: row.get(0)?,
                        thread_id: row.get(1)?,
                        internal_date: row.get(2)?,
                        from_name: row.get(3)?,
                        from_addr: row.get(4)?,
                        subject: row.get(5)?,
                        excerpt: row.get(6)?,
                        label_ids: labels
                            .unwrap_or_default()
                            .split(' ')
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned)
                            .collect(),
                    })
                },
            )?
            .collect::<rusqlite::Result<_>>()?;
        Ok(hits)
    }
}

/// Turns typed words into an FTS5 query that cannot be a syntax error: each
/// word becomes a quoted prefix phrase, and all of them must match. Words with
/// no letters or digits are dropped, since the tokenizer would ignore them.
pub(crate) fn fts_expression(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// Makes `%` and `_` typed by the user match themselves in `LIKE ... ESCAPE '!'`.
fn escape_like(text: &str) -> String {
    text.replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
}
