//! Where each message is in iCloud, and what filing a thread takes.
//!
//! A message is known by its Message-ID: filed into another folder it keeps
//! its ID instead of turning up as new mail, and copies in two folders, such
//! as mail you sent yourself, are one message with both labels, as in Gmail.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use super::{
    folders::{FOLDER_LABEL, Folder, Role, decode_utf7},
    mime,
};

/// One copy of a message: in which folder, under which UID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Location {
    /// Index into the folder list.
    pub folder: usize,
    pub uid_validity: u32,
    pub uid: u32,
    pub seen: bool,
    pub flagged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub id: String,
    pub thread_id: String,
    pub locations: Vec<Location>,
    /// When it arrived, in Unix milliseconds.
    pub date: i64,
    /// Sent from the account itself, so it goes back to Sent from Trash.
    pub from_self: bool,
}

/// The messages in the folders sync reads, by ID.
#[derive(Clone, Debug, Default)]
pub(super) struct Index {
    pub folders: Vec<Folder>,
    pub entries: HashMap<String, Entry>,
}

impl Index {
    /// Adds one copy, given the Message-ID, In-Reply-To, References and From
    /// headers IMAP returned for it, and when it arrived.
    pub fn add(&mut self, location: Location, headers: &[u8], date: i64, own_address: &str) {
        let message_id = mime::header_value(headers, "Message-ID");
        let id = match &message_id {
            Some(message_id) => key(message_id),
            // Without a Message-ID, a copy is only itself.
            None => key(&format!(
                "{}\0{}\0{}",
                self.folders[location.folder].name, location.uid_validity, location.uid
            )),
        };
        let thread_id = match thread_root(
            mime::header_value(headers, "References").as_deref(),
            mime::header_value(headers, "In-Reply-To").as_deref(),
        ) {
            Some(root) => key(&root),
            None => id.clone(),
        };
        let from_self = mime::header_value(headers, "From").is_some_and(|from| {
            from.to_ascii_lowercase()
                .contains(&own_address.to_ascii_lowercase())
        });
        let entry = self.entries.entry(id.clone()).or_insert_with(|| Entry {
            id,
            thread_id,
            locations: Vec::new(),
            date,
            from_self,
        });
        entry.date = entry.date.max(date);
        entry.locations.push(location);
    }

    /// Messages sync should have: those with a copy outside Trash and Junk,
    /// as Gmail leaves those out of a search.
    pub fn listed(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values().filter(|entry| {
            entry.locations.iter().any(|location| {
                !matches!(self.folders[location.folder].role, Role::Trash | Role::Junk)
            })
        })
    }

    pub fn thread(&self, thread_id: &str) -> Vec<&Entry> {
        self.entries
            .values()
            .filter(|entry| entry.thread_id == thread_id)
            .collect()
    }

    /// The labels a message has: its folders', and UNREAD and STARRED from
    /// its flags.
    pub fn labels(&self, entry: &Entry) -> Vec<String> {
        let mut labels: Vec<String> = Vec::new();
        for location in &entry.locations {
            if let Some(label) = self.folders[location.folder].label()
                && !labels.contains(&label)
            {
                labels.push(label);
            }
        }
        // Your own copy in Sent is read; the one that arrived may not be.
        let unread = entry.locations.iter().any(|location| {
            !location.seen
                && !matches!(
                    self.folders[location.folder].role,
                    Role::Sent | Role::Drafts
                )
        });
        if unread {
            labels.push("UNREAD".to_owned());
        }
        if entry.locations.iter().any(|location| location.flagged) {
            labels.push("STARRED".to_owned());
        }
        labels
    }

    /// The copy to read a message from: one outside Trash and Junk if
    /// there is one.
    pub fn best_location<'a>(&self, entry: &'a Entry) -> Option<&'a Location> {
        entry
            .locations
            .iter()
            .find(|location| {
                !matches!(self.folders[location.folder].role, Role::Trash | Role::Junk)
            })
            .or_else(|| entry.locations.first())
    }

    pub fn folder(&self, role: Role) -> Option<usize> {
        self.folders.iter().position(|folder| folder.role == role)
    }
}

