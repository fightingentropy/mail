use base64::engine::general_purpose::STANDARD;

use super::*;
use crate::{gmail::decode_words, sync::tests::FakeGmail};

const ME: &str = "me@example.com";

fn original() -> Message {
    Message {
        gmail_id: "m1".into(),
        thread_id: "t1".into(),
        subject: "Re: Plan for Q4".into(),
        from_name: "Ana Lima".into(),
        from_addr: "ana@example.org".into(),
        to: "Me <me@example.com>, Bo <bo@example.com>".into(),
        cc: "\"Team, Product\" <team@example.com>, ana@example.org".into(),
        body_text: "Draft attached.\n\nThoughts by Friday?".into(),
        message_id_header: Some("<abc@example.org>".into()),
        references: Some("<root@example.com>".into()),
        ..Message::default()
    }
}

/// Header lines and the decoded body of an RFC 5322 message.
fn parse(raw: &[u8]) -> (Vec<String>, String) {
    let text = String::from_utf8(raw.to_vec()).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let headers = head
        .replace("\r\n ", " ")
        .split("\r\n")
        .map(str::to_owned)
        .collect();
    let body = STANDARD.decode(body.replace("\r\n", "")).unwrap();
    (headers, String::from_utf8(body).unwrap())
}

fn header<'a>(headers: &'a [String], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}: ")))
}

#[test]
fn reply_goes_to_the_sender_and_threads() {
    let draft = Draft::reply(&original(), ME, false, "Sun 1 Nov 2026 18:06");
    assert_eq!(draft.to, "Ana Lima <ana@example.org>");
    assert_eq!(draft.cc, "");
    assert_eq!(draft.subject, "Re: Plan for Q4");
    let reply = draft.reply.as_ref().unwrap();
    assert_eq!(reply.thread_id, "t1");
    assert_eq!(reply.in_reply_to.as_deref(), Some("<abc@example.org>"));
    assert_eq!(
        reply.references.as_deref(),
        Some("<root@example.com> <abc@example.org>")
    );
    assert!(draft.body.starts_with(
        "\n\nOn Sun 1 Nov 2026 18:06, Ana Lima wrote:\n> Draft attached.\n>\n> Thoughts by Friday?"
    ));
}

#[test]
fn reply_all_copies_everyone_once_except_me() {
    let draft = Draft::reply(&original(), ME, true, "today");
    assert_eq!(draft.to, "Ana Lima <ana@example.org>");
    assert_eq!(
        draft.cc,
        "Bo <bo@example.com>, \"Team, Product\" <team@example.com>"
    );
}

#[test]
fn replying_to_my_own_message_goes_to_its_recipients() {
    let mine = Message {
        from_name: String::new(),
        from_addr: "ME@example.com".into(),
        to: "Bo <bo@example.com>".into(),
        cc: "carla@example.com".into(),
        ..original()
    };
    let draft = Draft::reply(&mine, ME, true, "today");
    assert_eq!(draft.to, "Bo <bo@example.com>");
    assert_eq!(draft.cc, "carla@example.com");
}

#[test]
fn subjects_get_one_re() {
    assert_eq!(reply_subject("Re: RE:re: Lunch"), "Re: Lunch");
    assert_eq!(reply_subject("Lunch"), "Re: Lunch");
    assert_eq!(reply_subject("Rebooking"), "Re: Rebooking");
}

#[test]
fn address_lists_split_outside_quotes_and_brackets() {
    assert_eq!(
        split_address_list("\"Lima, Ana\" <ana@example.org>, bo@example.com; <c,d@example.com>"),
        [
            "\"Lima, Ana\" <ana@example.org>",
            "bo@example.com",
            "<c,d@example.com>"
        ]
    );
    assert!(split_address_list(" , ").is_empty());
}

