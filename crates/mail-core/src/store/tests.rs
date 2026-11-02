use super::{search::fts_expression, *};
use crate::model::{Change, InlineImage};

fn message(
    id: &str,
    thread: &str,
    date: i64,
    from: &str,
    subject: &str,
    body: &str,
    labels: &[&str],
) -> Message {
    let (from_name, from_addr) = crate::gmail::split_address(from);
    Message {
        gmail_id: id.into(),
        thread_id: thread.into(),
        internal_date: date,
        subject: subject.into(),
        from_name,
        from_addr,
        to: "Me <me@example.com>".into(),
        snippet: body.chars().take(40).collect(),
        body_text: body.into(),
        label_ids: labels.iter().map(|label| label.to_string()).collect(),
        ..Message::default()
    }
}

fn store_with_mail() -> (Store, i64) {
    let mut store = Store::in_memory();
    let account = store.add_account("me@example.com").unwrap();
    store
        .save_messages(
            account.id,
            &[
                message(
                    "m1",
                    "t1",
                    1_000,
                    "Ana Lima <ana@example.org>",
                    "Invoice for October",
                    "Please find the invoice attached.",
                    &["INBOX", "UNREAD"],
                ),
                message(
                    "m2",
                    "t1",
                    2_000,
                    "me@example.com",
                    "Re: Invoice for October",
                    "Thanks, paid today.",
                    &["SENT"],
                ),
                message(
                    "m3",
                    "t2",
                    3_000,
                    "Rail Co <tickets@rail.example>",
                    "Your e-ticket",
                    "Booking reference XK42. The invoice is in your account.",
                    &["INBOX", "Label_1"],
                ),
                message(
                    "m4",
                    "t3",
                    4_000,
                    "Promo <deals@shop.example>",
                    "Invoice-free shopping",
                    "Spam about invoices.",
                    &["SPAM"],
                ),
            ],
        )
        .unwrap();
    (store, account.id)
}

fn ids(hits: &[SearchHit]) -> Vec<&str> {
    hits.iter().map(|hit| hit.gmail_id.as_str()).collect()
}

fn query(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        ..SearchQuery::default()
    }
}

#[test]
fn migrations_run_once_and_reopen_cleanly() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mail.sqlite3");
    Store::open(&path)
        .unwrap()
        .add_account("me@example.com")
        .unwrap();
    let store = Store::open(&path).unwrap();
    assert_eq!(store.account().unwrap().unwrap().email, "me@example.com");
    assert_eq!(schema::version(store.connection()).unwrap(), schema::LATEST);
}

#[test]
fn own_name_comes_from_the_newest_mail_you_sent() {
    let mut store = Store::in_memory();
    let account = store.add_account("me@example.com").unwrap();
    assert_eq!(store.own_name(&account).unwrap(), None);
    let sent = |id: &str, date: i64, name: &str, addr: &str| Message {
        gmail_id: id.into(),
        thread_id: id.into(),
        internal_date: date,
        from_name: name.into(),
        from_addr: addr.into(),
        ..Message::default()
    };
    store
        .save_messages(
            account.id,
            &[
                sent("a", 1, "Erlin H", "me@example.com"),
                sent("b", 2, "Erlin", "ME@example.com"),
                sent("c", 3, "", "me@example.com"),
                sent("d", 4, "Someone Else", "else@example.com"),
            ],
        )
        .unwrap();
    assert_eq!(store.own_name(&account).unwrap().as_deref(), Some("Erlin"));
}

#[test]
fn accounts_are_added_once_each_in_order() {
    let store = Store::in_memory();
    let first = store.add_account("me@example.com").unwrap();
    assert_eq!(store.add_account("ME@example.com").unwrap().id, first.id);
    store.add_account("work@example.org").unwrap();
    let emails: Vec<String> = store
        .accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.email)
        .collect();
    assert_eq!(emails, ["me@example.com", "work@example.org"]);
    // The command line keeps reading the first.
    assert_eq!(store.account().unwrap().unwrap().id, first.id);
}

