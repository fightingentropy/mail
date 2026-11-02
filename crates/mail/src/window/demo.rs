//! A made-up mailbox for screenshots, tests and trying the window without a
//! Google account. Every name and address is fictional.

use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use mail_core::{
    model::{Attachment, Label, LabelKind, Message},
    store::Store,
};

pub const ADDRESS: &str = "you@example.com";
/// A second, smaller account, so the demo shows several accounts.
pub const WORK_ADDRESS: &str = "you@studio.example";

const HOUR: i64 = 3_600_000;

struct Mail {
    thread: &'static str,
    hours_ago: i64,
    from: (&'static str, &'static str),
    subject: &'static str,
    body: &'static str,
    labels: &'static [&'static str],
    attachment: Option<(&'static str, &'static str, i64)>,
    /// An HTML version, for `h`.
    html: Option<&'static str>,
}

const MAILBOX: &[Mail] = &[
    Mail {
        thread: "t-offer",
        hours_ago: 1,
        from: ("Priya Raman", "priya@lumen.example"),
        subject: "Next steps: senior full-stack engineer",
        body: "Hi,\n\nThanks again for the conversation on Thursday. The team enjoyed the systems design discussion, especially the offline sync trade-offs.\n\nWe'd like to invite you to a final round next week: a pairing session in the morning and a chat with our CTO after lunch. Could you share a few times that work between Tuesday and Thursday?\n\nBest,\nPriya",
        labels: &["INBOX", "UNREAD", "IMPORTANT", "Label_work"],
        attachment: None,
        html: None,
    },
    Mail {
        thread: "t-rail",
        hours_ago: 3,
        from: ("Northline Rail", "tickets@northline.example"),
        subject: "Your e-ticket: London to Edinburgh",
        body: "Booking reference QX7-41K\n\nSat 15 Nov, depart 08:00, arrive 12:21\nCoach C, seat 42 (window, table)\n\nShow the attached ticket on your phone or print it. Your seat reservation is valid only on this train.\n\nChange seats or add a railcard at https://northline.example/booking/QX7-41K",
        labels: &["INBOX", "UNREAD", "Label_travel"],
        attachment: Some(("eticket-QX741K.pdf", "application/pdf", 84_000)),
        html: None,
    },
    Mail {
        thread: "t-review",
        hours_ago: 5,
        from: ("Sam Okafor", "sam@buildkite.test"),
        subject: "Code review: offline queue for triage",
        body: "Looked through the pending-changes queue. The re-apply after sync is a nice touch; it fixes the flicker I saw when archiving during a sync.\n\nOne question: should refused changes surface somewhere, or is dropping them silently enough for v1?",
        labels: &["INBOX", "Label_work"],
        attachment: None,
        html: None,
    },
    Mail {
        thread: "t-review",
        hours_ago: 4,
        from: ("Alex Rivera", ADDRESS),
        subject: "Re: Code review: offline queue for triage",
        body: "Good question. For v1 the status bar reports them after sync, and the next sync shows Gmail's state anyway. I'll add a log view if it turns out to matter.",
        labels: &["SENT"],
        attachment: None,
        html: None,
    },
    Mail {
        thread: "t-review",
        hours_ago: 2,
        from: ("Sam Okafor", "sam@buildkite.test"),
        subject: "Re: Code review: offline queue for triage",
        body: "Works for me. Approved. Ship it.",
        labels: &["INBOX", "UNREAD", "Label_work"],
        attachment: None,
        html: None,
    },
    Mail {
        thread: "t-energy",
        hours_ago: 26,
        from: ("Brightwater Energy", "bills@brightwater.example"),
        subject: "Your October bill is ready",
        body: "Amount due: £64.18\nDirect debit on 18 Nov.\n\nThis month you used 9% less electricity than last October.",
        labels: &["INBOX", "Label_receipts"],
        attachment: Some(("bill-october.pdf", "application/pdf", 120_000)),
        html: None,
    },
    Mail {
        thread: "t-dinner",
        hours_ago: 30,
        from: ("Maya Chen", "maya@fastmail.test"),
        subject: "Dinner Friday?",
        body: "There's a new Georgian place near Borough Market. 7:30? I can book for four if Leo and Ana are around.",
        labels: &["INBOX", "STARRED"],
        attachment: None,
        html: None,
    },
    Mail {
        thread: "t-newsletter",
        hours_ago: 2 * 24 + 4,
        from: ("Rust Weekly", "editor@rustweekly.example"),
        subject: "This week: async traits, SQLite tips and egui 0.36",
        body: "Top stories\n\n- Writing a sync engine that survives quota limits\n- SQLite FTS5: trigram or unicode61?\n- What's new in egui 0.36\n\nRead online: https://rustweekly.example/issues/212\nUnsubscribe: https://rustweekly.example/unsubscribe",
        labels: &["INBOX", "CATEGORY_UPDATES"],
        attachment: None,
        html: Some(NEWSLETTER_HTML),
    },
    Mail {
        thread: "t-flat",
        hours_ago: 3 * 24,
        from: ("Hollis Lettings", "team@hollis.example"),
        subject: "Inspection on 21 November",
        body: "Hello,\n\nA routine inspection of the flat is booked for Friday 21 November between 10:00 and 12:00. You don't need to be home. Reply if the time doesn't work.\n\nKind regards,\nHollis Lettings",
        labels: &["INBOX"],
        attachment: None,
        html: None,
    },
    Mail {
        thread: "t-receipt",
        hours_ago: 4 * 24,
        from: ("Paper & Co", "orders@paperco.example"),
        subject: "Receipt for order 20931",
        body: "Thanks for your order.\n\n1 × A5 dot-grid notebook  £14.00\n1 × fineliner set         £9.50\nDelivery                 free\nTotal                    £23.50",
        labels: &["Label_receipts"],
        attachment: None,
        html: None,
    },
    Mail {
        thread: "t-meetup",
        hours_ago: 6 * 24,
        from: ("London Rust", "hello@ldnrust.example"),
        subject: "Talk accepted: building a native email client",
        body: "Great news, your talk is accepted for the December meetup. 25 minutes plus questions. Slides by the 5th, please.",
        labels: &["INBOX", "STARRED", "Label_work"],
        attachment: None,
        html: None,
    },
];

