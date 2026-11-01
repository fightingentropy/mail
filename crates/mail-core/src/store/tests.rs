use super::{search::fts_expression, *};

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
fn one_account_per_database() {
    let store = Store::in_memory();
    store.add_account("me@example.com").unwrap();
    assert!(store.add_account("ME@example.com").is_ok());
    assert!(store.add_account("someone@example.com").is_err());
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