#[test]
fn reads_a_thread_with_labels_and_attachments() {
    let (mut store, account) = store_with_mail();
    let mut with_file = message(
        "m5",
        "t1",
        5_000,
        "Ana Lima <ana@example.org>",
        "Re: Invoice",
        "Receipt attached.",
        &["INBOX"],
    );
    with_file.attachments.push(Attachment {
        part_id: "1".into(),
        filename: "receipt.pdf".into(),
        mime_type: "application/pdf".into(),
        size: 2048,
        gmail_attachment_id: Some("att-1".into()),
    });
    store.save_messages(account, &[with_file.clone()]).unwrap();

    let thread = store.thread(account, "t1").unwrap();
    assert_eq!(
        thread
            .iter()
            .map(|m| m.gmail_id.as_str())
            .collect::<Vec<_>>(),
        ["m1", "m2", "m5"]
    );
    assert_eq!(thread[0].label_ids, ["INBOX", "UNREAD"]);
    assert_eq!(thread[2], with_file);
}

#[test]
fn ranks_subject_and_sender_matches_first() {
    let (store, account) = store_with_mail();
    // m3 mentions "invoice" only in its body; the thread t1 has it in the subject.
    let hits = store.search(account, &query("invoice")).unwrap();
    assert_eq!(ids(&hits)[2], "m3");
    assert!(hits[2].excerpt.contains("«invoice»"));
}

#[test]
fn words_match_as_prefixes_and_all_must_match() {
    let (store, account) = store_with_mail();
    assert_eq!(
        ids(&store.search(account, &query("inv paid")).unwrap()),
        ["m2"]
    );
    assert_eq!(ids(&store.search(account, &query("xk42")).unwrap()), ["m3"]);
    assert_eq!(
        ids(&store.search(account, &query("tickets@rail")).unwrap()),
        ["m3"]
    );
}

#[test]
fn filters_by_label_sender_and_date() {
    let (store, account) = store_with_mail();
    let inbox = SearchQuery {
        label_id: Some("INBOX".into()),
        ..query("invoice")
    };
    assert_eq!(ids(&store.search(account, &inbox).unwrap()), ["m1", "m3"]);

    let from_ana = SearchQuery {
        from: Some("ANA@".into()),
        ..query("")
    };
    assert_eq!(ids(&store.search(account, &from_ana).unwrap()), ["m1"]);

    let window = SearchQuery {
        since: Some(2_000),
        before: Some(3_000),
        ..query("")
    };
    assert_eq!(ids(&store.search(account, &window).unwrap()), ["m2"]);
}

#[test]
fn empty_query_lists_newest_first_without_spam() {
    let (store, account) = store_with_mail();
    assert_eq!(
        ids(&store.search(account, &query("")).unwrap()),
        ["m3", "m2", "m1"]
    );
    let spam = SearchQuery {
        label_id: Some("SPAM".into()),
        ..query("invoice")
    };
    assert_eq!(ids(&store.search(account, &spam).unwrap()), ["m4"]);
}

#[test]
fn newest_order_ignores_rank() {
    let (store, account) = store_with_mail();
    let newest = SearchQuery {
        order: Order::Newest,
        ..query("invoice")
    };
    assert_eq!(
        ids(&store.search(account, &newest).unwrap()),
        ["m3", "m2", "m1"]
    );
}

#[test]
fn index_follows_updates_and_deletes() {
    let (mut store, account) = store_with_mail();
    let edited = message(
        "m3",
        "t2",
        3_000,
        "Rail Co <tickets@rail.example>",
        "Your e-ticket",
        "Seat 14C.",
        &["INBOX"],
    );
    store.save_messages(account, &[edited]).unwrap();
    assert!(store.search(account, &query("xk42")).unwrap().is_empty());
    assert_eq!(ids(&store.search(account, &query("seat")).unwrap()), ["m3"]);

    assert_eq!(
        store.delete_messages(account, ["m3", "missing"]).unwrap(),
        1
    );
    assert!(store.search(account, &query("seat")).unwrap().is_empty());
    assert!(store.message(account, "m3").unwrap().is_none());
}

