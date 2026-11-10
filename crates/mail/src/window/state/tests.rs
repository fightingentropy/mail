//! Drives the window model against the demo mailbox, as a person would.

use std::time::{Duration, Instant};

use super::*;
use crate::window::demo;

fn model() -> (Model, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mail.sqlite3");
    demo::seed(&path).unwrap();
    let store = Store::open(&path).unwrap();
    (Model::new(store).unwrap(), folder)
}

/// The demo mailbox plus a second, smaller account.
fn two_accounts() -> (Model, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mail.sqlite3");
    demo::seed(&path).unwrap();
    demo::seed_work(&path).unwrap();
    let store = Store::open(&path).unwrap();
    (Model::new(store).unwrap(), folder)
}

fn subjects(model: &Model) -> Vec<&str> {
    model.threads.iter().map(|t| t.subject.as_str()).collect()
}

fn selected_subject(model: &Model) -> &str {
    &model.selected_thread().unwrap().subject
}

#[test]
fn opens_on_the_inbox_with_the_newest_thread_selected() {
    let (model, _folder) = model();
    assert_eq!(model.view, View::Mailbox("INBOX".into()));
    assert_eq!(model.threads.len(), 8);
    assert_eq!(
        selected_subject(&model),
        "Next steps: senior full-stack engineer"
    );
    assert_eq!(model.open.as_ref().unwrap().messages.len(), 1);
    assert_eq!(model.unread("INBOX"), 3);
}

#[test]
fn j_and_k_stop_at_the_ends() {
    let (mut model, _folder) = model();
    model.apply(Action::Previous).unwrap();
    assert_eq!(model.selected, 0);
    for _ in 0..20 {
        model.apply(Action::Next).unwrap();
    }
    assert_eq!(model.selected, 7);
    assert!(model.reveal_selected);
}

#[test]
fn archive_moves_to_the_next_thread_and_undo_brings_it_back() {
    let (mut model, _folder) = model();
    model.apply(Action::Archive).unwrap();
    assert_eq!(model.threads.len(), 7);
    assert_eq!(
        selected_subject(&model),
        "Code review: offline queue for triage"
    );
    assert!(model.wants_sync);
    assert_eq!(model.pending_changes(), 1);

    model.apply(Action::Undo).unwrap();
    assert_eq!(model.threads.len(), 8);
    assert_eq!(
        selected_subject(&model),
        "Next steps: senior full-stack engineer"
    );
    assert_eq!(model.pending_changes(), 2);
    assert_eq!(model.notice(), Some("Undone."));
    assert!(!model.notice_offers_undo());
}

#[test]
fn trash_then_undo_restores_the_inbox() {
    let (mut model, _folder) = model();
    model.apply(Action::Next).unwrap();
    model.apply(Action::Trash).unwrap();
    assert!(!subjects(&model).contains(&"Code review: offline queue for triage"));

    model.apply(Action::Show("TRASH".into())).unwrap();
    assert_eq!(subjects(&model), ["Code review: offline queue for triage"]);

    model.apply(Action::Show("INBOX".into())).unwrap();
    model.apply(Action::Undo).unwrap();
    assert_eq!(
        selected_subject(&model),
        "Code review: offline queue for triage"
    );
}

#[test]
fn archive_outside_the_inbox_says_so() {
    let (mut model, _folder) = model();
    model.apply(Action::Show("Label_receipts".into())).unwrap();
    let receipt = model
        .threads
        .iter()
        .position(|t| t.subject.starts_with("Receipt"))
        .unwrap();
    model.apply(Action::Select(receipt)).unwrap();
    model.apply(Action::Archive).unwrap();
    assert_eq!(model.notice(), Some("Not in the inbox."));
    assert_eq!(model.pending_changes(), 0);
}

#[test]
fn star_and_unread_toggle() {
    let (mut model, _folder) = model();
    model.apply(Action::ToggleStar).unwrap();
    assert!(model.selected_thread().unwrap().starred);
    model.apply(Action::ToggleUnread).unwrap();
    assert!(!model.selected_thread().unwrap().unread);
    model.apply(Action::ToggleUnread).unwrap();
    assert!(model.selected_thread().unwrap().unread);

    model.apply(Action::Show("STARRED".into())).unwrap();
    assert!(subjects(&model).contains(&"Next steps: senior full-stack engineer"));
}

