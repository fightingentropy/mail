//! Messages as IMAP has them, raw MIME, in the shape of Gmail's
//! `format=full` JSON, so [`parse_message`](crate::gmail::parse_message)
//! reads mail from either. Also the headers a message needs before it is
//! saved or sent, which Gmail would otherwise add.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use encoding_rs::WINDOWS_1252;
use mailparse::{DispositionType, MailHeader, ParsedMail, parse_content_type};

use crate::gmail::{ApiMessage, BASE64URL, Body, Header, Part};

/// What IMAP says about a message besides its content.
pub(super) struct Meta {
    pub id: String,
    pub thread_id: String,
    pub label_ids: Vec<String>,
    /// Unix milliseconds.
    pub internal_date: i64,
    pub size: i64,
}

/// A whole message. Text parts come with their contents; any other part
/// gets an attachment ID from `attachment_id`, given the part's IMAP number
/// such as `2` or `1.2`, and is read again when opened.
pub(super) fn full(raw: &[u8], meta: Meta, attachment_id: &dyn Fn(&str) -> String) -> ApiMessage {
    let payload = match mailparse::parse_mail(raw) {
        Ok(mail) => {
            let path = if mail.subparts.is_empty() { "1" } else { "" };
            part(&mail, path, attachment_id)
        }
        Err(_) => Part::default(),
    };
    api_message(meta, payload)
}

/// Just the headers, for a message whose body is read later.
pub(super) fn headers_only(raw_headers: &[u8], meta: Meta) -> ApiMessage {
    let headers = mailparse::parse_headers(raw_headers)
        .map(|(headers, _)| headers)
        .unwrap_or_default();
    let payload = Part {
        mime_type: headers
            .iter()
            .find(|header| header.get_key_ref().eq_ignore_ascii_case("Content-Type"))
            .map(|header| parse_content_type(&header.get_value()).mimetype)
            .unwrap_or_else(|| "text/plain".to_owned()),
        headers: headers.iter().map(header).collect(),
        ..Part::default()
    };
    api_message(meta, payload)
}

fn api_message(meta: Meta, payload: Part) -> ApiMessage {
    ApiMessage {
        id: meta.id,
        thread_id: meta.thread_id,
        label_ids: meta.label_ids,
        // Made from the body by parse_message.
        snippet: String::new(),
        internal_date: meta.internal_date.to_string(),
        size_estimate: meta.size,
        payload: Some(payload),
    }
}

fn part(mail: &ParsedMail, path: &str, attachment_id: &dyn Fn(&str) -> String) -> Part {
    let mime_type = mail.ctype.mimetype.to_ascii_lowercase();
    let disposition = mail.get_content_disposition();
    let filename = disposition
        .params
        .get("filename")
        .or_else(|| mail.ctype.params.get("name"))
        .cloned()
        .unwrap_or_default();
    let headers = mail.headers.iter().map(header).collect();
    if !mail.subparts.is_empty() {
        let parts = mail
            .subparts
            .iter()
            .enumerate()
            .map(|(number, child)| {
                let child_path = match path {
                    "" => (number + 1).to_string(),
                    _ => format!("{path}.{}", number + 1),
                };
                part(child, &child_path, attachment_id)
            })
            .collect();
        return Part {
            part_id: path.to_owned(),
            mime_type,
            filename,
            headers,
            body: None,
            parts,
        };
    }
    let data = mail.get_body_raw().unwrap_or_default();
    let is_body = matches!(mime_type.as_str(), "text/plain" | "text/html")
        && filename.is_empty()
        && disposition.disposition != DispositionType::Attachment;
    let body = Body {
        size: data.len() as i64,
        data: is_body.then(|| BASE64URL.encode(&data)),
        attachment_id: (!is_body).then(|| attachment_id(path)),
    };
    Part {
        part_id: path.to_owned(),
        mime_type,
        filename,
        headers,
        body: Some(body),
        parts: Vec::new(),
    }
}

/// The header as Gmail gives it: unfolded, with encoded words left for
/// `parse_message` to decode.
fn header(header: &MailHeader) -> Header {
    let mut value = Vec::new();
    let mut bytes = header.get_value_raw().iter().peekable();
    while let Some(&byte) = bytes.next() {
        match byte {
            b'\r' => {}
            b'\n'
                if bytes
                    .peek()
                    .is_some_and(|next| matches!(next, b' ' | b'\t')) => {}
            _ => value.push(byte),
        }
    }
    let value = match String::from_utf8(value) {
        Ok(value) => value,
        Err(error) => WINDOWS_1252.decode(error.as_bytes()).0.into_owned(),
    };
    Header {
        name: header.get_key(),
        value: value.trim().to_owned(),
    }
}