#[test]
fn relabels_only_known_messages() {
    let (mut store, account) = store_with_mail();
    let archived = vec!["UNREAD".to_string()];
    let changed = store
        .set_labels(
            account,
            [
                ("m1", archived.as_slice()),
                ("missing", archived.as_slice()),
            ],
        )
        .unwrap();
    assert_eq!(changed, 1);
    assert_eq!(
        store.message(account, "m1").unwrap().unwrap().label_ids,
        ["UNREAD"]
    );
}

#[test]
fn resolves_labels_by_id_or_name() {
    let (mut store, account) = store_with_mail();
    store
        .replace_labels(
            account,
            &[
                Label {
                    id: "INBOX".into(),
                    name: "INBOX".into(),
                    kind: LabelKind::System,
                },
                Label {
                    id: "Label_1".into(),
                    name: "Travel".into(),
                    kind: LabelKind::User,
                },
            ],
        )
        .unwrap();
    assert_eq!(
        store.resolve_label(account, "travel").unwrap().as_deref(),
        Some("Label_1")
    );
    assert_eq!(
        store.resolve_label(account, "INBOX").unwrap().as_deref(),
        Some("INBOX")
    );
    assert_eq!(store.resolve_label(account, "nope").unwrap(), None);
    assert_eq!(store.labels(account).unwrap()[1].name, "Travel");
}

#[test]
fn read_only_store_cannot_write() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mail.sqlite3");
    let account = Store::open(&path)
        .unwrap()
        .add_account("me@example.com")
        .unwrap();
    let mut store = Store::open_read_only(&path).unwrap();
    assert!(store.replace_labels(account.id, &[]).is_err());
    assert!(Store::open_read_only(&folder.path().join("missing.sqlite3")).is_err());
}

#[test]
fn stats_count_messages_and_threads() {
    let (store, account) = store_with_mail();
    let stats = store.stats(account).unwrap();
    assert_eq!(
        (stats.messages, stats.threads, stats.oldest, stats.newest),
        (4, 3, Some(1_000), Some(4_000))
    );
}

#[test]
fn typed_text_becomes_a_safe_fts_query() {
    assert_eq!(
        fts_expression("invoice  2024"),
        Some("\"invoice\"* \"2024\"*".into())
    );
    assert_eq!(
        fts_expression("say \"hi\" OR -- NEAR("),
        Some("\"say\"* \"\"\"hi\"\"\"* \"OR\"* \"NEAR(\"*".into())
    );
    assert_eq!(fts_expression(" -- "), None);
    assert_eq!(fts_expression("a bo"), Some("\"a\" \"bo\"*".into()));
}

#[test]
fn search_rejects_punctuation_only() {
    let (store, account) = store_with_mail();
    assert!(store.search(account, &query("--")).is_err());
}

#[test]
fn like_wildcards_in_sender_filter_are_literal() {
    let (store, account) = store_with_mail();
    let percent = SearchQuery {
        from: Some("%".into()),
        ..query("")
    };
    assert!(store.search(account, &percent).unwrap().is_empty());
}

fn me(store: &Store) -> Account {
    store.account().unwrap().unwrap()
}