#[test]
fn search_lists_matching_threads_and_clearing_returns() {
    let (mut model, _folder) = model();
    model.apply(Action::Search("seat".into())).unwrap();
    assert_eq!(subjects(&model), ["Your e-ticket: London to Edinburgh"]);
    model.apply(Action::ClearSearch).unwrap();
    assert_eq!(model.view, View::Mailbox("INBOX".into()));
    assert_eq!(model.threads.len(), 8);
}

#[test]
fn label_picker_filters_and_toggles() {
    let (mut model, _folder) = model();
    model.apply(Action::OpenPicker).unwrap();
    model.apply(Action::PickerFilter("tra".into())).unwrap();
    let names: Vec<&str> = model
        .picker_labels()
        .iter()
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(names, ["Travel"]);

    model.apply(Action::PickerToggle(None)).unwrap();
    assert!(model.picker.is_none());
    assert!(
        model
            .selected_thread()
            .unwrap()
            .label_ids
            .contains(&"Label_travel".to_string())
    );
    assert_eq!(model.notice(), Some("Labeled Travel"));
    assert!(model.notice_offers_undo());

    model.apply(Action::OpenPicker).unwrap();
    model
        .apply(Action::PickerToggle(Some("Label_travel".into())))
        .unwrap();
    assert!(
        !model
            .selected_thread()
            .unwrap()
            .label_ids
            .contains(&"Label_travel".to_string())
    );
}

#[test]
fn a_thread_counts_as_read_after_being_shown() {
    let (mut model, _folder) = model();
    let opened = Instant::now();
    assert!(
        model.tick(opened).unwrap().is_some(),
        "waits before marking read"
    );
    assert!(model.selected_thread().unwrap().unread);

    model
        .tick(opened + READ_AFTER + Duration::from_millis(50))
        .unwrap();
    assert!(!model.selected_thread().unwrap().unread);
    assert!(model.wants_sync);
}

#[test]
fn notices_clear_themselves_but_work_in_progress_stays() {
    let (mut model, _folder) = model();
    model.apply(Action::Archive).unwrap();
    let set = Instant::now();
    assert_eq!(model.notice(), Some("Archived"));
    assert!(model.notice_offers_undo());
    let wait = model.tick(set).unwrap().expect("waits to clear the notice");
    assert!(wait <= NOTICE_FOR);

    model
        .tick(set + NOTICE_FOR + Duration::from_millis(50))
        .unwrap();
    assert_eq!(model.notice(), None);
    // Undo still works once the notice has gone.
    model.apply(Action::Undo).unwrap();
    assert_eq!(model.threads.len(), 8);

    model.notify("Sending…".into());
    model.tick(Instant::now() + NOTICE_FOR * 3).unwrap();
    assert_eq!(model.notice(), Some("Sending…"));
}

#[test]
fn long_threads_open_with_only_the_latest_and_unread_expanded() {
    let (mut model, _folder) = model();
    model.apply(Action::Next).unwrap();
    let open = model.open.as_ref().unwrap();
    assert_eq!(open.messages.len(), 3);
    // Oldest read message collapsed, my reply collapsed, the unread latest open.
    assert_eq!(open.expanded, [false, false, true]);

    model.apply(Action::ToggleExpanded(0)).unwrap();
    assert_eq!(model.open.as_ref().unwrap().expanded, [true, false, true]);
}

#[test]
fn reply_opens_a_threaded_draft_to_the_sender() {
    let (mut model, _folder) = model();
    model.apply(Action::Reply).unwrap();
    let compose = model.compose.as_ref().unwrap();
    assert_eq!(compose.title, "Reply");
    assert_eq!(compose.to, "Priya Raman <priya@lumen.example>");
    assert_eq!(
        compose.subject,
        "Re: Next steps: senior full-stack engineer"
    );
    assert!(compose.body.contains("Priya Raman wrote:\n> Hi,"));
    assert!(
        !compose.is_dirty(),
        "an untouched reply closes without asking"
    );
    model.apply(Action::CloseCompose).unwrap();
    assert!(model.compose.is_none());
}

