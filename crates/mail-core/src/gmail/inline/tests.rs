//! Finding, fetching and embedding pictures sent inside a message.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;

use super::*;
use crate::{gmail::ApiMessage, sync::tests::FakeGmail};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n made-up logo";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0 made-up stamp";

fn image(content_id: &str, data: &[u8]) -> InlineImage {
    InlineImage {
        content_id: content_id.into(),
        mime_type: "image/png".into(),
        data: data.to_vec(),
    }
}

#[test]
fn references_are_found_in_attributes_and_css_but_not_in_words() {
    let html = r#"<img src="cid:logo@shop.example"><img src='cid:logo@shop.example'>
        <td background=cid:paper style="background: url(cid:stripe%40shop.example)">
        <div style="background:url(&quot;cid:quoted&quot;)">An acid: test. Write cid: here.</div>"#;
    assert_eq!(
        content_ids(html),
        [
            "logo@shop.example",
            "paper",
            "stripe@shop.example",
            "quoted"
        ]
    );
    assert!(content_ids("<p>No pictures</p>").is_empty());
}

#[test]
fn known_pictures_are_put_in_place_and_others_left() {
    let html = r#"<img src="CID:logo"><img src="cid:gone"><p>acid:logo</p>"#;
    let embedded = embed(html, &[image("logo", PNG)]);
    let data = format!("data:image/png;base64,{}", STANDARD.encode(PNG));
    assert_eq!(
        embedded,
        format!(r#"<img src="{data}"><img src="cid:gone"><p>acid:logo</p>"#)
    );
    assert!(matches!(embed(html, &[]), Cow::Borrowed(_)));
}

#[test]
fn content_ids_match_regardless_of_case() {
    let embedded = embed(r#"<img src="cid:Logo@Shop">"#, &[image("logo@shop", PNG)]);
    assert!(embedded.contains("data:image/png"));
}

/// A receipt as `format=full` returns it: HTML, then the pictures it shows.
fn receipt() -> ApiMessage {
    let part = |id: &str, mime: &str, content_id: &str, body: serde_json::Value| {
        json!({
            "partId": id,
            "mimeType": mime,
            "headers": [{"name": "Content-ID", "value": format!("<{content_id}>")}],
            "body": body
        })
    };
    serde_json::from_value(json!({
        "id": "m1",
        "threadId": "t1",
        "payload": {
            "mimeType": "multipart/related",
            "parts": [
                {
                    "partId": "0",
                    "mimeType": "text/html",
                    "body": {"data": URL_SAFE_NO_PAD.encode("<img src=\"cid:logo\">")}
                },
                // Labelled as anything; the bytes say it is a JPEG.
                part("1", "application/octet-stream", "stamp", json!({"size": JPEG.len(), "attachmentId": "att-stamp"})),
                part("2", "image/png", "Logo", json!({"size": PNG.len(), "attachmentId": "att-logo"})),
                part("3", "image/png", "tiny", json!({"size": PNG.len(), "data": URL_SAFE_NO_PAD.encode(PNG)})),
                part("4", "application/pdf", "ticket", json!({"size": 20, "attachmentId": "att-ticket"})),
                part("5", "image/png", "huge", json!({"size": MAX_SIZE + 1, "attachmentId": "att-huge"}))
            ]
        }
    }))
    .unwrap()
}

#[test]
fn fetches_only_the_pictures_asked_for() {
    let gmail = FakeGmail::new(vec![receipt()]);
    gmail.add_attachment("att-stamp", JPEG);
    gmail.add_attachment("att-logo", PNG);
    gmail.add_attachment("att-ticket", b"%PDF-1.7 made-up ticket");
    gmail.add_attachment("att-huge", PNG);

    let wanted = ["logo", "stamp", "tiny", "ticket", "huge", "missing"].map(String::from);
    let images = fetch(&gmail, "m1", &wanted).unwrap();
    let found: Vec<(&str, &str)> = images
        .iter()
        .map(|image| (image.content_id.as_str(), image.mime_type.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            ("logo", "image/png"),
            ("stamp", "image/jpeg"),
            ("tiny", "image/png")
        ]
    );
    assert_eq!(images[2].data, PNG, "small parts come with the message");
    assert_eq!(gmail.fetches(), 1, "the message is read once");
    assert_eq!(
        gmail.calls(),
        ["attachment att-logo", "attachment att-stamp"],
        "nothing labelled as a document or too big to be a picture is downloaded"
    );
}

#[test]
fn a_message_deleted_in_gmail_says_so() {
    let gmail = FakeGmail::new(Vec::new());
    let error = fetch(&gmail, "m1", &["logo".into()]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "The message is no longer in your mailbox"
    );
}
