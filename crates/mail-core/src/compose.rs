//! Writing mail. A draft is built here as an RFC 5322 message and saved to
//! Gmail's Drafts, where it syncs to every device. Only the window sends,
//! through [`send`], after you confirm.

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE},
};

use crate::{
    gmail::{GmailApi, Outcome, split_address},
    model::Message,
};

/// Longest body accepted, in bytes. Far above any real reply; it keeps a
/// runaway agent from uploading megabytes as text.
pub const MAX_BODY: usize = 200_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    /// Comma-separated addresses, as typed: `Ana <ana@example.org>, bo@example.com`.
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    /// Set for replies, so Gmail and other clients put the draft in the thread.
    pub reply: Option<ReplyTo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyTo {
    pub thread_id: String,
    /// The answered message's Message-ID.
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
}

impl Draft {
    /// A reply to `message`: to its sender, or with `all` also to everyone
    /// else on it except you. Replying to your own message goes to its
    /// recipients. `date` is the original's date as it should read in the
    /// "On …, … wrote:" line.
    pub fn reply(message: &Message, own_address: &str, all: bool, date: &str) -> Self {
        let is_mine = |address: &str| address.eq_ignore_ascii_case(own_address);
        let sender = format_address(&message.from_name, &message.from_addr);

        let mut to = Vec::new();
        let mut cc = Vec::new();
        if is_mine(&message.from_addr) {
            to.extend(split_address_list(&message.to));
            if all {
                cc.extend(split_address_list(&message.cc));
            }
        } else {
            to.push(sender.clone());
            if all {
                cc.extend(split_address_list(&message.to));
                cc.extend(split_address_list(&message.cc));
            }
        }
        // Nobody twice, and never yourself.
        let mut seen: Vec<String> = vec![own_address.to_ascii_lowercase()];
        let mut keep = |entry: &String| {
            let address = split_address(entry).1.to_ascii_lowercase();
            if seen.contains(&address) {
                false
            } else {
                seen.push(address);
                true
            }
        };
        to.retain(&mut keep);
        cc.retain(&mut keep);

        let author = if message.from_name.is_empty() {
            message.from_addr.as_str()
        } else {
            message.from_name.as_str()
        };
        let quoted: String = message
            .body_text
            .lines()
            .map(|line| {
                if line.is_empty() {
                    ">".to_owned()
                } else {
                    format!("> {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let references = [
            message.references.as_deref(),
            message.message_id_header.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");

        Self {
            to: to.join(", "),
            cc: cc.join(", "),
            subject: reply_subject(&message.subject),
            body: format!("\n\nOn {date}, {author} wrote:\n{quoted}\n"),
            reply: Some(ReplyTo {
                thread_id: message.thread_id.clone(),
                in_reply_to: message.message_id_header.clone(),
                references: (!references.is_empty()).then_some(references),
            }),
        }
    }

    pub fn has_recipients(&self) -> bool {
        !split_address_list(&self.to).is_empty() || !split_address_list(&self.cc).is_empty()
    }

    /// Entries in To and Cc that don't look like an address.
    pub fn invalid_addresses(&self) -> Vec<String> {
        split_address_list(&self.to)
            .into_iter()
            .chain(split_address_list(&self.cc))
            .filter(|entry| {
                let address = split_address(entry).1;
                let (local, domain) = address.split_once('@').unwrap_or_default();
                local.is_empty() || !domain.contains('.') || address.contains(char::is_whitespace)
            })
            .collect()
    }

    /// The draft as an RFC 5322 message: UTF-8 plain text, base64 encoded.
    /// Header values lose any line breaks, so no field can inject headers.
    pub fn to_rfc5322(&self, from: &str) -> Vec<u8> {
        let mut headers = vec![format!("From: {}", address_header(from))];
        if !self.to.trim().is_empty() {
            headers.push(format!("To: {}", address_header(&self.to)));
        }
        if !self.cc.trim().is_empty() {
            headers.push(format!("Cc: {}", address_header(&self.cc)));
        }
        headers.push(format!(
            "Subject: {}",
            encode_text(&one_line(&self.subject))
        ));
        if let Some(reply) = &self.reply {
            if let Some(id) = &reply.in_reply_to {
                headers.push(format!("In-Reply-To: {}", one_line(id)));
            }
            if let Some(references) = &reply.references {
                headers.push(format!("References: {}", one_line(references)));
            }
        }
        headers.push("MIME-Version: 1.0".to_owned());
        headers.push("Content-Type: text/plain; charset=UTF-8".to_owned());
        headers.push("Content-Transfer-Encoding: base64".to_owned());

        let body = self.body.replace("\r\n", "\n").replace('\n', "\r\n");
        let encoded = STANDARD.encode(body.as_bytes());
        let lines: Vec<&str> = encoded
            .as_bytes()
            .chunks(76)
            .map(|chunk| std::str::from_utf8(chunk).expect("base64 is ASCII"))
            .collect();
        format!("{}\r\n\r\n{}\r\n", headers.join("\r\n"), lines.join("\r\n")).into_bytes()
    }
}

/// Saves `draft` to Gmail's Drafts and returns the draft ID. With an
/// existing ID it updates that draft, or creates a new one if it was
/// deleted meanwhile.
pub fn save(
    api: &dyn GmailApi,
    from: &str,
    draft: &Draft,
    existing: Option<&str>,
) -> anyhow::Result<String> {
    anyhow::ensure!(draft.body.len() <= MAX_BODY, "The message is too long.");
    let invalid = draft.invalid_addresses();
    anyhow::ensure!(
        invalid.is_empty(),
        "Not an email address: {}",
        invalid.join(", ")
    );
    let raw = draft.to_rfc5322(from);
    let thread_id = draft.reply.as_ref().map(|reply| reply.thread_id.as_str());
    if let Some(id) = existing {
        match api.update_draft(id, &raw, thread_id)? {
            Outcome::Done => return Ok(id.to_owned()),
            Outcome::Gone => {}
            Outcome::Rejected(message) => anyhow::bail!("The draft was refused: {message}"),
        }
    }
    api.create_draft(&raw, thread_id)
}

/// Saves `draft` (updating `existing` if given) and sends it. `saved` hears
/// the draft ID first, so a failed send can be retried without making a
/// second draft. Returns the sent message's ID.
///
/// Only the window calls this, after you confirm. The agent CLI and
/// `mail draft` stop at [`save`].
pub fn send(
    api: &dyn GmailApi,
    from: &str,
    draft: &Draft,
    existing: Option<&str>,
    saved: impl FnOnce(&str),
) -> anyhow::Result<String> {
    anyhow::ensure!(draft.has_recipients(), "Add a recipient first.");
    let draft_id = save(api, from, draft, existing)?;
    saved(&draft_id);
    api.send_draft(&draft_id)
}

/// Gmail's draft API wants the message as base64url.
pub fn raw_for_api(raw: &[u8]) -> String {
    URL_SAFE.encode(raw)
}

/// `Re: ` once, however many the original already had.
pub fn reply_subject(subject: &str) -> String {
    let mut rest = subject.trim();
    while let Some(stripped) = strip_prefix_ignoring_case(rest, "re:") {
        rest = stripped.trim_start();
    }
    format!("Re: {rest}")
}

fn strip_prefix_ignoring_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// Splits `a@x.com, "Lima, Ana" <ana@y.org>` at the commas between addresses,
/// not the ones inside quotes or angle brackets.
pub fn split_address_list(list: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut angled = false;
    for character in list.chars() {
        match character {
            '"' => quoted = !quoted,
            '<' if !quoted => angled = true,
            '>' if !quoted => angled = false,
            ',' | ';' if !quoted && !angled => {
                entries.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    entries.push(current);
    entries
        .into_iter()
        .map(|entry| one_line(entry.trim()))
        .filter(|entry| !entry.is_empty())
        .collect()
}

fn format_address(name: &str, address: &str) -> String {
    if name.is_empty() {
        address.to_owned()
    } else {
        format!("{name} <{address}>")
    }
}

/// An address list ready for a header: display names quoted or encoded as
/// needed, addresses as they are.
fn address_header(list: &str) -> String {
    split_address_list(list)
        .iter()
        .map(|entry| {
            let (name, address) = split_address(entry);
            if name.is_empty() {
                address
            } else {
                format!("{} <{address}>", phrase(&name))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A display name: plain, quoted if it holds punctuation, or RFC 2047
/// encoded if it isn't ASCII.
fn phrase(name: &str) -> String {
    if !name.is_ascii() {
        return encode_text(name);
    }
    if name.contains(|c: char| "()<>[]:;@\\,.\"".contains(c)) {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        name.to_owned()
    }
}

/// Encodes non-ASCII header text as RFC 2047 words of at most 75 characters,
/// folded onto separate lines.
fn encode_text(text: &str) -> String {
    if text.is_ascii() {
        return text.to_owned();
    }
    let mut words = Vec::new();
    let mut chunk = String::new();
    for character in text.chars() {
        // 45 bytes of UTF-8 become 60 base64 characters, within the limit.
        if chunk.len() + character.len_utf8() > 45 {
            words.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(&chunk)));
            chunk.clear();
        }
        chunk.push(character);
    }
    if !chunk.is_empty() {
        words.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(&chunk)));
    }
    words.join("\r\n ")
}

fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests;