#[test]
fn reply_all_in_a_thread_answers_the_latest_message_without_me() {
    let (mut model, _folder) = model();
    model.apply(Action::Next).unwrap();
    model.apply(Action::ReplyAll).unwrap();
    let compose = model.compose.as_ref().unwrap();
    assert_eq!(compose.to, "Sam Okafor <sam@buildkite.test>");
    assert_eq!(compose.cc, "");
}

#[test]
fn unsaved_edits_keep_the_panel_open_until_saved_or_discarded() {
    let (mut model, _folder) = model();
    model.apply(Action::NewMessage).unwrap();
    {
        let compose = model.compose.as_mut().unwrap();
        compose.to = "maya@fastmail.test".into();
        compose.subject = "Friday".into();
        compose.body = "7:30 works.".into();
    }
    model.apply(Action::CloseCompose).unwrap();
    assert!(model.compose.is_some());
    assert!(model.notice().unwrap().starts_with("Not saved yet"));

    model.apply(Action::SaveDraft).unwrap();
    let [
        Effect::SaveDraft {
            from,
            draft,
            draft_id,
        },
    ] = model.effects.as_slice()
    else {
        panic!("expected one save, got {:?}", model.effects);
    };
    assert_eq!(from, demo::ADDRESS);
    assert_eq!(draft.to, "maya@fastmail.test");
    assert_eq!(*draft_id, None);
    model.apply(Action::SaveDraft).unwrap();
    assert_eq!(model.effects.len(), 1, "one save at a time");
    model.effects.clear();

    model.apply(Action::DraftSaved("draft-9".into())).unwrap();
    assert!(model.wants_sync);
    assert!(!model.compose.as_ref().unwrap().is_dirty());

    // A second save updates the same draft.
    model
        .compose
        .as_mut()
        .unwrap()
        .body
        .push_str(" See you there.");
    model.apply(Action::SaveDraft).unwrap();
    let [Effect::SaveDraft { draft_id, .. }] = model.effects.as_slice() else {
        panic!("expected one save, got {:?}", model.effects);
    };
    assert_eq!(draft_id.as_deref(), Some("draft-9"));
    model.effects.clear();
    model.apply(Action::DraftFailed("offline".into())).unwrap();
    assert!(!model.compose.as_ref().unwrap().is_saving());

    model.apply(Action::DiscardDraft).unwrap();
    assert!(model.compose.is_none());
    assert_eq!(
        model.effects,
        [Effect::DiscardDraft {
            from: demo::ADDRESS.into(),
            draft_id: "draft-9".into()
        }]
    );
}

fn select(model: &mut Model, subject: &str) {
    let index = model
        .threads
        .iter()
        .position(|t| t.subject == subject)
        .unwrap();
    model.apply(Action::Select(index)).unwrap();
}

#[test]
fn clicking_an_attachment_asks_the_shell_to_download_it() {
    let (mut model, _folder) = model();
    select(&mut model, "Your e-ticket: London to Edinburgh");
    model
        .apply(Action::Download {
            message: 0,
            attachment: 0,
        })
        .unwrap();
    let [
        Effect::Download {
            account,
            message_id,
            attachment,
        },
    ] = model.effects.as_slice()
    else {
        panic!("expected one download, got {:?}", model.effects);
    };
    assert_eq!(account, demo::ADDRESS);
    assert_eq!(
        message_id,
        &model.open.as_ref().unwrap().messages[0].gmail_id
    );
    assert_eq!(attachment.filename, "eticket-QX741K.pdf");

    // A stale click, after the thread changed, asks for nothing.
    model
        .apply(Action::Download {
            message: 0,
            attachment: 5,
        })
        .unwrap();
    assert_eq!(model.effects.len(), 1);
}

#[test]
fn links_open_only_through_the_shell() {
    let (mut model, _folder) = model();
    let url = "https://northline.example/booking/QX7-41K".to_owned();
    model.apply(Action::OpenLink(url.clone())).unwrap();
    assert_eq!(model.effects, [Effect::OpenLink(url)]);
}