#[test]
fn summarizes_threads_newest_first() {
    let (store, _) = store_with_mail();
    let threads = store
        .threads(&me(&store), &ThreadFilter::Label("INBOX".into()))
        .unwrap();
    assert_eq!(
        threads
            .iter()
            .map(|t| t.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["t2", "t1"]
    );

    let invoice = &threads[1];
    assert_eq!(invoice.subject, "Invoice for October");
    assert_eq!(invoice.senders, ["Ana Lima", "me"]);
    assert_eq!(invoice.snippet, "Thanks, paid today.");
    assert_eq!(invoice.message_count, 2);
    assert_eq!(invoice.last_date, 2_000);
    assert!(invoice.unread);
    assert!(!invoice.starred);
    assert_eq!(invoice.label_ids, ["INBOX", "SENT", "UNREAD"]);
}

#[test]
fn all_mail_and_label_views_hide_trash_and_spam() {
    let (mut store, account) = store_with_mail();
    let all = |store: &Store| {
        store
            .threads(&me(store), &ThreadFilter::AllMail)
            .unwrap()
            .into_iter()
            .map(|t| t.thread_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(all(&store), ["t2", "t1"]);

    store.apply_change(account, "t2", &Change::Trash).unwrap();
    assert_eq!(all(&store), ["t1"]);
    assert!(
        store
            .threads(&me(&store), &ThreadFilter::Label("Label_1".into()))
            .unwrap()
            .is_empty()
    );
    let trash = store
        .threads(&me(&store), &ThreadFilter::Label("TRASH".into()))
        .unwrap();
    assert_eq!(trash[0].thread_id, "t2");
}

#[test]
fn search_groups_matches_by_thread() {
    let (store, _) = store_with_mail();
    let threads = store
        .search_threads(&me(&store), &query("invoice"))
        .unwrap();
    // m1 and m2 share t1; spam (t3) stays hidden.
    assert_eq!(
        threads
            .iter()
            .map(|t| t.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["t1", "t2"]
    );
    assert_eq!(threads[0].message_count, 2);
}

#[test]
fn counts_unread_threads_per_label() {
    let (store, account) = store_with_mail();
    let counts = store.unread_counts(account).unwrap();
    assert_eq!(counts.get("INBOX"), Some(&1));
    assert_eq!(counts.get("Label_1"), None);
}

#[test]
fn triage_applies_to_every_message_in_the_thread_and_queues() {
    let (mut store, account) = store_with_mail();
    store
        .apply_change(account, "t1", &Change::add("STARRED"))
        .unwrap();
    store
        .apply_change(account, "t1", &Change::archive())
        .unwrap();

    for message in store.thread(account, "t1").unwrap() {
        assert!(message.has_label("STARRED"));
        assert!(!message.has_label("INBOX"));
    }
    let pending = store.pending_changes(account).unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[1].change, Change::archive());

    store.finish_change(pending[0].id).unwrap();
    assert_eq!(store.pending_changes(account).unwrap().len(), 1);
}

#[test]
fn trash_and_untrash_round_trip() {
    let (mut store, account) = store_with_mail();
    store.apply_change(account, "t2", &Change::Trash).unwrap();
    let labels = store.message(account, "m3").unwrap().unwrap().label_ids;
    assert_eq!(labels, ["Label_1", "TRASH"]);
    store.apply_change(account, "t2", &Change::Untrash).unwrap();
    store
        .apply_change(account, "t2", &Change::add("INBOX"))
        .unwrap();
    let labels = store.message(account, "m3").unwrap().unwrap().label_ids;
    assert_eq!(labels, ["INBOX", "Label_1"]);
}

#[test]
fn queued_changes_win_over_an_older_sync() {
    let (mut store, account) = store_with_mail();
    store
        .apply_change(account, "t1", &Change::archive())
        .unwrap();
    // Sync writes Gmail's labels, which predate the archive.
    let inbox = vec!["INBOX".to_string(), "UNREAD".to_string()];
    store
        .set_labels(account, [("m1", inbox.as_slice())])
        .unwrap();
    store.reapply_pending(account).unwrap();
    assert_eq!(
        store.message(account, "m1").unwrap().unwrap().label_ids,
        ["UNREAD"]
    );
}

/// Timings on a 10,000-message mailbox, for checking performance changes:
/// `cargo test -p mail-core --release -- --ignored scale_timings --nocapture`
#[test]
#[ignore]
fn scale_timings() {
    use std::time::Instant;

    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mail.sqlite3");
    let mut store = Store::open(&path).unwrap();
    let account = store.add_account("me@example.com").unwrap();
    let words = [
        "invoice", "ticket", "booking", "meeting", "receipt", "travel", "project", "review",
        "dinner", "report",
    ];
    // Newsletters make rows wide: a few KB of HTML each, as in real mail.
    let html = "<table><tr><td style=\"padding:8px\">A weekly update with links and images</td></tr></table>".repeat(40);
    for chunk in 0..20 {
        let batch: Vec<Message> = (0..500)
            .map(|offset| {
                let n = chunk * 500 + offset;
                let labels: &[&str] = if n >= 9_400 { &["INBOX", "UNREAD"] } else { &[] };
                let mut m = message(
                    &format!("m{n}"),
                    &format!("t{}", n / 2),
                    n as i64 * 60_000,
                    &format!("Sender {} <s{}@example.com>", n % 300, n % 300),
                    &format!("About the {} number {n}", words[n % 10]),
                    &format!("Hello, here is the {} you asked about, number {n}. Thanks, and see you soon.", words[(n + 3) % 10]),
                    labels,
                );
                m.body_html = Some(html.clone());
                m
            })
            .collect();
        store.save_messages(account.id, &batch).unwrap();
    }
    let me = me(&store);
    let time = |name: &str, run: &mut dyn FnMut() -> usize| {
        let started = Instant::now();
        let mut count = 0;
        for _ in 0..5 {
            count = run();
        }
        println!(
            "{name:<34} {:>7.2} ms  ({count} results)",
            started.elapsed().as_secs_f64() * 1000.0 / 5.0
        );
    };
    time("search \"a\" (CLI, 50)", &mut || {
        store.search(account.id, &query("a")).unwrap().len()
    });
    time("search \"tick\" (CLI, 50)", &mut || {
        store.search(account.id, &query("tick")).unwrap().len()
    });
    let window_search = |text: &str| SearchQuery {
        text: text.into(),
        order: Order::Newest,
        limit: 300,
        ..SearchQuery::default()
    };
    time("search_threads \"tick\" (window, 300)", &mut || {
        store
            .search_threads(&me, &window_search("tick"))
            .unwrap()
            .len()
    });
    time("threads INBOX", &mut || {
        store
            .threads(&me, &ThreadFilter::Label("INBOX".into()))
            .unwrap()
            .len()
    });
    time("threads All mail", &mut || {
        store.threads(&me, &ThreadFilter::AllMail).unwrap().len()
    });
    time("unread_counts", &mut || {
        store.unread_counts(account.id).unwrap().len()
    });
    time("open a thread", &mut || {
        store.thread(account.id, "t4800").unwrap().len()
    });
    drop(store);
    let size = std::fs::metadata(&path).unwrap().len() as f64 / 1_048_576.0;
    println!("{:<34} {size:>7.1} MB", "database");
}

#[test]
fn html_is_stored_compressed_and_loaded_on_demand() {
    let (mut store, account) = store_with_mail();
    let html = "<p>Your <b>receipt</b> is attached.</p>".repeat(50);
    let mut with_html = store.message(account, "m1").unwrap().unwrap();
    with_html.body_html = Some(html.clone());
    store.save_messages(account, &[with_html.clone()]).unwrap();

    let read = store.message(account, "m1").unwrap().unwrap();
    assert!(read.has_html);
    assert_eq!(
        read.body_html, None,
        "lists and the reader never carry HTML"
    );
    assert_eq!(
        store.message_html(account, "m1").unwrap().as_deref(),
        Some(html.as_str())
    );
    let stored: i64 = store
        .connection()
        .query_row("SELECT length(html) FROM message_bodies", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        stored < html.len() as i64 / 10,
        "{stored} bytes stored for {}",
        html.len()
    );

    with_html.body_html = None;
    store.save_messages(account, &[with_html]).unwrap();
    assert!(!store.message(account, "m1").unwrap().unwrap().has_html);
    assert_eq!(store.message_html(account, "m1").unwrap(), None);
}

#[test]
fn migration_moves_existing_html_out_and_compresses_it() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mail.sqlite3");
    let html = "<div>Weekly digest with plenty of markup</div>".repeat(200);
    {
        // A database as version 2 of Mail left it, HTML inline in `messages`.
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(schema::MIGRATIONS[0]).unwrap();
        connection.execute_batch(schema::MIGRATIONS[1]).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        connection
            .execute("INSERT INTO accounts (email) VALUES ('me@example.com')", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO messages (account_id, gmail_id, thread_id, internal_date, subject, body_text, body_html)
                 VALUES (1, 'm1', 't1', 1000, 'Digest', 'Weekly digest', ?1),
                        (1, 'm2', 't2', 2000, 'Plain', 'No HTML here', NULL)",
                [&html],
            )
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    assert_eq!(
        store.message_html(1, "m1").unwrap().as_deref(),
        Some(html.as_str())
    );
    assert_eq!(store.message_html(1, "m2").unwrap(), None);
    assert!(store.message(1, "m1").unwrap().unwrap().has_html);
    let kinds: Vec<String> = store
        .connection()
        .prepare("SELECT typeof(html) FROM message_bodies")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(kinds, ["blob"]);
    let has_column: bool = store
        .connection()
        .query_row(
            "SELECT count(*) FROM pragma_table_info('messages') WHERE name = 'body_html'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!has_column);
    // Search still works on the rebuilt table.
    assert_eq!(ids(&store.search(1, &query("digest")).unwrap()), ["m1"]);
}

#[test]
fn link_words_are_stored_with_the_message() {
    let mut store = Store::in_memory();
    let account = store.add_account("me@example.com").unwrap();
    let message = Message {
        gmail_id: "m1".into(),
        thread_id: "t1".into(),
        body_text: "Track your order".into(),
        body_links: vec![BodyLink {
            start: 0,
            end: 16,
            url: "https://shop.example/track".into(),
        }],
        ..Message::default()
    };
    store
        .save_messages(account.id, std::slice::from_ref(&message))
        .unwrap();
    let stored = store.message(account.id, "m1").unwrap().unwrap();
    assert_eq!(stored.body_links, message.body_links);
}

#[test]
fn stored_bodies_are_made_again_once() {
    let mut store = Store::in_memory();
    let account = store.add_account("me@example.com").unwrap();
    let html = r#"<p>Dispatched</p><p><a href="https://shop.example/t">Track it</a></p>"#;
    let newsletter = Message {
        gmail_id: "news".into(),
        thread_id: "news".into(),
        // As an older Mail stored a stripped-HTML text part.
        body_text: "Dispatched&nbsp;\n\n\n\n      Track it".into(),
        body_html: Some(html.into()),
        ..Message::default()
    };
    let letter = Message {
        gmail_id: "letter".into(),
        thread_id: "letter".into(),
        body_text: "Hi,\n\n\n\nSee you.".into(),
        ..Message::default()
    };
    store
        .save_messages(account.id, &[newsletter, letter])
        .unwrap();
    store
        .connection()
        .execute_batch(
            "CREATE TABLE body_refresh (message_id INTEGER PRIMARY KEY);
             INSERT INTO body_refresh (message_id) SELECT id FROM messages;",
        )
        .unwrap();
    bodies::refresh_body_text(&mut store.connection).unwrap();

    let newsletter = store.message(account.id, "news").unwrap().unwrap();
    assert_eq!(newsletter.body_text, "Dispatched\n\nTrack it");
    assert_eq!(newsletter.body_links.len(), 1);
    let letter = store.message(account.id, "letter").unwrap().unwrap();
    assert_eq!(letter.body_text, "Hi,\n\nSee you.");
    // The queue is gone, so this happens once.
    let queued: bool = store
        .connection()
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'body_refresh')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!queued);
    // Search sees the new text.
    let hits = store
        .search(
            account.id,
            &SearchQuery {
                text: "track".into(),
                ..SearchQuery::default()
            },
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn inline_pictures_are_kept_with_their_message() {
    let (mut store, account) = store_with_mail();
    let logo = InlineImage {
        content_id: "logo@shop".into(),
        mime_type: "image/png".into(),
        data: b"\x89PNG made-up".to_vec(),
    };
    store
        .save_inline_images(account, "m1", std::slice::from_ref(&logo))
        .unwrap();
    // Saving again replaces rather than duplicates.
    store
        .save_inline_images(account, "m1", std::slice::from_ref(&logo))
        .unwrap();
    store
        .save_inline_images(account, "no-such-message", std::slice::from_ref(&logo))
        .unwrap();
    assert_eq!(store.inline_images(account, "m1").unwrap(), [logo]);
    assert!(store.inline_images(account, "m2").unwrap().is_empty());

    store.delete_messages(account, ["m1"]).unwrap();
    let left: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM inline_images", [], |row| row.get(0))
        .unwrap();
    assert_eq!(left, 0, "they go with the message");
}
