//! Turns Gmail's `format=full` message JSON into a [`Message`].

use std::borrow::Cow;

use base64::{
    Engine, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use encoding_rs::{Encoding, UTF_8};
use serde::Deserialize;

use super::html;
use crate::model::{Attachment, Message};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiMessage {
    pub id: String,
    pub thread_id: String,
    #[serde(default)]
    pub label_ids: Vec<String>,
    #[serde(default)]
    pub snippet: String,
    /// Unix milliseconds, as a string.
    #[serde(default)]
    pub internal_date: String,
    #[serde(default)]
    pub size_estimate: i64,
    pub payload: Option<Part>,
}

/// One node of the MIME tree. The root carries the message headers.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(default)]
    pub part_id: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub headers: Vec<Header>,
    pub body: Option<Body>,
    #[serde(default)]
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    #[serde(default)]
    pub size: i64,
    /// base64url, present for text parts.
    pub data: Option<String>,
    /// Present for attachments, whose data is fetched separately.
    pub attachment_id: Option<String>,
}

impl Part {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    }

    fn is_attachment(&self) -> bool {
        let disposition = self.header("Content-Disposition").unwrap_or_default();
        !self.filename.is_empty()
            || disposition
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("attachment")
    }

    /// The part's text, decoded from base64url and its declared charset.
    fn text(&self) -> Option<String> {
        let data = self.body.as_ref()?.data.as_ref()?;
        let bytes = BASE64URL.decode(data.trim_end()).ok()?;
        let charset = self
            .header("Content-Type")
            .and_then(|value| parameter(value, "charset"))
            .unwrap_or_default();
        let encoding = Encoding::for_label(charset.as_bytes()).unwrap_or(UTF_8);
        let (text, _, _) = encoding.decode(&bytes);
        Some(text.into_owned())
    }
}

/// Gmail's base64url sometimes has padding and sometimes doesn't.
pub(super) const BASE64URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

pub fn parse_message(api: ApiMessage) -> Message {
    let root = api.payload.unwrap_or_default();
    let header = |name: &str| root.header(name).map(decode_words);
    let (from_name, from_addr) = header("From")
        .map(|from| split_address(&from))
        .unwrap_or_default();

    let mut found = Found::default();
    collect(&root, &mut found);
    let body_text = match (&found.text, &found.html) {
        (Some(text), _) => clean_plain_text(text),
        (None, Some(html)) => html::to_text(html),
        (None, None) => String::new(),
    };

    Message {
        gmail_id: api.id,
        thread_id: api.thread_id,
        internal_date: api.internal_date.parse().unwrap_or_default(),
        subject: header("Subject").unwrap_or_default(),
        from_name,
        from_addr,
        to: header("To").unwrap_or_default(),
        cc: header("Cc").unwrap_or_default(),
        snippet: html::tidy(&html::decode_entities(&api.snippet)),
        body_text,
        has_html: found.html.is_some(),
        body_pending: false,
        body_html: found.html,
        message_id_header: header("Message-ID"),
        in_reply_to: header("In-Reply-To"),
        references: header("References"),
        size_estimate: api.size_estimate,
        label_ids: api.label_ids,
        attachments: found.attachments,
    }
}

#[derive(Default)]
struct Found {
    text: Option<String>,
    html: Option<String>,
    attachments: Vec<Attachment>,
}

/// Walks the MIME tree depth first. The first text/plain and text/html parts
/// that are not attachments are the body; in multipart/alternative they are
/// two versions of the same content.
fn collect(part: &Part, found: &mut Found) {
    if part.is_attachment() {
        let body = part.body.clone().unwrap_or_default();
        found.attachments.push(Attachment {
            part_id: part.part_id.clone(),
            filename: decode_words(&part.filename),
            mime_type: part.mime_type.clone(),
            size: body.size,
            gmail_attachment_id: body.attachment_id,
        });
        return;
    }
    match part.mime_type.to_ascii_lowercase().as_str() {
        "text/plain" if found.text.is_none() => found.text = part.text(),
        "text/html" if found.html.is_none() => found.html = part.text(),
        _ => {}
    }
    for child in &part.parts {
        collect(child, found);
    }
}

/// Plain text keeps its own line structure; only line endings and invisible
/// padding change.
fn clean_plain_text(text: &str) -> String {
    let text: String = text
        .replace("\r\n", "\n")
        .chars()
        .filter(|&c| !html::is_invisible(c))
        .collect();
    text.trim().to_owned()
}

/// Reads `name=value` or `name="value"` from a header like Content-Type.
fn parameter<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').skip(1).find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().trim_matches('"'))
    })
}

/// Splits `"Ana Example" <ana@example.com>` into name and address. A bare
/// address has an empty name.
pub(crate) fn split_address(value: &str) -> (String, String) {
    let value = value.trim();
    match (value.rfind('<'), value.rfind('>')) {
        (Some(open), Some(close)) if open < close => {
            let name = value[..open].trim().trim_matches('"').replace("\\\"", "\"");
            let address = value[open + 1..close].trim().to_owned();
            (name, address)
        }
        _ => (String::new(), value.to_owned()),
    }
}

/// Decodes RFC 2047 encoded words such as `=?UTF-8?B?Q2Fmw6k=?=` in headers.
/// Gmail usually decodes these already; this covers the times it doesn't.
pub(crate) fn decode_words(value: &str) -> String {
    if !value.contains("=?") {
        return value.to_owned();
    }
    let mut out = String::new();
    let mut rest = value;
    let mut last_was_word = false;
    while let Some(start) = rest.find("=?") {
        let (before, candidate) = rest.split_at(start);
        match decode_word(candidate) {
            Some((text, used)) => {
                // Whitespace between two encoded words is not part of the text.
                if !(last_was_word && before.trim().is_empty()) {
                    out.push_str(before);
                }
                out.push_str(&text);
                rest = &candidate[used..];
                last_was_word = true;
            }
            None => {
                out.push_str(before);
                out.push_str("=?");
                rest = &candidate[2..];
                last_was_word = false;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decodes one `=?charset?encoding?text?=` at the start of `input`, returning
/// the text and how many bytes it used.
fn decode_word(input: &str) -> Option<(String, usize)> {
    let inner = input.strip_prefix("=?")?;
    let mut fields = inner.splitn(3, '?');
    let charset = fields.next()?;
    let encoding = fields.next()?;
    let rest = fields.next()?;
    let end = rest.find("?=")?;
    let text = &rest[..end];
    let used = 2 + charset.len() + 1 + encoding.len() + 1 + end + 2;

    let bytes: Cow<[u8]> = match encoding {
        "B" | "b" => Cow::Owned(
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .ok()?,
        ),
        "Q" | "q" => Cow::Owned(decode_q(text)),
        _ => return None,
    };
    // Charsets may carry a language tag: `UTF-8*en`.
    let label = charset.split('*').next().unwrap_or(charset);
    let decoder = Encoding::for_label(label.as_bytes())?;
    Some((decoder.decode(&bytes).0.into_owned(), used))
}

/// The "Q" encoding: quoted-printable where `_` is a space.
fn decode_q(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'_' => out.push(b' '),
            b'=' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        index += 2;
                    }
                    None => out.push(b'='),
                }
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    out
}

#[cfg(test)]
mod tests;