#[test]
fn h_opens_the_html_of_the_message_on_screen() {
    let (mut model, _folder) = model();
    select(
        &mut model,
        "This week: async traits, SQLite tips and egui 0.36",
    );
    model.apply(Action::OpenHtml).unwrap();
    let [Effect::OpenHtml { html, .. }] = model.effects.as_slice() else {
        panic!("expected HTML, got {:?}", model.effects);
    };
    assert!(html.contains("<h1"));
}

#[test]
fn plain_text_mail_says_it_has_no_html() {
    let (mut model, _folder) = model();
    model.apply(Action::OpenHtml).unwrap();
    assert!(model.effects.is_empty());
    assert_eq!(model.notice(), Some("This message has no HTML version."));
}

#[test]
fn live_search_waits_for_a_second_letter() {
    let (mut model, _folder) = model();
    model.apply(Action::Search("s".into())).unwrap();
    assert_eq!(model.view, View::Mailbox("INBOX".into()));
    model.apply(Action::Search("se".into())).unwrap();
    assert_eq!(model.view, View::Search("se".into()));
}

fn compose_to(model: &mut Model, to: &str) {
    model.apply(Action::NewMessage).unwrap();
    let compose = model.compose.as_mut().unwrap();
    compose.to = to.into();
    compose.subject = "Friday".into();
    compose.body = "7:30 works.".into();
}

#[test]
fn sending_asks_for_confirmation_first() {
    let (mut model, _folder) = model();
    compose_to(&mut model, "Maya <maya@fastmail.test>");
    model.apply(Action::RequestSend).unwrap();
    assert!(model.effects.is_empty(), "nothing goes before you confirm");
    let summary = model.confirm_send.clone().unwrap();
    assert_eq!(summary.to, "Maya <maya@fastmail.test>");
    assert_eq!(summary.subject, "Friday");
    assert_eq!(summary.from, None, "one account goes without saying");

    model.apply(Action::CancelSend).unwrap();
    assert!(model.confirm_send.is_none());
    assert!(model.effects.is_empty());
    assert!(model.compose.is_some(), "cancelling keeps the message");
}

#[test]
fn confirming_sends_once_and_closes_on_success() {
    let (mut model, _folder) = model();
    compose_to(&mut model, "maya@fastmail.test");
    model.apply(Action::RequestSend).unwrap();
    model.apply(Action::ConfirmSend).unwrap();
    let [
        Effect::SendDraft {
            draft,
            draft_id: None,
            ..
        },
    ] = model.effects.as_slice()
    else {
        panic!("expected one send, got {:?}", model.effects);
    };
    assert_eq!(draft.to, "maya@fastmail.test");
    // A second ⌘Enter while sending does nothing.
    model.apply(Action::RequestSend).unwrap();
    model.apply(Action::ConfirmSend).unwrap();
    assert_eq!(model.effects.len(), 1);

    // The save half reports the draft ID without changing the notice.
    model.apply(Action::DraftSaved("draft-3".into())).unwrap();
    assert_eq!(model.notice(), Some("Sending…"));
    model.apply(Action::MessageSent).unwrap();
    assert!(model.compose.is_none());
    assert_eq!(model.notice(), Some("Sent."));
    assert!(model.wants_sync);
}

#[test]
fn a_failed_send_keeps_the_message_and_its_draft() {
    let (mut model, _folder) = model();
    compose_to(&mut model, "maya@fastmail.test");
    model.apply(Action::RequestSend).unwrap();
    model.apply(Action::ConfirmSend).unwrap();
    model.apply(Action::DraftSaved("draft-3".into())).unwrap();
    model
        .apply(Action::SendFailed("Not sent: offline".into()))
        .unwrap();
    let compose = model.compose.as_ref().unwrap();
    assert!(!compose.is_sending());
    assert_eq!(compose.draft_id.as_deref(), Some("draft-3"));

    // Retrying sends the same draft rather than making another.
    model.effects.clear();
    model.apply(Action::RequestSend).unwrap();
    model.apply(Action::ConfirmSend).unwrap();
    let [Effect::SendDraft { draft_id, .. }] = model.effects.as_slice() else {
        panic!("expected one send, got {:?}", model.effects);
    };
    assert_eq!(draft_id.as_deref(), Some("draft-3"));
}