const NEWSLETTER_HTML: &str = r#"<!DOCTYPE html>
<html><head><title>Rust Weekly</title></head>
<body style="font-family: -apple-system, sans-serif; max-width: 600px; margin: 40px auto; color: #222">
<h1 style="font-size: 28px">Rust Weekly</h1>
<h2 style="font-size: 18px">Top stories</h2>
<ul>
<li><a href="https://rustweekly.example/issues/212#sync">Writing a sync engine that survives quota limits</a></li>
<li><a href="https://rustweekly.example/issues/212#fts5">SQLite FTS5: trigram or unicode61?</a></li>
<li><a href="https://rustweekly.example/issues/212#egui">What's new in egui 0.36</a></li>
</ul>
<p><a href="https://rustweekly.example/unsubscribe">Unsubscribe</a></p>
</body></html>"#;

const WORK_MAILBOX: &[Mail] = &[
    Mail {
        thread: "w-onboarding",
        hours_ago: 2,
        from: ("Lena Brandt", "lena@studio.example"),
        subject: "Comments on the onboarding flow",
        body: "Left a few comments on the second and third screens. The copy is close; the empty state needs one more pass.\n\nCan we look at it together tomorrow morning?",
        labels: &["INBOX", "UNREAD", "Label_clients"],
        attachment: None,
        html: None,
    },
    Mail {
        thread: "w-standup",
        hours_ago: 8,
        from: ("Omar Haddad", "omar@studio.example"),
        subject: "Standup moves to 10:30",
        body: "From Monday, standup is at 10:30 so the Lisbon folks can join. Same room, same link.",
        labels: &["INBOX", "UNREAD"],
        attachment: None,
        html: None,
    },
    Mail {
        thread: "w-invoice",
        hours_ago: 30,
        from: ("Ledgerly", "billing@ledgerly.example"),
        subject: "Invoice 2026-114 paid",
        body: "Thanks! We received your payment for invoice 2026-114.",
        labels: &["INBOX"],
        attachment: None,
        html: None,
    },
];

/// Fills a new database at `path` with the made-up mailbox.
pub fn seed(path: &Path) -> anyhow::Result<()> {
    let labels = [
        ("Label_receipts", "Receipts"),
        ("Label_travel", "Travel"),
        ("Label_work", "Work"),
    ];
    seed_account(path, ADDRESS, MAILBOX, &labels)
}

/// Adds the second made-up account to a database [`seed`] filled.
pub fn seed_work(path: &Path) -> anyhow::Result<()> {
    seed_account(
        path,
        WORK_ADDRESS,
        WORK_MAILBOX,
        &[("Label_clients", "Clients")],
    )
}

fn seed_account(
    path: &Path,
    address: &str,
    mailbox: &[Mail],
    user_labels: &[(&str, &str)],
) -> anyhow::Result<()> {
    let mut store = Store::open(path)?;
    let account = store.add_account(address)?;
    let system = [
        "INBOX",
        "STARRED",
        "SENT",
        "DRAFT",
        "SPAM",
        "TRASH",
        "UNREAD",
        "IMPORTANT",
    ];
    let mut labels: Vec<Label> = system
        .iter()
        .map(|id| Label {
            id: (*id).into(),
            name: (*id).into(),
            kind: LabelKind::System,
        })
        .collect();
    for &(id, name) in user_labels {
        labels.push(Label {
            id: id.into(),
            name: name.into(),
            kind: LabelKind::User,
        });
    }
    store.replace_labels(account.id, &labels)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default();
    let messages: Vec<Message> = mailbox
        .iter()
        .enumerate()
        .map(|(index, mail)| Message {
            gmail_id: format!("demo-{index}"),
            thread_id: mail.thread.into(),
            internal_date: now - mail.hours_ago * HOUR - (index as i64 % 7) * 60_000,
            subject: mail.subject.into(),
            from_name: mail.from.0.into(),
            from_addr: mail.from.1.into(),
            to: if mail.from.1 == address {
                "Sam Okafor <sam@buildkite.test>".into()
            } else {
                format!("You <{address}>")
            },
            snippet: mail
                .body
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(120)
                .collect(),
            body_text: mail.body.into(),
            body_html: mail.html.map(str::to_owned),
            label_ids: mail.labels.iter().map(|label| (*label).into()).collect(),
            attachments: mail
                .attachment
                .map(|(filename, mime_type, size)| Attachment {
                    part_id: "1".into(),
                    filename: filename.into(),
                    mime_type: mime_type.into(),
                    size,
                    gmail_attachment_id: None,
                    shown_in_html: false,
                })
                .into_iter()
                .collect(),
            ..Message::default()
        })
        .collect();
    store.save_messages(account.id, &messages)?;
    // Mark the mailbox as synced so the status bar reads naturally.
    store.save_sync_point(account.id, "demo", now - 2 * 60_000)?;
    Ok(())
}
