use std::collections::HashMap;

use imap_proto::NameAttribute;

use super::{
    attachment_id,
    folders::{self, Folder, Role, decode_utf7},
    index::{self, Index, Location, Step},
    mime, parse_attachment_id,
};
use crate::gmail::parse_message;

const ME: &str = "ana@icloud.com";

fn folder(name: &str, role: Role) -> Folder {
    Folder {
        name: name.to_owned(),
        role,
    }
}

/// iCloud's usual folders and one of your own.
fn mailbox() -> Index {
    Index {
        folders: vec![
            folder("INBOX", Role::Inbox),
            folder("Sent Messages", Role::Sent),
            folder("Drafts", Role::Drafts),
            folder("Archive", Role::Archive),
            folder("Deleted Messages", Role::Trash),
            folder("Junk", Role::Junk),
            folder("Receipts", Role::Other),
        ],
        entries: HashMap::new(),
    }
}

const INBOX: usize = 0;
const SENT: usize = 1;
const ARCHIVE: usize = 3;
const TRASH: usize = 4;
const RECEIPTS: usize = 6;

fn at(folder: usize, uid: u32) -> Location {
    Location {
        folder,
        uid_validity: 7,
        uid,
        seen: true,
        flagged: false,
    }
}

fn headers(message_id: &str, extra: &str) -> Vec<u8> {
    format!("Message-ID: {message_id}\r\nFrom: Bo <bo@example.org>\r\n{extra}\r\n").into_bytes()
}

#[test]
fn folders_take_roles_from_special_use_before_names() {
    let names = [
        ("INBOX", vec![]),
        ("Sent", vec![]),
        ("Sent Messages", vec![NameAttribute::Sent]),
        ("Deleted Messages", vec![]),
        ("Notes", vec![]),
        ("Projects", vec![NameAttribute::NoSelect]),
        ("Projects/R&AOk-sum&AOk-", vec![]),
    ];
    let found = folders::from_list(
        names
            .iter()
            .map(|(name, attributes)| (*name, attributes.as_slice())),
    );
    assert_eq!(
        found,
        vec![
            folder("INBOX", Role::Inbox),
            folder("Sent", Role::Other),
            folder("Sent Messages", Role::Sent),
            folder("Deleted Messages", Role::Trash),
            folder("Projects/R&AOk-sum&AOk-", Role::Other),
        ]
    );
    assert_eq!(found[4].display_name(), "Projects/Résumé");
    assert_eq!(found[4].label().unwrap(), "Folder_Projects/R&AOk-sum&AOk-");
    assert_eq!(found[1].label().unwrap(), "Folder_Sent");
}

#[test]
fn folder_names_decode_from_modified_utf7() {
    assert_eq!(decode_utf7("Fish &- Chips"), "Fish & Chips");
    assert_eq!(decode_utf7("&ZeVnLIqe-"), "日本語");
    assert_eq!(decode_utf7("Plain"), "Plain");
}

#[test]
fn copies_with_one_message_id_are_one_message_with_both_labels() {
    let mut index = mailbox();
    let unread = Location {
        seen: false,
        ..at(INBOX, 11)
    };
    index.add(unread, &headers("<a@x>", ""), 1_000, ME);
    index.add(at(SENT, 3), &headers("<a@x>", ""), 900, ME);
    index.add(at(ARCHIVE, 5), &headers("<b@x>", ""), 2_000, ME);

    assert_eq!(index.entries.len(), 2);
    let both = &index.entries[&index::key("<a@x>")];
    assert_eq!(both.date, 1_000);
    assert_eq!(index.labels(both), ["INBOX", "SENT", "UNREAD"]);
    let archived = &index.entries[&index::key("<b@x>")];
    assert!(index.labels(archived).is_empty());
}