#[test]
fn sending_needs_a_valid_recipient() {
    let (mut model, _folder) = model();
    compose_to(&mut model, "");
    model.apply(Action::RequestSend).unwrap();
    assert!(model.confirm_send.is_none());
    assert_eq!(model.notice(), Some("Add a recipient first."));

    model.compose.as_mut().unwrap().to = "maya@".into();
    model.apply(Action::RequestSend).unwrap();
    assert!(model.confirm_send.is_none());
    assert!(model.notice().unwrap().starts_with("Not an email address"));
}

#[test]
fn all_accounts_merge_newest_first_and_one_can_be_chosen() {
    let (mut model, _folder) = two_accounts();
    assert_eq!(model.accounts.len(), 2);
    assert_eq!(model.scope, None);
    // 8 threads in the first inbox, 3 in the second, by date across both.
    assert_eq!(model.threads.len(), 11);
    assert_eq!(
        selected_subject(&model),
        "Next steps: senior full-stack engineer"
    );
    assert_eq!(model.threads[1].subject, "Comments on the onboarding flow");
    assert!(
        model
            .threads
            .windows(2)
            .all(|pair| pair[0].last_date >= pair[1].last_date)
    );
    assert_eq!(model.unread("INBOX"), 5);

    let work = model.accounts[1].id;
    model.apply(Action::SetScope(Some(work))).unwrap();
    assert_eq!(model.threads.len(), 3);
    assert!(model.threads.iter().all(|thread| thread.account_id == work));
    assert_eq!(model.unread("INBOX"), 2);

    model.apply(Action::SetScope(None)).unwrap();
    assert_eq!(model.threads.len(), 11);
}

#[test]
fn actions_go_to_the_account_the_thread_is_in() {
    let (mut model, _folder) = two_accounts();
    let work = model.accounts[1].id;
    select(&mut model, "Standup moves to 10:30");
    model.apply(Action::Archive).unwrap();
    assert_eq!(model.store.pending_changes(work).unwrap().len(), 1);
    let first = model.accounts[0].id;
    assert!(model.store.pending_changes(first).unwrap().is_empty());

    // Replies come from the address the thread was sent to.
    select(&mut model, "Comments on the onboarding flow");
    model.apply(Action::Reply).unwrap();
    assert_eq!(model.compose.as_ref().unwrap().from, demo::WORK_ADDRESS);
    assert_eq!(
        model.compose.as_ref().unwrap().to,
        "Lena Brandt <lena@studio.example>"
    );
    model.apply(Action::DiscardDraft).unwrap();

    // The label picker offers the thread's own account's labels.
    model.apply(Action::OpenPicker).unwrap();
    let names: Vec<&str> = model
        .picker_labels()
        .iter()
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(names, ["Clients"]);
}

#[test]
fn a_label_belongs_to_one_account_so_all_returns_to_the_inbox() {
    let (mut model, _folder) = two_accounts();
    let work = model.accounts[1].id;
    model.apply(Action::SetScope(Some(work))).unwrap();
    model.apply(Action::Show("Label_clients".into())).unwrap();
    assert_eq!(subjects(&model), ["Comments on the onboarding flow"]);

    model.apply(Action::SetScope(None)).unwrap();
    assert_eq!(model.view, View::Mailbox("INBOX".into()));

    // Starred is shared by every account, so it stays.
    model.apply(Action::Show("STARRED".into())).unwrap();
    model.apply(Action::SetScope(Some(work))).unwrap();
    assert_eq!(model.view, View::Mailbox("STARRED".into()));
}

#[test]
fn new_mail_comes_from_the_account_shown() {
    let (mut model, _folder) = two_accounts();
    model.apply(Action::NewMessage).unwrap();
    assert_eq!(model.compose.take().unwrap().from, demo::ADDRESS);
    let work = model.accounts[1].id;
    model.apply(Action::SetScope(Some(work))).unwrap();
    model.apply(Action::NewMessage).unwrap();
    assert_eq!(model.compose.take().unwrap().from, demo::WORK_ADDRESS);
}

