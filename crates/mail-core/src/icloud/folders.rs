//! iCloud's folders and the Gmail labels they stand for.

use base64::{
    Engine, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use imap_proto::NameAttribute;

/// What a folder is for. Each role but [`Role::Other`] is held by one folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Inbox,
    Sent,
    Drafts,
    Archive,
    Trash,
    Junk,
    /// A folder you made, shown as a label.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Folder {
    /// The name IMAP knows it by, in modified UTF-7.
    pub name: String,
    pub role: Role,
}

/// User labels for folders are this prefix and the folder's IMAP name.
pub(super) const FOLDER_LABEL: &str = "Folder_";

impl Folder {
    /// The label mail in this folder has. Archive has none: archived Gmail
    /// is mail without INBOX.
    pub fn label(&self) -> Option<String> {
        Some(match self.role {
            Role::Inbox => "INBOX".to_owned(),
            Role::Sent => "SENT".to_owned(),
            Role::Drafts => "DRAFT".to_owned(),
            Role::Trash => "TRASH".to_owned(),
            Role::Junk => "SPAM".to_owned(),
            Role::Archive => return None,
            Role::Other => format!("{FOLDER_LABEL}{}", self.name),
        })
    }

    /// The name to show, with any `&…-` runs decoded.
    pub fn display_name(&self) -> String {
        decode_utf7(&self.name)
    }

    /// Mail you can file in and out of this folder; Sent, Drafts, Trash and
    /// Junk keep theirs.
    pub fn is_filing(&self) -> bool {
        matches!(self.role, Role::Inbox | Role::Archive | Role::Other)
    }
}

/// Folders from a LIST, in order, skipping ones that can't hold mail and
/// iCloud's Notes. Each role goes to one folder: special-use attributes
/// first, then the usual names. Any other folder is an ordinary one.
pub(super) fn from_list<'a>(
    names: impl IntoIterator<Item = (&'a str, &'a [NameAttribute<'a>])>,
) -> Vec<Folder> {
    let listed: Vec<(&str, &[NameAttribute])> = names
        .into_iter()
        .filter(|(name, attributes)| {
            !attributes.contains(&NameAttribute::NoSelect) && !name.eq_ignore_ascii_case("Notes")
        })
        .collect();
    let mut folders: Vec<Folder> = listed
        .iter()
        .map(|(name, _)| Folder {
            name: (*name).to_owned(),
            role: Role::Other,
        })
        .collect();
    for by_attribute in [true, false] {
        for (number, (name, attributes)) in listed.iter().enumerate() {
            if folders[number].role != Role::Other {
                continue;
            }
            let guess = if by_attribute {
                special_use(attributes)
            } else {
                by_name(name)
            };
            if let Some(role) = guess
                && !folders.iter().any(|folder| folder.role == role)
            {
                folders[number].role = role;
            }
        }
    }
    folders
}

fn special_use(attributes: &[NameAttribute]) -> Option<Role> {
    attributes.iter().find_map(|attribute| match attribute {
        NameAttribute::Sent => Some(Role::Sent),
        NameAttribute::Drafts => Some(Role::Drafts),
        NameAttribute::Archive => Some(Role::Archive),
        NameAttribute::Trash => Some(Role::Trash),
        NameAttribute::Junk => Some(Role::Junk),
        _ => None,
    })
}

fn by_name(name: &str) -> Option<Role> {
    Some(match name.to_ascii_lowercase().as_str() {
        "inbox" => Role::Inbox,
        "sent messages" | "sent" => Role::Sent,
        "drafts" => Role::Drafts,
        "archive" => Role::Archive,
        "deleted messages" | "trash" => Role::Trash,
        "junk" | "spam" => Role::Junk,
        _ => return None,
    })
}

/// Modified UTF-7 (RFC 3501 5.1.3): `&` starts UTF-16 in base64 with `,`
/// for `/`, `-` ends it, and `&-` is `&` itself.
pub(super) fn decode_utf7(name: &str) -> String {
    const BASE64: GeneralPurpose = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let mut out = String::new();
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let encoded = &after[..end];
        if encoded.is_empty() {
            out.push('&');
        } else {
            match BASE64.decode(encoded.replace(',', "/")) {
                Ok(bytes) => {
                    let units: Vec<u16> = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| u16::from_be_bytes(*pair))
                        .collect();
                    out.push_str(&String::from_utf16_lossy(&units));
                }
                Err(_) => out.push_str(&rest[start..start + end + 2]),
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}
