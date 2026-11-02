//! Downloads through the in-memory Gmail, including a rotated attachment ID.

use std::fs;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;

use super::*;
use crate::{gmail::ApiMessage, sync::tests::FakeGmail};

const TICKET: &[u8] = b"%PDF-1.7 made-up ticket";

/// A message with a text body and one PDF attachment, as `format=full`
/// returns it.
fn with_ticket(attachment_id: &str) -> ApiMessage {
    serde_json::from_value(json!({
        "id": "m1",
        "threadId": "t1",
        "labelIds": ["INBOX"],
        "internalDate": "1761998400000",
        "payload": {
            "mimeType": "multipart/mixed",
            "headers": [
                {"name": "From", "value": "Rail <tickets@rail.example>"},
                {"name": "Subject", "value": "Your ticket"}
            ],
            "parts": [
                {
                    "partId": "0",
                    "mimeType": "text/plain",
                    "body": {"data": URL_SAFE_NO_PAD.encode("Ticket attached.")}
                },
                {
                    "partId": "1",
                    "mimeType": "application/pdf",
                    "filename": "ticket.pdf",
                    "body": {"size": TICKET.len(), "attachmentId": attachment_id}
                }
            ]
        }
    }))
    .unwrap()
}

/// The fake mailbox and the attachment as sync stored it.
fn mailbox() -> (FakeGmail, Attachment) {
    let gmail = FakeGmail::new(vec![with_ticket("att-1")]);
    gmail.add_attachment("att-1", TICKET);
    let stored = parse_message(with_ticket("att-1")).attachments.remove(0);
    (gmail, stored)
}

#[test]
fn downloads_with_the_stored_id() {
    let (gmail, stored) = mailbox();
    assert_eq!(fetch(&gmail, "m1", &stored).unwrap(), TICKET);
    assert_eq!(gmail.calls(), ["attachment att-1"]);
    assert_eq!(gmail.fetches(), 0, "no need to read the message again");
}

#[test]
fn a_rotated_id_is_refreshed_from_the_message() {
    let (gmail, stored) = mailbox();
    gmail.rotate_attachment("att-1", "att-2");

    assert_eq!(fetch(&gmail, "m1", &stored).unwrap(), TICKET);
    assert_eq!(gmail.calls(), ["attachment att-1", "attachment att-2"]);
    assert_eq!(gmail.fetches(), 1);
}

#[test]
fn network_errors_do_not_spend_quota_on_a_new_id() {
    let (gmail, stored) = mailbox();
    gmail.set_offline(true);
    assert!(fetch(&gmail, "m1", &stored).is_err());
    assert_eq!(gmail.fetches(), 0);
}

#[test]
fn a_message_deleted_in_gmail_says_so() {
    let (gmail, mut stored) = mailbox();
    stored.gmail_attachment_id = None;
    let error = fetch(&gmail, "gone", &stored).unwrap_err();
    assert_eq!(
        error.to_string(),
        "The message is no longer in your mailbox"
    );
}

#[test]
fn saving_never_replaces_an_existing_file() {
    let folder = tempfile::tempdir().unwrap();
    let first = save(folder.path(), "ticket.pdf", b"one").unwrap();
    let second = save(folder.path(), "ticket.pdf", b"two").unwrap();
    let third = save(folder.path(), "ticket.pdf", b"three").unwrap();

    let names: Vec<_> = [&first, &second, &third]
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["ticket.pdf", "ticket (1).pdf", "ticket (2).pdf"]);
    assert_eq!(fs::read(&first).unwrap(), b"one");
    assert_eq!(fs::read(&third).unwrap(), b"three");
}

#[test]
fn names_without_an_extension_are_numbered_too() {
    let folder = tempfile::tempdir().unwrap();
    save(folder.path(), "README", b"a").unwrap();
    let second = save(folder.path(), "README", b"b").unwrap();
    assert_eq!(second.file_name().unwrap(), "README (1)");
}

#[test]
fn unsafe_names_stay_in_the_folder_and_visible() {
    assert_eq!(safe_filename("../../.zshrc"), "zshrc");
    assert_eq!(safe_filename("C:\\Users\\ana\\notes.txt"), "notes.txt");
    assert_eq!(safe_filename(".."), "attachment");
    assert_eq!(safe_filename("  "), "attachment");
    assert_eq!(safe_filename("tab\there.txt"), "tab_here.txt");
    // U+202E would display "invoice\u{202e}fdp.app" as "invoiceppa.pdf".
    assert_eq!(safe_filename("invoice\u{202e}fdp.app"), "invoicefdp.app");

    let long = format!("{}.pdf", "a".repeat(400));
    let shortened = safe_filename(&long);
    assert!(shortened.len() <= 240);
    assert!(shortened.ends_with("a.pdf"));
}