#[test]
fn a_new_message_can_come_from_another_account() {
    let (mut model, _folder) = two_accounts();
    compose_to(&mut model, "maya@fastmail.test");
    model
        .apply(Action::SetFrom(demo::WORK_ADDRESS.into()))
        .unwrap();
    assert_eq!(model.compose.as_ref().unwrap().from, demo::WORK_ADDRESS);
    // Only your own accounts.
    model
        .apply(Action::SetFrom("someone@else.example".into()))
        .unwrap();
    assert_eq!(model.compose.as_ref().unwrap().from, demo::WORK_ADDRESS);

    model.apply(Action::RequestSend).unwrap();
    let summary = model.confirm_send.clone().unwrap();
    assert_eq!(summary.from.as_deref(), Some(demo::WORK_ADDRESS));
    model.apply(Action::ConfirmSend).unwrap();
    let [Effect::SendDraft { from, .. }] = model.effects.as_slice() else {
        panic!("expected one send, got {:?}", model.effects);
    };
    assert_eq!(from, demo::WORK_ADDRESS);
}

#[test]
fn changing_from_moves_a_saved_draft_to_the_new_account() {
    let (mut model, _folder) = two_accounts();
    compose_to(&mut model, "maya@fastmail.test");
    model.apply(Action::SaveDraft).unwrap();
    // Not while a save is on its way.
    model
        .apply(Action::SetFrom(demo::WORK_ADDRESS.into()))
        .unwrap();
    assert_eq!(model.compose.as_ref().unwrap().from, demo::ADDRESS);
    assert_eq!(model.notice(), Some("Wait for the draft to finish saving."));
    model.apply(Action::DraftSaved("draft-1".into())).unwrap();
    model.effects.clear();

    model
        .apply(Action::SetFrom(demo::WORK_ADDRESS.into()))
        .unwrap();
    assert_eq!(
        model.effects,
        [Effect::DiscardDraft {
            from: demo::ADDRESS.into(),
            draft_id: "draft-1".into()
        }]
    );
    let compose = model.compose.as_ref().unwrap();
    assert_eq!(compose.draft_id, None);
    assert!(compose.is_dirty(), "unsaved until saved in the new account");
    model.effects.clear();

    model.apply(Action::SaveDraft).unwrap();
    let [
        Effect::SaveDraft {
            from,
            draft_id: None,
            ..
        },
    ] = model.effects.as_slice()
    else {
        panic!("expected one new save, got {:?}", model.effects);
    };
    assert_eq!(from, demo::WORK_ADDRESS);
}

#[test]
fn a_reply_stays_with_its_conversations_account() {
    let (mut model, _folder) = two_accounts();
    model.apply(Action::Reply).unwrap();
    let from = model.compose.as_ref().unwrap().from.clone();
    let other = if from == demo::ADDRESS {
        demo::WORK_ADDRESS
    } else {
        demo::ADDRESS
    };
    model.apply(Action::SetFrom(other.into())).unwrap();
    assert_eq!(model.compose.as_ref().unwrap().from, from);
    assert!(model.effects.is_empty());
}

#[test]
fn the_header_uses_the_first_name_you_sign_with() {
    let (model, _folder) = two_accounts();
    let first = model.accounts[0].id;
    assert_eq!(model.first_name(first), Some("Alex"));
    // No mail sent from the second account yet.
    assert_eq!(model.first_name(model.accounts[1].id), None);
}

