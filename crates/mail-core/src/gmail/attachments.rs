//! Attachments on demand. Sync stores only their names and Gmail IDs; the
//! contents download when you open one.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::Context;

use super::{GmailApi, parse_message};
use crate::model::Attachment;

/// Downloads an attachment of message `message_id`.
///
/// Gmail can issue new attachment IDs and stop accepting the one stored at
/// sync. When that happens, the message is read again (60 quota units) for a
/// fresh ID. Network and quota errors are returned as they are: a new ID
/// would not help.
pub fn fetch(
    gmail: &impl GmailApi,
    message_id: &str,
    attachment: &Attachment,
) -> anyhow::Result<Vec<u8>> {
    if let Some(id) = &attachment.gmail_attachment_id
        && let Some(bytes) = gmail.attachment(message_id, id)?
    {
        return Ok(bytes);
    }
    let message = gmail
        .message(message_id)?
        .context("The message is no longer in Gmail")?;
    let fresh = parse_message(message)
        .attachments
        .into_iter()
        .find(|candidate| candidate.part_id == attachment.part_id)
        .and_then(|candidate| candidate.gmail_attachment_id)
        .context("Gmail no longer has this attachment")?;
    gmail
        .attachment(message_id, &fresh)?
        .context("Gmail no longer has this attachment")
}

/// Writes `bytes` into `folder` under the attachment's name, made safe for
/// the file system. An existing file is never replaced: the name gets a
/// number instead, as in `report (1).pdf`.
pub fn save(folder: &Path, filename: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let name = safe_filename(filename);
    let path = Path::new(&name);
    let stem = path
        .file_stem()
        .map_or(name.clone(), |stem| stem.to_string_lossy().into_owned());
    let extension = path
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();

    for copy in 0..1000 {
        let candidate = if copy == 0 {
            folder.join(&name)
        } else {
            folder.join(format!("{stem} ({copy}){extension}"))
        };
        // `create_new` fails if the file exists, so two downloads of the same
        // name can never write over each other.
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&candidate);
            return Err(error);
        }
        return Ok(candidate);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("Too many files named {name} already"),
    ))
}

/// The attachment's name as a single, visible file name. Mail is untrusted
/// input: a name like `../../.zshrc` must not leave the folder or hide, and
/// right-to-left marks must not disguise `fdp.app` as `ppa.pdf`.
pub fn safe_filename(filename: &str) -> String {
    let last = filename.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .filter(|&c| !is_direction_mark(c))
        .map(|c| if c.is_control() || c == ':' { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        return "attachment".to_owned();
    }
    shorten(cleaned)
}

/// Bidirectional controls, which can reorder how a name is displayed.
fn is_direction_mark(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// macOS allows 255 bytes per name. Long names keep their extension, and
/// room for a ` (999)` suffix.
fn shorten(name: &str) -> String {
    const MAX: usize = 240;
    if name.len() <= MAX {
        return name.to_owned();
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && extension.len() <= 16 => {
            (stem, format!(".{extension}"))
        }
        _ => (name, String::new()),
    };
    let mut end = MAX - extension.len();
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{extension}", &stem[..end])
}

#[cfg(test)]
mod tests;