#[test]
fn builds_a_plain_text_message() {
    let draft = Draft::reply(&original(), ME, true, "today");
    let (headers, body) = parse(&draft.to_rfc5322(ME));
    assert_eq!(header(&headers, "From"), Some(ME));
    assert_eq!(header(&headers, "To"), Some("Ana Lima <ana@example.org>"));
    assert_eq!(
        header(&headers, "Cc"),
        Some("Bo <bo@example.com>, \"Team, Product\" <team@example.com>")
    );
    assert_eq!(header(&headers, "Subject"), Some("Re: Plan for Q4"));
    assert_eq!(header(&headers, "In-Reply-To"), Some("<abc@example.org>"));
    assert_eq!(
        header(&headers, "Content-Type"),
        Some("text/plain; charset=UTF-8")
    );
    assert!(
        body.contains("\r\n> Thoughts by Friday?"),
        "lines end in CRLF"
    );
}

#[test]
fn line_breaks_cannot_inject_headers() {
    let draft = Draft {
        to: "bo@example.com\r\nBcc: spy@example.net".into(),
        subject: "Hello\r\nBcc: spy@example.net".into(),
        body: "Hi".into(),
        ..Draft::default()
    };
    let (headers, _) = parse(&draft.to_rfc5322(ME));
    assert!(headers.iter().all(|line| !line.starts_with("Bcc:")));
    assert_eq!(
        header(&headers, "Subject"),
        Some("Hello  Bcc: spy@example.net")
    );
}

#[test]
fn non_ascii_subjects_and_names_are_encoded() {
    let subject = "Café on Friday, and a long subject that needs folding across lines ☕";
    let draft = Draft {
        to: "José Núñez <jose@example.com>".into(),
        subject: subject.into(),
        body: "Até já".into(),
        ..Draft::default()
    };
    let (headers, body) = parse(&draft.to_rfc5322(ME));
    let encoded = header(&headers, "Subject").unwrap();
    assert!(encoded.is_ascii());
    assert!(encoded.split(' ').all(|word| word.len() <= 75));
    assert_eq!(decode_words(encoded), subject);
    assert_eq!(
        decode_words(header(&headers, "To").unwrap()),
        "José Núñez <jose@example.com>"
    );
    assert_eq!(body, "Até já");
}

#[test]
fn flags_entries_that_are_not_addresses() {
    let draft = Draft {
        to: "Ana <ana@example.org>, bob, carla@localhost".into(),
        cc: "dan @example.com".into(),
        ..Draft::default()
    };
    assert_eq!(
        draft.invalid_addresses(),
        ["bob", "carla@localhost", "dan @example.com"]
    );
}

#[test]
fn saving_creates_then_updates_and_recreates_a_deleted_draft() {
    let gmail = FakeGmail::new(Vec::new());
    let draft = Draft::reply(&original(), ME, false, "today");

    let id = save(&gmail, ME, &draft, None).unwrap();
    let (raw, thread) = gmail.draft(&id).unwrap();
    assert_eq!(thread.as_deref(), Some("t1"));
    assert!(
        String::from_utf8(raw)
            .unwrap()
            .contains("Subject: Re: Plan for Q4")
    );

    let edited = Draft {
        body: "Sounds good.".into(),
        ..draft.clone()
    };
    assert_eq!(save(&gmail, ME, &edited, Some(&id)).unwrap(), id);

    gmail.delete_draft(&id).unwrap();
    let again = save(&gmail, ME, &edited, Some(&id)).unwrap();
    assert_ne!(again, id);
    assert_eq!(
        gmail.calls(),
        [
            "create_draft draft-1",
            "update_draft draft-1",
            "delete_draft draft-1",
            "create_draft draft-2"
        ]
    );
}

#[test]
fn saving_refuses_bad_addresses_and_huge_bodies() {
    let gmail = FakeGmail::new(Vec::new());
    let bad = Draft {
        to: "not an address".into(),
        ..Draft::default()
    };
    assert!(save(&gmail, ME, &bad, None).is_err());
    let huge = Draft {
        body: "x".repeat(MAX_BODY + 1),
        ..Draft::default()
    };
    assert!(save(&gmail, ME, &huge, None).is_err());
    assert!(gmail.calls().is_empty());
}