/// The contents of the part numbered `path`, decoded from its transfer
/// encoding. A message that isn't multipart is its own part 1.
pub(super) fn part_data(raw: &[u8], path: &str) -> Option<Vec<u8>> {
    let mail = mailparse::parse_mail(raw).ok()?;
    let mut part = &mail;
    for number in path.split('.') {
        let number: usize = number.parse().ok()?;
        if part.subparts.is_empty() && number == 1 {
            continue;
        }
        part = part.subparts.get(number.checked_sub(1)?)?;
    }
    part.get_body_raw().ok()
}

/// The value of the first `name` header in a block of headers, unfolded.
pub(super) fn header_value(raw_headers: &[u8], name: &str) -> Option<String> {
    let (headers, _) = mailparse::parse_headers(raw_headers).ok()?;
    headers
        .iter()
        .find(|header| header.get_key_ref().eq_ignore_ascii_case(name))
        .map(|found| header(found).value)
        .filter(|value| !value.is_empty())
}

/// The bare addresses in the To, Cc and Bcc headers: who a message goes to.
pub(super) fn recipients(raw: &[u8]) -> Vec<String> {
    let Ok((headers, _)) = mailparse::parse_headers(raw) else {
        return Vec::new();
    };
    let mut addresses = Vec::new();
    for found in headers.iter().filter(|header| {
        let key = header.get_key_ref();
        ["To", "Cc", "Bcc"]
            .iter()
            .any(|name| key.eq_ignore_ascii_case(name))
    }) {
        let Ok(list) = mailparse::addrparse_header(found) else {
            continue;
        };
        for address in list.iter() {
            match address {
                mailparse::MailAddr::Single(single) => addresses.push(single.addr.clone()),
                mailparse::MailAddr::Group(group) => {
                    addresses.extend(group.addrs.iter().map(|single| single.addr.clone()));
                }
            }
        }
    }
    addresses
}

/// `raw` with its own Date and Message-ID: Gmail adds these when it saves a
/// draft, IMAP doesn't. Any it had are replaced, so every saved version of a
/// draft keeps the ID it is found by. Bcc is dropped if `without_bcc`, for
/// the copy that goes out.
pub(super) fn stamped(raw: &[u8], message_id: &str, without_bcc: bool) -> Vec<u8> {
    let split = find(raw, b"\r\n\r\n").map_or(raw.len(), |at| at + 2);
    let (head, body) = raw.split_at(split);
    let mut out = format!("Date: {}\r\nMessage-ID: {message_id}\r\n", rfc2822_now()).into_bytes();
    let mut skipping = false;
    for line in head.split_inclusive(|&byte| byte == b'\n') {
        let continues = line
            .first()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'));
        if !continues {
            let name = line
                .split(|&byte| byte == b':')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            skipping = matches!(name.as_slice(), b"date" | b"message-id")
                || (without_bcc && name == b"bcc");
        }
        if !skipping {
            out.extend_from_slice(line);
        }
    }
    out.extend_from_slice(body);
    out
}

/// A new Message-ID at the domain of `from`.
pub(super) fn new_message_id(from: &str) -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the system has a random source");
    let random: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let domain = from
        .rsplit_once('@')
        .map_or("mail.local", |(_, domain)| domain);
    format!("<{random}@{domain}>")
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Now as an RFC 5322 date, in UTC.
fn rfc2822_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default();
    rfc2822(seconds)
}

pub(super) fn rfc2822(seconds: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil(days);
    format!(
        "{}, {day} {} {year} {:02}:{:02}:{:02} +0000",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[month as usize - 1],
        time / 3600,
        time / 60 % 60,
        time % 60,
    )
}

/// The date `days` before now as IMAP's SEARCH writes it, such as `4-Aug-2026`.
pub(super) fn imap_date(days_ago: u32) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default();
    let (year, month, day) = civil(now.div_euclid(86_400) - i64::from(days_ago));
    format!("{day}-{}-{year}", MONTHS[month as usize - 1])
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Year, month and day of a count of days since 1970-01-01 (Howard
/// Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