#[test]
fn a_reply_joins_the_thread_of_the_message_it_answers() {
    let mut index = mailbox();
    index.add(at(INBOX, 1), &headers("<first@x>", ""), 1, ME);
    index.add(
        at(SENT, 2),
        &headers(
            "<second@x>",
            "In-Reply-To: <first@x>\r\nReferences: <first@x>",
        ),
        2,
        ME,
    );
    index.add(
        at(INBOX, 3),
        &headers(
            "<third@x>",
            "In-Reply-To: <second@x>\r\nReferences: <first@x>\r\n <second@x>",
        ),
        3,
        ME,
    );
    let thread = index::key("<first@x>");
    assert_eq!(index.entries[&thread].thread_id, thread);
    assert_eq!(index.thread(&thread).len(), 3);
}

#[test]
fn only_mail_outside_trash_and_junk_is_listed() {
    let mut index = mailbox();
    index.add(at(TRASH, 1), &headers("<gone@x>", ""), 1, ME);
    index.add(at(INBOX, 2), &headers("<here@x>", ""), 2, ME);
    let listed: Vec<&str> = index.listed().map(|entry| entry.id.as_str()).collect();
    assert_eq!(listed, [index::key("<here@x>")]);
}

fn entry_in(locations: &[Location]) -> (Index, String) {
    let mut index = mailbox();
    for location in locations {
        index.add(location.clone(), &headers("<m@x>", ""), 1, ME);
    }
    (index, index::key("<m@x>"))
}

fn plan(locations: &[Location], add: &[&str], remove: &[&str]) -> Result<Vec<Step>, String> {
    let (index, id) = entry_in(locations);
    let strings = |labels: &[&str]| {
        labels
            .iter()
            .map(|label| label.to_string())
            .collect::<Vec<_>>()
    };
    index::file(&index, &index.entries[&id], &strings(add), &strings(remove))
}

#[test]
fn archiving_moves_mail_out_of_the_inbox_into_archive() {
    assert_eq!(
        plan(&[at(INBOX, 4)], &[], &["INBOX"]),
        Ok(vec![Step::Move {
            location: at(INBOX, 4),
            to: ARCHIVE
        }])
    );
    // Undo: back from Archive.
    assert_eq!(
        plan(&[at(ARCHIVE, 9)], &["INBOX"], &[]),
        Ok(vec![Step::Move {
            location: at(ARCHIVE, 9),
            to: INBOX
        }])
    );
}

#[test]
fn a_label_is_a_copy_in_that_folder() {
    assert_eq!(
        plan(&[at(INBOX, 4)], &["Folder_Receipts"], &[]),
        Ok(vec![Step::Copy {
            location: at(INBOX, 4),
            to: RECEIPTS
        }])
    );
    // Archiving mail that is in a folder too just leaves the inbox.
    assert_eq!(
        plan(&[at(INBOX, 4), at(RECEIPTS, 8)], &[], &["INBOX"]),
        Ok(vec![Step::Remove {
            location: at(INBOX, 4)
        }])
    );
    assert_eq!(
        plan(&[at(INBOX, 4)], &["Folder_Taxes"], &[]),
        Err("iCloud has no folder Taxes".to_owned())
    );
}

#[test]
fn read_and_starred_are_flags_on_every_copy() {
    assert_eq!(
        plan(&[at(INBOX, 4), at(SENT, 2)], &["STARRED"], &["UNREAD"]),
        Ok(vec![
            Step::Flag {
                location: at(INBOX, 4),
                flag: "\\Seen",
                on: true
            },
            Step::Flag {
                location: at(SENT, 2),
                flag: "\\Seen",
                on: true
            },
            Step::Flag {
                location: at(INBOX, 4),
                flag: "\\Flagged",
                on: true
            },
            Step::Flag {
                location: at(SENT, 2),
                flag: "\\Flagged",
                on: true
            },
        ])
    );
}

#[test]
fn mail_only_in_sent_stays_out_of_the_inbox() {
    assert_eq!(plan(&[at(SENT, 2)], &["INBOX"], &[]), Ok(vec![]));
    assert_eq!(plan(&[at(SENT, 2)], &[], &["INBOX"]), Ok(vec![]));
}