/// Adds a receipt whose logo is sent inside it, and opens it.
fn open_receipt(model: &mut Model) -> i64 {
    let account = model.accounts[0].id;
    let receipt = Message {
        gmail_id: "receipt".into(),
        thread_id: "receipt".into(),
        internal_date: i64::MAX / 2,
        subject: "Your receipt".into(),
        from_name: "Shop".into(),
        from_addr: "orders@shop.example".into(),
        body_text: "Thanks for your order.".into(),
        body_html: Some(r#"<img src="cid:logo@shop"><p>Thanks for your order.</p>"#.into()),
        label_ids: vec!["INBOX".into()],
        ..Message::default()
    };
    model.store.save_messages(account, &[receipt]).unwrap();
    model.reload().unwrap();
    select(model, "Your receipt");
    account
}

fn logo() -> InlineImage {
    InlineImage {
        content_id: "logo@shop".into(),
        mime_type: "image/png".into(),
        data: b"\x89PNG made-up".to_vec(),
    }
}

#[test]
fn pictures_sent_inside_an_email_are_asked_for_once_then_shown() {
    let (mut model, _folder) = model();
    let account = open_receipt(&mut model);
    let [Effect::FetchInlineImages { wanted, .. }] = model.effects.as_slice() else {
        panic!("expected one request, got {:?}", model.effects);
    };
    assert_eq!(wanted, &["logo@shop"]);
    assert_eq!(model.showing_original().unwrap().inline, 0);
    model.effects.clear();

    select(
        &mut model,
        "This week: async traits, SQLite tips and egui 0.36",
    );
    select(&mut model, "Your receipt");
    assert!(model.effects.is_empty(), "asked for only once");

    model
        .apply(Action::InlineImagesArrived {
            account_id: account,
            message_id: "receipt".into(),
            images: Ok(vec![logo()]),
        })
        .unwrap();
    let original = model.showing_original().unwrap();
    assert_eq!(original.inline, 1);
    assert!(
        original
            .html
            .contains(r#"<img src="data:image/png;base64,"#)
    );

    // Kept: next time it opens with the logo and asks for nothing.
    select(
        &mut model,
        "This week: async traits, SQLite tips and egui 0.36",
    );
    select(&mut model, "Your receipt");
    assert_eq!(model.showing_original().unwrap().inline, 1);
    assert!(model.effects.is_empty());
}

#[test]
fn pictures_that_could_not_load_are_asked_for_again_next_time() {
    let (mut model, _folder) = model();
    let account = open_receipt(&mut model);
    model.effects.clear();
    model
        .apply(Action::InlineImagesArrived {
            account_id: account,
            message_id: "receipt".into(),
            images: Err("network unreachable".into()),
        })
        .unwrap();
    assert_eq!(
        model.notice(),
        Some("Could not load the pictures in this email: network unreachable")
    );
    select(
        &mut model,
        "This week: async traits, SQLite tips and egui 0.36",
    );
    select(&mut model, "Your receipt");
    assert_eq!(model.effects.len(), 1);
}

#[test]
fn pictures_are_only_asked_for_while_the_original_shows() {
    let (mut model, _folder) = model();
    model.show_original = false;
    open_receipt(&mut model);
    assert!(model.effects.is_empty());
    model.apply(Action::ToggleOriginal).unwrap();
    assert_eq!(model.effects.len(), 1);
}

#[test]
fn remote_images_load_as_an_email_opens_unless_turned_off() {
    let (mut model, _folder) = model();
    open_receipt(&mut model);
    assert!(model.showing_original().unwrap().images);

    // Turned off in Settings: the open email follows at once.
    model.apply(Action::SetRemoteImages(false)).unwrap();
    assert!(!model.showing_original().unwrap().images);

    // The next email waits for "Load images".
    select(
        &mut model,
        "This week: async traits, SQLite tips and egui 0.36",
    );
    select(&mut model, "Your receipt");
    assert!(!model.showing_original().unwrap().images);
    model.apply(Action::LoadImages).unwrap();
    assert!(model.showing_original().unwrap().images);

    // Turned back on: emails load them as they open again.
    model.apply(Action::SetRemoteImages(true)).unwrap();
    select(
        &mut model,
        "This week: async traits, SQLite tips and egui 0.36",
    );
    select(&mut model, "Your receipt");
    assert!(model.showing_original().unwrap().images);
}

#[test]
fn settings_can_make_emails_read_as_text() {
    let (mut model, _folder) = model();
    open_receipt(&mut model);
    assert!(model.showing_original().is_some());
    model.apply(Action::SetShowOriginal(false)).unwrap();
    assert!(model.showing_original().is_none());
    // v still shows this one as designed.
    model.apply(Action::ToggleOriginal).unwrap();
    assert!(model.showing_original().is_some());
}

#[test]
fn reply_and_new_message_keep_an_unsaved_compose() {
    let (mut model, _folder) = model();
    compose_to(&mut model, "maya@fastmail.test");
    model.apply(Action::Reply).unwrap();
    model.apply(Action::ReplyAll).unwrap();
    model.apply(Action::NewMessage).unwrap();
    let compose = model.compose.as_ref().unwrap();
    assert_eq!(compose.to, "maya@fastmail.test");
    assert_eq!(compose.body, "7:30 works.");
}
