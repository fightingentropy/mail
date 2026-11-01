use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;

use super::*;

fn b64(bytes: impl AsRef<[u8]>) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn api(value: serde_json::Value) -> ApiMessage {
    serde_json::from_value(value).unwrap()
}

#[test]
fn multipart_message_with_attachment() {
    let message = parse_message(api(json!({
        "id": "18c0ffee",
        "threadId": "18c0ffe0",
        "labelIds": ["INBOX", "UNREAD"],
        "snippet": "Hi Bo, the plan&#39;s attached",
        "internalDate": "1761998400000",
        "sizeEstimate": 48213,
        "payload": {
            "partId": "",
            "mimeType": "multipart/mixed",
            "headers": [
                {"name": "From", "value": "\"Ana Lima\" <ana@example.org>"},
                {"name": "To", "value": "Bo <bo@example.com>"},
                {"name": "Cc", "value": "team@example.com"},
                {"name": "Subject", "value": "Plan for Q4"},
                {"name": "Message-ID", "value": "<abc@example.org>"},
                {"name": "In-Reply-To", "value": "<prev@example.com>"},
                {"name": "References", "value": "<root@example.com> <prev@example.com>"}
            ],
            "body": {"size": 0},
            "parts": [
                {
                    "partId": "0",
                    "mimeType": "multipart/alternative",
                    "body": {"size": 0},
                    "parts": [
                        {
                            "partId": "0.0",
                            "mimeType": "text/plain",
                            "headers": [{"name": "Content-Type", "value": "text/plain; charset=\"UTF-8\""}],
                            "body": {"size": 30, "data": b64("Hi Bo,\r\n\r\n  The plan's attached.\r\n")}
                        },
                        {
                            "partId": "0.1",
                            "mimeType": "text/html",
                            "headers": [{"name": "Content-Type", "value": "text/html; charset=UTF-8"}],
                            "body": {"size": 40, "data": b64("<p>Hi Bo,</p><p>The plan's attached.</p>")}
                        }
                    ]
                },
                {
                    "partId": "1",
                    "mimeType": "application/pdf",
                    "filename": "plan.pdf",
                    "headers": [{"name": "Content-Disposition", "value": "attachment; filename=\"plan.pdf\""}],
                    "body": {"size": 45000, "attachmentId": "ANGjdJ8"}
                }
            ]
        }
    })));

    assert_eq!(message.gmail_id, "18c0ffee");
    assert_eq!(message.thread_id, "18c0ffe0");
    assert_eq!(message.internal_date, 1_761_998_400_000);
    assert_eq!(
        (message.from_name.as_str(), message.from_addr.as_str()),
        ("Ana Lima", "ana@example.org")
    );
    assert_eq!(message.to, "Bo <bo@example.com>");
    assert_eq!(message.cc, "team@example.com");
    assert_eq!(message.subject, "Plan for Q4");
    assert_eq!(message.snippet, "Hi Bo, the plan's attached");
    assert_eq!(message.body_text, "Hi Bo,\n\n  The plan's attached.");
    assert_eq!(
        message.body_html.as_deref(),
        Some("<p>Hi Bo,</p><p>The plan's attached.</p>")
    );
    assert_eq!(
        message.message_id_header.as_deref(),
        Some("<abc@example.org>")
    );
    assert_eq!(
        message.references.as_deref(),
        Some("<root@example.com> <prev@example.com>")
    );
    assert_eq!(message.label_ids, ["INBOX", "UNREAD"]);
    assert_eq!(
        message.attachments,
        [Attachment {
            part_id: "1".into(),
            filename: "plan.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 45000,
            gmail_attachment_id: Some("ANGjdJ8".into()),
        }]
    );
}

#[test]
fn html_only_message_gets_a_text_body() {
    let message = parse_message(api(json!({
        "id": "1", "threadId": "1",
        "payload": {
            "mimeType": "text/html",
            "headers": [{"name": "Subject", "value": "Weekly digest"}],
            "body": {"data": b64("<h1>Top stories</h1><p>Rust&nbsp;2026 ships</p>")}
        }
    })));
    assert_eq!(message.body_text, "Top stories\n\nRust 2026 ships");
}

#[test]
fn decodes_declared_charsets() {
    let message = parse_message(api(json!({
        "id": "1", "threadId": "1",
        "payload": {
            "mimeType": "text/plain",
            "headers": [{"name": "Content-Type", "value": "text/plain; charset=ISO-8859-1"}],
            "body": {"data": b64(b"Caf\xe9 at 9")}
        }
    })));
    assert_eq!(message.body_text, "Café at 9");
}

#[test]
fn decodes_encoded_word_headers() {
    assert_eq!(decode_words("=?UTF-8?B?Q2Fmw6k=?= tonight"), "Café tonight");
    assert_eq!(
        decode_words("=?iso-8859-1?Q?Caf=E9_au_lait?="),
        "Café au lait"
    );
    // Whitespace between adjacent encoded words is dropped.
    assert_eq!(
        decode_words("=?UTF-8?Q?Hello?= =?UTF-8?Q?_world?="),
        "Hello world"
    );
    assert_eq!(decode_words("Plain =? not encoded"), "Plain =? not encoded");
}

#[test]
fn splits_addresses() {
    assert_eq!(
        split_address("Ana <ana@example.org>"),
        ("Ana".into(), "ana@example.org".into())
    );
    assert_eq!(
        split_address("\"Lima, Ana\" <ana@example.org>"),
        ("Lima, Ana".into(), "ana@example.org".into())
    );
    assert_eq!(
        split_address("ana@example.org"),
        (String::new(), "ana@example.org".into())
    );
}

#[test]
fn missing_payload_is_an_empty_message() {
    let message = parse_message(api(json!({"id": "1", "threadId": "2"})));
    assert_eq!(message.subject, "");
    assert_eq!(message.body_text, "");
    assert!(message.attachments.is_empty());
}

#[test]
fn a_text_part_stripped_from_html_gives_way_to_the_html() {
    // What many newsletters send as their "plain text".
    let stripped = "Your order is on its way!&nbsp;\n\n\n\n        \n                Order 42 has been dispatched\n";
    let html = r#"<h1>Your order is on its way!</h1><p>Order 42 has been dispatched.</p>
        <p><a href="https://shop.example/track/42">Track your order</a></p>"#;
    let (text, links) = body(Some(stripped), Some(html));
    assert_eq!(
        text,
        "Your order is on its way!\n\nOrder 42 has been dispatched.\n\nTrack your order"
    );
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].url, "https://shop.example/track/42");
}

#[test]
fn a_written_text_part_is_kept_with_its_indentation() {
    let written = "Hi Ana,\r\n\r\n\r\n\r\nThe fix:\r\n\r\n    let x = 1;   \r\n\r\nThanks";
    let (text, links) = body(Some(written), Some("<p>Hi Ana, the fix…</p>"));
    assert_eq!(text, "Hi Ana,\n\nThe fix:\n\n    let x = 1;\n\nThanks");
    assert!(links.is_empty());
}

#[test]
fn a_list_of_tracking_links_counts_as_machine_made() {
    let link = format!("https://click.shop.example/?q={}", "x".repeat(80));
    let plain = format!("Sale\n{link}\n{link}\n{link}\n{link}\n{link}\n");
    let (text, _) = body(
        Some(&plain),
        Some("<p><a href=\"https://shop.example\">Sale</a></p>"),
    );
    assert_eq!(text, "Sale");
}