#[test]
fn trash_and_back() {
    let (index, id) = entry_in(&[at(INBOX, 4)]);
    assert_eq!(
        index::trash(&index, &index.entries[&id]),
        Ok(vec![Step::Move {
            location: at(INBOX, 4),
            to: TRASH
        }])
    );
    let (index, id) = entry_in(&[at(TRASH, 12)]);
    assert_eq!(
        index::untrash(&index, &index.entries[&id]),
        Ok(vec![Step::Move {
            location: at(TRASH, 12),
            to: INBOX
        }])
    );
    // Your own mail goes back to Sent.
    let mut index = mailbox();
    index.add(
        at(TRASH, 13),
        b"Message-ID: <mine@x>\r\nFrom: Ana <ANA@icloud.com>\r\n\r\n",
        1,
        ME,
    );
    let mine = &index.entries[&index::key("<mine@x>")];
    assert_eq!(
        index::untrash(&index, mine),
        Ok(vec![Step::Move {
            location: at(TRASH, 13),
            to: SENT
        }])
    );
}

const RECEIPT: &str = "From: Shop <orders@shop.example>\r\n\
To: ana@icloud.com\r\n\
Subject: =?UTF-8?Q?Your_receipt_=E2=80=94_order_42?=\r\n\
Message-ID: <r42@shop.example>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
\r\n\
--outer\r\n\
Content-Type: multipart/related; boundary=\"inner\"\r\n\
\r\n\
--inner\r\n\
Content-Type: text/html; charset=ISO-8859-1\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
<p>Thanks! Total =A342.00</p><img src=3D\"cid:logo@shop\">\r\n\
--inner\r\n\
Content-Type: image/png\r\n\
Content-ID: <logo@shop>\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0K\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: application/pdf; name=\"receipt.pdf\"\r\n\
Content-Disposition: attachment; filename=\"receipt.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--outer--\r\n";

fn meta() -> mime::Meta {
    mime::Meta {
        id: "abc".into(),
        thread_id: "abc".into(),
        label_ids: vec!["INBOX".into()],
        internal_date: 1_793_610_307_000,
        size: RECEIPT.len() as i64,
    }
}

#[test]
fn a_message_from_imap_reads_like_one_from_gmail() {
    let location = at(INBOX, 31);
    let api = mime::full(RECEIPT.as_bytes(), meta(), &|path| {
        attachment_id("INBOX", &location, path)
    });
    let message = parse_message(api);
    assert_eq!(message.subject, "Your receipt — order 42");
    assert_eq!(message.from_addr, "orders@shop.example");
    assert_eq!(message.internal_date, 1_793_610_307_000);
    assert_eq!(
        message.message_id_header.as_deref(),
        Some("<r42@shop.example>")
    );
    assert!(message.body_html.as_deref().unwrap().contains("£42.00"));
    assert_eq!(message.snippet, "Thanks! Total £42.00");

    let files: Vec<(&str, &str, bool)> = message
        .attachments
        .iter()
        .map(|file| {
            (
                file.part_id.as_str(),
                file.gmail_attachment_id.as_deref().unwrap(),
                file.shown_in_html,
            )
        })
        .collect();
    assert_eq!(files, [("2", "7:31:2@INBOX", false)]);
    assert_eq!(message.attachments[0].filename, "receipt.pdf");
    assert_eq!(message.attachments[0].size, 9);

    // The logo isn't a file sent with it, but it can be read as one.
    assert_eq!(
        mime::part_data(RECEIPT.as_bytes(), "1.2").unwrap(),
        b"\x89PNG\r\n"
    );
    assert_eq!(
        mime::part_data(RECEIPT.as_bytes(), "2").unwrap(),
        b"%PDF-1.4\n"
    );
    assert_eq!(mime::part_data(RECEIPT.as_bytes(), "3"), None);
}

#[test]
fn a_plain_message_is_its_own_part_one() {
    let raw = b"Subject: Hi\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSee you at 6.\r\n";
    let message = parse_message(mime::full(raw, meta(), &|path| path.to_owned()));
    assert_eq!(message.body_text.trim(), "See you at 6.");
    assert_eq!(mime::part_data(raw, "1").unwrap(), b"See you at 6.\r\n");
}

