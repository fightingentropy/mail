//! Pictures sent inside a message, which its HTML shows by `cid:` reference
//! rather than by web address: logos in receipts, icons in booking emails,
//! signatures. Sync stores only the HTML; the pictures are read from Gmail
//! the first time the message is shown as designed, and kept from then on.

use std::{borrow::Cow, ops::Range};

use anyhow::Context;
use base64::{Engine, engine::general_purpose::STANDARD};

use super::{
    GmailApi,
    message::{BASE64URL, Part},
};
use crate::model::InlineImage;

/// Larger parts are left out. Inline pictures are small; a big part with a
/// Content-ID is more likely a document.
const MAX_SIZE: i64 = 5 * 1024 * 1024;

/// The Content-IDs `html` shows, each once, in order.
pub fn content_ids(html: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for (_, id) in references(html) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// `html` with each reference to one of `images` replaced by the picture
/// itself, as a `data:` address. Other references stay as they are.
pub fn embed<'a>(html: &'a str, images: &[InlineImage]) -> Cow<'a, str> {
    let mut out = String::new();
    let mut copied = 0;
    for (range, id) in references(html) {
        let Some(image) = find(images, &id) else {
            continue;
        };
        out.push_str(&html[copied..range.start]);
        out.push_str("data:");
        out.push_str(&image.mime_type);
        out.push_str(";base64,");
        STANDARD.encode_string(&image.data, &mut out);
        copied = range.end;
    }
    if out.is_empty() {
        return Cow::Borrowed(html);
    }
    out.push_str(&html[copied..]);
    Cow::Owned(out)
}

/// Reads the pictures named by `wanted` from Gmail: the message once, for
/// its parts, then each picture. A part that isn't a picture is skipped, so
/// a reference can't put a document in the page.
pub fn fetch(
    gmail: &impl GmailApi,
    message_id: &str,
    wanted: &[String],
) -> anyhow::Result<Vec<InlineImage>> {
    let message = gmail
        .message(message_id)?
        .context("The message is no longer in Gmail")?;
    let mut parts = Vec::new();
    if let Some(root) = &message.payload {
        collect(root, &mut parts);
    }
    let mut images = Vec::new();
    for id in wanted {
        let Some(part) = find_part(&parts, id) else {
            continue;
        };
        let Some(body) = part
            .body
            .as_ref()
            .filter(|body| body.size <= MAX_SIZE && could_be_image(&part.mime_type))
        else {
            continue;
        };
        let data = match (&body.data, &body.attachment_id) {
            (Some(data), _) => BASE64URL.decode(data.trim_end())?,
            (None, Some(attachment_id)) => match gmail.attachment(message_id, attachment_id)? {
                Some(data) => data,
                None => continue,
            },
            (None, None) => continue,
        };
        if let Some(mime_type) = image_type(&part.mime_type, &data) {
            images.push(InlineImage {
                content_id: id.clone(),
                mime_type,
                data,
            });
        }
    }
    Ok(images)
}

/// Each `cid:` reference in `html`, where it is and the Content-ID it names.
/// Only references in attributes and CSS count: a reference follows a quote,
/// `=`, `(` or an escaped quote's `;`, so "acid:" in text stays as it is.
fn references(html: &str) -> Vec<(Range<usize>, String)> {
    // ASCII lowercasing keeps every byte where it was.
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find("cid:") {
        let start = from + at;
        from = start + "cid:".len();
        if !matches!(
            html[..start].chars().next_back(),
            Some('"' | '\'' | '=' | '(' | ';')
        ) {
            continue;
        }
        let end = html[from..]
            .find(|c: char| matches!(c, '"' | '\'' | ')' | '>' | '<' | '&') || c.is_whitespace())
            .map_or(html.len(), |length| from + length);
        let id = percent_decode(&html[from..end]);
        if !id.is_empty() {
            found.push((start..end, id));
        }
    }
    found
}

/// A `cid:` address is a Content-ID with some characters %-escaped.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = (bytes[index] == b'%')
            .then(|| text.get(index + 1..index + 3))
            .flatten()
            .filter(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                index += 3;
            }
            None => {
                out.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Content-IDs are compared exactly, then ignoring case, as some senders
/// change the case between the part and the reference.
fn find<'a>(images: &'a [InlineImage], id: &str) -> Option<&'a InlineImage> {
    images
        .iter()
        .find(|image| image.content_id == id)
        .or_else(|| {
            images
                .iter()
                .find(|image| image.content_id.eq_ignore_ascii_case(id))
        })
}

fn find_part<'a>(parts: &[(String, &'a Part)], id: &str) -> Option<&'a Part> {
    parts
        .iter()
        .find(|(content_id, _)| content_id == id)
        .or_else(|| {
            parts
                .iter()
                .find(|(content_id, _)| content_id.eq_ignore_ascii_case(id))
        })
        .map(|(_, part)| *part)
}

/// Every part with a Content-ID.
fn collect<'a>(part: &'a Part, found: &mut Vec<(String, &'a Part)>) {
    if let Some(id) = part.content_id() {
        found.push((id, part));
    }
    for child in &part.parts {
        collect(child, found);
    }
}

/// Whether a part's label leaves room for a picture: senders often label
/// pictures as `application/octet-stream`, but never as `application/pdf`.
pub(super) fn could_be_image(declared: &str) -> bool {
    let declared = declared.trim().to_ascii_lowercase();
    declared.starts_with("image/") || declared == "application/octet-stream" || declared.is_empty()
}

/// The picture's type: from its first bytes, as senders often label
/// pictures `application/octet-stream`, else as labelled. `None` for
/// anything that isn't a picture.
fn image_type(declared: &str, data: &[u8]) -> Option<String> {
    let sniffed = match data {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some("image/webp"),
        _ => None,
    };
    if let Some(sniffed) = sniffed {
        return Some(sniffed.to_owned());
    }
    let declared = declared.trim().to_ascii_lowercase();
    declared.starts_with("image/").then_some(declared)
}

#[cfg(test)]
mod tests;