/// A short stable ID, like Gmail's 16 hex digits, for a Message-ID or a
/// thread's first Message-ID. A thread's first message has the thread's
/// ID, as in Gmail.
pub(super) fn key(text: &str) -> String {
    let digest = Sha256::digest(text.trim().as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The Message-ID a conversation started with: the first in References,
/// else the one replied to. `None` for a message that starts one.
pub(super) fn thread_root(references: Option<&str>, in_reply_to: Option<&str>) -> Option<String> {
    [references, in_reply_to]
        .into_iter()
        .flatten()
        .find_map(first_message_id)
}

fn first_message_id(text: &str) -> Option<String> {
    let start = text.find('<')?;
    let end = start + text[start..].find('>')?;
    Some(text[start..=end].to_owned())
}

/// One step of filing a message, done to one copy of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// Sets (`on`) or clears an IMAP flag such as `\Seen`.
    Flag {
        location: Location,
        flag: &'static str,
        on: bool,
    },
    Copy {
        location: Location,
        to: usize,
    },
    Move {
        location: Location,
        to: usize,
    },
    /// Removes this copy; another stays.
    Remove {
        location: Location,
    },
}

/// Why a change can't be made in iCloud.
pub(super) type Refusal = String;

/// The steps that give `entry` the labels Gmail would after adding `add`
/// and removing `remove`. Folders you file in are labels; Archive is
/// having none of them. Flags come first, so copies carry them.
pub(super) fn file(
    index: &Index,
    entry: &Entry,
    add: &[String],
    remove: &[String],
) -> Result<Vec<Step>, Refusal> {
    let mut steps = Vec::new();
    for (label, flag, on_when_added) in
        [("UNREAD", "\\Seen", false), ("STARRED", "\\Flagged", true)]
    {
        let on = if add.iter().any(|id| id == label) {
            on_when_added
        } else if remove.iter().any(|id| id == label) {
            !on_when_added
        } else {
            continue;
        };
        for location in &entry.locations {
            steps.push(Step::Flag {
                location: location.clone(),
                flag,
                on,
            });
        }
    }

    let folder_for = |label: &String| -> Option<Result<usize, Refusal>> {
        let found = index
            .folders
            .iter()
            .position(|folder| folder.is_filing() && folder.label().as_ref() == Some(label));
        match found {
            Some(folder) => Some(Ok(folder)),
            // UNREAD, STARRED, SENT and the like aren't folders to file in.
            None => label
                .strip_prefix(FOLDER_LABEL)
                .map(|name| Err(format!("iCloud has no folder {}", decode_utf7(name)))),
        }
    };
    let adding = add
        .iter()
        .filter_map(folder_for)
        .collect::<Result<Vec<_>, _>>()?;
    let removing = remove
        .iter()
        .filter_map(folder_for)
        .collect::<Result<Vec<_>, _>>()?;
    if adding.is_empty() && removing.is_empty() {
        return Ok(steps);
    }

    // Only copies in folders you file in move. Mail only in Sent or Drafts
    // stays there, as Gmail keeps it out of the inbox.
    let filed: Vec<&Location> = entry
        .locations
        .iter()
        .filter(|location| index.folders[location.folder].is_filing())
        .collect();
    let Some(&source) = filed.first() else {
        return Ok(steps);
    };
    let archive = index.folder(Role::Archive);
    let mut wanted: Vec<usize> = Vec::new();
    for location in &filed {
        let folder = location.folder;
        if Some(folder) != archive && !removing.contains(&folder) && !wanted.contains(&folder) {
            wanted.push(folder);
        }
    }
    for folder in adding {
        if !wanted.contains(&folder) {
            wanted.push(folder);
        }
    }
    if wanted.is_empty() {
        wanted.push(archive.ok_or("iCloud has no Archive folder to archive to")?);
    }

    let mut leaving: Vec<&Location> = filed
        .iter()
        .copied()
        .filter(|location| !wanted.contains(&location.folder))
        .collect();
    let arriving: Vec<usize> = wanted
        .into_iter()
        .filter(|folder| !filed.iter().any(|location| location.folder == *folder))
        .collect();
    let mut moves = Vec::new();
    for to in arriving {
        match leaving.pop() {
            Some(location) => moves.push(Step::Move {
                location: location.clone(),
                to,
            }),
            // Copied before anything moves, from a copy that is still there.
            None => steps.push(Step::Copy {
                location: source.clone(),
                to,
            }),
        }
    }
    steps.extend(moves);
    steps.extend(leaving.into_iter().map(|location| Step::Remove {
        location: location.clone(),
    }));
    Ok(steps)
}

/// Moves every copy outside Trash into it.
pub(super) fn trash(index: &Index, entry: &Entry) -> Result<Vec<Step>, Refusal> {
    let trash = index
        .folder(Role::Trash)
        .ok_or("iCloud has no Deleted Messages folder")?;
    Ok(entry
        .locations
        .iter()
        .filter(|location| location.folder != trash)
        .map(|location| Step::Move {
            location: location.clone(),
            to: trash,
        })
        .collect())
}

/// Moves copies in Trash back: your own mail to Sent, the rest to Inbox.
/// IMAP doesn't remember where they were.
pub(super) fn untrash(index: &Index, entry: &Entry) -> Result<Vec<Step>, Refusal> {
    let Some(trash) = index.folder(Role::Trash) else {
        return Ok(Vec::new());
    };
    let to = match index.folder(Role::Sent) {
        Some(sent) if entry.from_self => sent,
        _ => index.folder(Role::Inbox).ok_or("iCloud has no Inbox")?,
    };
    Ok(entry
        .locations
        .iter()
        .filter(|location| location.folder == trash)
        .map(|location| Step::Move {
            location: location.clone(),
            to,
        })
        .collect())
}