#[test]
fn headers_alone_make_a_message_waiting_for_its_body() {
    let raw = RECEIPT.split("\r\n\r\n").next().unwrap();
    let message = parse_message(mime::headers_only(raw.as_bytes(), meta()));
    assert_eq!(message.subject, "Your receipt — order 42");
    assert!(message.body_text.is_empty());
}

#[test]
fn attachment_ids_round_trip() {
    let id = attachment_id("Sent Messages", &at(SENT, 77), "1.2");
    assert_eq!(id, "7:77:1.2@Sent Messages");
    let (folder, location, path) = parse_attachment_id(&id).unwrap();
    assert_eq!(
        (
            folder.as_str(),
            location.uid_validity,
            location.uid,
            path.as_str()
        ),
        ("Sent Messages", 7, 77, "1.2")
    );
    assert!(parse_attachment_id("not an id").is_none());
}

#[test]
fn a_saved_draft_gets_its_own_date_and_message_id() {
    let raw = b"From: ana@icloud.com\r\nTo: bo@example.org\r\nBcc: cy@example.net\r\nMessage-ID: <old@x>\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\nSubject: Hi\r\n\r\nBody\r\n";
    let stamped = String::from_utf8(mime::stamped(raw, "<new@icloud.com>", false)).unwrap();
    assert!(stamped.starts_with("Date: "));
    assert!(stamped.contains("\r\nMessage-ID: <new@icloud.com>\r\n"));
    assert!(!stamped.contains("<old@x>") && !stamped.contains("2024"));
    assert!(stamped.contains("Bcc: cy@example.net\r\n"));
    assert!(stamped.ends_with("Subject: Hi\r\n\r\nBody\r\n"));

    let outgoing = String::from_utf8(mime::stamped(raw, "<new@icloud.com>", true)).unwrap();
    assert!(!outgoing.contains("Bcc"));
    assert_eq!(mime::recipients(raw), ["bo@example.org", "cy@example.net"]);
}

#[test]
fn recipients_include_names_and_groups() {
    let raw = b"To: \"Lima, Ana\" <ana@example.org>, bo@example.org\r\nCc: Team: cy@example.net, di@example.net;\r\n\r\n";
    assert_eq!(
        mime::recipients(raw),
        [
            "ana@example.org",
            "bo@example.org",
            "cy@example.net",
            "di@example.net"
        ]
    );
}

#[test]
fn new_message_ids_are_at_the_senders_domain() {
    let id = mime::new_message_id(ME);
    assert!(id.starts_with('<') && id.ends_with("@icloud.com>"), "{id}");
    assert_ne!(id, mime::new_message_id(ME));
}

#[test]
fn dates_are_written_as_mail_and_imap_want_them() {
    assert_eq!(mime::rfc2822(0), "Thu, 1 Jan 1970 00:00:00 +0000");
    assert_eq!(
        mime::rfc2822(1_793_610_307),
        "Mon, 2 Nov 2026 09:05:07 +0000"
    );
}

#[test]
fn changing_one_thread_keeps_the_index_for_the_others() {
    let client = super::IcloudClient::new(
        ME,
        super::Login {
            user: "ana".into(),
            password: "fake".into(),
        },
    );
    let mut index = mailbox();
    index.add(at(INBOX, 1), &headers("<a@x>", ""), 1, ME);
    index.add(at(INBOX, 2), &headers("<b@x>", ""), 2, ME);
    let index = std::sync::Arc::new(index);
    client.cache.lock().unwrap().index = Some(index.clone());

    client.mark_changed(&index::key("<a@x>"));

    // Reading the other thread needs no rescan (and so no connection).
    let other = index::key("<b@x>");
    let again = client.index_for(|_| Some(other.clone())).unwrap();
    assert!(std::sync::Arc::ptr_eq(&index, &again));
    assert!(client.cache.lock().unwrap().index.is_some());
}
