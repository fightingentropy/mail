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

#[test]
fn searching_and_switching_mailbox_leave_the_sidebar_counts_alone() {
    let (mut model, _folder) = model();
    model.unread.clear();
    model.apply(Action::Search("se".into())).unwrap();
    model.apply(Action::Show("SENT".into())).unwrap();
    model.apply(Action::ClearSearch).unwrap();
    assert!(model.unread.is_empty(), "counts are not rescanned per view");
    model.reload().unwrap();
    assert!(!model.unread.is_empty(), "a full reload refreshes them");
}

#[test]
fn a_held_key_does_not_repeat_sending_or_removing_mail() {
    for action in [
        Action::RequestSend,
        Action::ConfirmSend,
        Action::Archive,
        Action::Trash,
        Action::DiscardDraft,
    ] {
        assert!(action.once_per_press(), "{action:?}");
    }
    // Moving through the list still follows a held arrow key.
    assert!(!Action::Next.once_per_press());
    assert!(!Action::Previous.once_per_press());
}

/// Each helper keeps its own doc comment: one helper's text must not end up
/// stacked on another, leaving the function it describes bare.
#[test]
fn set_from_keeps_its_own_doc_comment() {
    let source = include_str!("../state.rs");
    let lines: Vec<&str> = source.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.trim_start().starts_with("fn set_from("))
        .unwrap();
    let doc = lines[at - 1].trim_start();
    assert!(doc.starts_with("///"), "set_from has no doc comment");
    assert!(
        lines[at - 3].contains("Writes the new message from another account"),
        "set_from's doc comment is not its own"
    );
}

#[test]
fn discarding_waits_for_a_save_or_send_in_flight() {
    let (mut model, _folder) = model();
    compose_to(&mut model, "maya@fastmail.test");
    model.apply(Action::SaveDraft).unwrap();
    model.effects.clear();
    model.apply(Action::DiscardDraft).unwrap();
    assert!(model.compose.is_some(), "a save is still on its way");
    assert!(model.effects.is_empty());

    model.apply(Action::DraftSaved("draft-1".into())).unwrap();
    model.apply(Action::RequestSend).unwrap();
    model.apply(Action::ConfirmSend).unwrap();
    model.effects.clear();
    model.apply(Action::DiscardDraft).unwrap();
    assert!(model.compose.as_ref().unwrap().is_sending());
    assert!(model.effects.is_empty());
}

#[test]
fn a_sent_notice_does_not_close_a_newer_message() {
    let (mut model, _folder) = model();
    compose_to(&mut model, "maya@fastmail.test");
    // The message that was sent is gone; the user is writing another.
    model.compose = None;
    compose_to(&mut model, "lee@fastmail.test");
    model.apply(Action::MessageSent).unwrap();
    assert!(model.compose.is_some());
}

#[test]
fn undo_leaves_labels_that_sync_added_after_the_change() {
    let (mut model, _folder) = model();
    let read = model.threads.iter().position(|t| !t.unread).unwrap();
    model.apply(Action::Select(read)).unwrap();
    let thread = model.selected_thread().unwrap();
    let (account, thread_id) = (thread.account_id, thread.thread_id.clone());
    model.apply(Action::Archive).unwrap();

    // Sync delivers a new message to the thread, unread and in the inbox.
    let path = _folder.path().join("mail.sqlite3");
    let mut sync = Store::open(&path).unwrap();
    sync.save_messages(
        account,
        &[mail_core::model::Message {
            gmail_id: "arrived-later".into(),
            thread_id: thread_id.clone(),
            internal_date: i64::MAX / 4,
            label_ids: vec!["INBOX".into(), "UNREAD".into()],
            ..Default::default()
        }],
    )
    .unwrap();

    model.apply(Action::Undo).unwrap();
    let labels: BTreeSet<String> = model
        .store
        .thread(account, &thread_id)
        .unwrap()
        .into_iter()
        .flat_map(|message| message.label_ids)
        .collect();
    assert!(labels.contains("UNREAD"), "the new message was marked read");
    assert!(labels.contains("INBOX"));
}

const WAIT: Duration = Duration::from_secs(10);
const NEWSLETTER: &str = "This week: async traits, SQLite tips and egui 0.36";

/// The demo mailbox, building originals on a thread of their own.
fn loading_model() -> (Model, tempfile::TempDir) {
    let (mut model, folder) = model();
    model
        .start_original_loader(&folder.path().join("mail.sqlite3"), || {})
        .unwrap();
    (model, folder)
}

/// Like [`loading_model`], with each built original held until released.
/// The first receiver hears of each build as it finishes.
fn gated_model() -> (
    Model,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
    tempfile::TempDir,
) {
    let (mut model, folder) = model();
    let store = Store::open_read_only(&folder.path().join("mail.sqlite3")).unwrap();
    let (loader, built, release) = originals::Loader::gated(store);
    model.originals = Some(loader);
    (model, built, release, folder)
}

fn open_id(model: &Model) -> &str {
    &model.open.as_ref().unwrap().messages[0].gmail_id
}

#[test]
fn opening_an_original_shows_text_while_it_loads_off_thread() {
    let (mut model, _folder) = loading_model();
    open_receipt(&mut model);
    let open = model.open.as_ref().unwrap();
    assert!(model.showing_original().is_none(), "not built on the spot");
    assert!(open.loading_original());
    assert_eq!(open.messages[0].body_text, "Thanks for your order.");
    assert!(model.effects.is_empty(), "pictures wait for the page");

    assert!(model.wait_for_original(WAIT));
    assert_eq!(model.showing_original().unwrap().message_id, "receipt");
    let [Effect::FetchInlineImages { wanted, .. }] = model.effects.as_slice() else {
        panic!("expected one request, got {:?}", model.effects);
    };
    assert_eq!(wanted, &["logo@shop"]);
}

#[test]
fn a_result_for_a_thread_no_longer_selected_is_dropped() {
    let (mut model, built, release, _folder) = gated_model();
    let account = open_receipt(&mut model);
    // The receipt's page is built and waiting when the selection moves on.
    built.recv_timeout(WAIT).unwrap();
    select(&mut model, "Next steps: senior full-stack engineer");
    release.send(()).unwrap();
    assert!(model.receive_one_original(WAIT), "the late page arrived");
    assert!(model.open.as_ref().unwrap().original.is_none());
    assert!(
        !model.cache.contains(account, "receipt"),
        "and was not kept"
    );

    select(&mut model, "Your receipt");
    assert!(model.open.as_ref().unwrap().loading_original());
    assert!(model.showing_original().is_none());
}

#[test]
fn holding_j_applies_only_the_last_original() {
    let (mut model, built, release, _folder) = gated_model();
    open_receipt(&mut model);
    // The first page is built and held while j and k go on.
    built.recv_timeout(WAIT).unwrap();
    let newsletter = model
        .threads
        .iter()
        .position(|t| t.subject == NEWSLETTER)
        .unwrap();
    for step in 0..12 {
        model
            .apply(if step % 3 == 2 {
                Action::Previous
            } else {
                Action::Next
            })
            .unwrap();
        model.receive_originals();
        if let Some(original) = model.showing_original() {
            assert_eq!(original.message_id, open_id(&model));
        }
    }
    model.apply(Action::Select(newsletter)).unwrap();
    assert!(model.showing_original().is_none());
    // Let everything through: the held page, then the newest asked for.
    for _ in 0..12 {
        let _ = release.send(());
    }
    assert!(model.wait_for_original(WAIT));
    let wanted = open_id(&model).to_owned();
    assert_eq!(model.showing_original().unwrap().message_id, wanted);
    assert_eq!(selected_subject(&model), NEWSLETTER);
}

#[test]
fn reopening_a_message_uses_the_cache() {
    let (mut model, _folder) = loading_model();
    open_receipt(&mut model);
    assert!(model.wait_for_original(WAIT));
    select(&mut model, "Next steps: senior full-stack engineer");
    select(&mut model, "Your receipt");
    assert_eq!(model.showing_original().unwrap().message_id, "receipt");
    assert!(!model.open.as_ref().unwrap().loading_original());
}

#[test]
fn arriving_pictures_rebuild_off_thread_keeping_the_page() {
    let (mut model, _folder) = loading_model();
    let account = open_receipt(&mut model);
    assert!(model.wait_for_original(WAIT));
    model
        .apply(Action::InlineImagesArrived {
            account_id: account,
            message_id: "receipt".into(),
            images: Ok(vec![logo()]),
        })
        .unwrap();
    assert_eq!(
        model.showing_original().unwrap().inline,
        0,
        "old page stays"
    );
    assert!(model.open.as_ref().unwrap().loading_original());

    assert!(model.wait_for_original(WAIT));
    let original = model.showing_original().unwrap();
    assert_eq!(original.inline, 1);
    assert!(original.html.contains("data:image/png;base64,"));

    select(&mut model, "Next steps: senior full-stack engineer");
    select(&mut model, "Your receipt");
    assert_eq!(model.showing_original().unwrap().inline, 1, "kept with it");
}

#[test]
fn pictures_arriving_while_the_page_first_loads_are_not_lost() {
    let (mut model, built, release, _folder) = gated_model();
    let account = open_receipt(&mut model);
    // The page was read before the pictures were saved.
    built.recv_timeout(WAIT).unwrap();
    model
        .apply(Action::InlineImagesArrived {
            account_id: account,
            message_id: "receipt".into(),
            images: Ok(vec![logo()]),
        })
        .unwrap();
    release.send(()).unwrap();
    assert!(model.receive_one_original(WAIT));
    assert!(
        model.showing_original().is_none(),
        "the page without them is dropped"
    );

    built.recv_timeout(WAIT).unwrap();
    release.send(()).unwrap();
    assert!(model.wait_for_original(WAIT));
    assert_eq!(model.showing_original().unwrap().inline, 1);
    assert_eq!(model.cache.get(account, "receipt").unwrap().inline, 1);
}

#[test]
fn pictures_for_a_message_not_open_replace_its_kept_page() {
    for loader in [false, true] {
        let (mut model, folder) = model();
        if loader {
            model
                .start_original_loader(&folder.path().join("mail.sqlite3"), || {})
                .unwrap();
        }
        let account = open_receipt(&mut model);
        assert!(model.wait_for_original(WAIT));
        assert!(model.cache.contains(account, "receipt"));
        select(&mut model, "Next steps: senior full-stack engineer");
        model
            .apply(Action::InlineImagesArrived {
                account_id: account,
                message_id: "receipt".into(),
                images: Ok(vec![logo()]),
            })
            .unwrap();
        select(&mut model, "Your receipt");
        assert!(model.wait_for_original(WAIT));
        let original = model.showing_original().unwrap();
        assert_eq!(original.inline, 1, "with the loader: {loader}");
        assert!(original.html.contains("data:image/png;base64,"));
    }
}

#[test]
fn async_and_sync_originals_are_identical() {
    let built = |loader: bool| {
        let (mut model, folder) = model();
        if loader {
            model
                .start_original_loader(&folder.path().join("mail.sqlite3"), || {})
                .unwrap();
        }
        let account = model.accounts[0].id;
        open_receipt(&mut model);
        model
            .store
            .save_inline_images(account, "receipt", &[logo()])
            .unwrap();
        select(&mut model, "Next steps: senior full-stack engineer");
        model.cache.remove(account, "receipt");
        select(&mut model, "Your receipt");
        assert_eq!(model.open.as_ref().unwrap().loading_original(), loader);
        assert!(model.wait_for_original(WAIT));
        let original = model.showing_original().unwrap();
        (
            original.html.to_string(),
            original.inline,
            original.missing.clone(),
        )
    };
    let on_the_spot = built(false);
    assert_eq!(on_the_spot.1, 1);
    assert_eq!(built(true), on_the_spot);
}

#[test]
fn a_dead_loader_falls_back_to_loading_on_the_window_thread() {
    let (mut model, _folder) = model();
    model.originals = Some(originals::Loader::dead());
    open_receipt(&mut model);
    assert!(model.originals.is_none(), "a failed request gives up on it");
    assert_eq!(model.showing_original().unwrap().message_id, "receipt");

    // Gone after taking the request: the one waiting is built here.
    model.originals = Some(originals::Loader::dies_after_asking());
    select(&mut model, NEWSLETTER);
    assert!(model.open.as_ref().unwrap().loading_original());
    model.receive_originals();
    assert!(model.originals.is_none());
    assert!(!model.open.as_ref().unwrap().loading_original());
    assert_eq!(
        model.showing_original().unwrap().message_id,
        open_id(&model)
    );

    model.cache = originals::Cache::default();
    select(&mut model, "Your receipt");
    assert!(model.showing_original().is_some(), "later ones too");
}

#[test]
fn toggle_original_works_while_loading() {
    let (mut model, _folder) = loading_model();
    model.show_original = false;
    open_receipt(&mut model);
    assert!(model.open.as_ref().unwrap().loading_original());
    model.apply(Action::ToggleOriginal).unwrap();
    assert!(model.show_original);
    assert_eq!(model.notice(), None);
    assert!(model.wait_for_original(WAIT));
    assert!(model.showing_original().is_some());
}

/// Answers the open thread's pending original with `built`, as the loader
/// would.
fn answer_pending(model: &mut Model, built: Result<Option<originals::Built>, String>) {
    let open = model.open.as_ref().unwrap();
    let pending = open.loading.as_ref().unwrap();
    let loaded = originals::Loaded {
        generation: pending.generation,
        account_id: open.account_id,
        message_id: pending.message_id.clone(),
        built,
    };
    model.finish_original(loaded);
}

#[test]
fn v_while_loading_is_undone_when_it_reads_as_text_after_all() {
    let (mut model, _built, _release, _folder) = gated_model();
    model.show_original = false;
    open_receipt(&mut model);
    assert!(model.open.as_ref().unwrap().loading_original());
    model.apply(Action::ToggleOriginal).unwrap();
    assert!(model.show_original);
    assert_eq!(model.notice(), None);

    answer_pending(&mut model, Ok(None));
    assert!(!model.show_original, "the setting goes back");
    assert_eq!(
        model.notice(),
        Some("This one reads as text. h opens its HTML in your browser.")
    );
}

#[test]
fn v_while_loading_is_undone_when_the_original_fails() {
    let (mut model, _built, _release, _folder) = gated_model();
    open_receipt(&mut model);
    model.apply(Action::ToggleOriginal).unwrap();
    assert!(!model.show_original);

    answer_pending(&mut model, Err("disk on fire".into()));
    assert!(model.show_original, "the setting goes back");
    assert_eq!(
        model.notice(),
        Some("Could not show the original: disk on fire")
    );
}

#[test]
fn v_twice_while_loading_leaves_nothing_to_undo() {
    let (mut model, _built, _release, _folder) = gated_model();
    open_receipt(&mut model);
    model.apply(Action::ToggleOriginal).unwrap();
    model.apply(Action::ToggleOriginal).unwrap();
    assert!(model.show_original);

    answer_pending(&mut model, Ok(None));
    assert!(model.show_original);
    assert_eq!(model.notice(), None);
}

/// Stores a message in the first account, newest of all, and opens it.
fn open_new(model: &mut Model, message: Message) -> i64 {
    let account = model.accounts[0].id;
    let subject = message.subject.clone();
    let message = Message {
        internal_date: i64::MAX / 2,
        label_ids: vec!["INBOX".into()],
        ..message
    };
    model.store.save_messages(account, &[message]).unwrap();
    model.reload().unwrap();
    select(model, &subject);
    account
}

#[test]
fn tracking_pixels_are_taken_out_of_an_original_and_counted() {
    let (mut model, _folder) = model();
    open_new(
        &mut model,
        Message {
            gmail_id: "promo".into(),
            thread_id: "promo".into(),
            subject: "Sale".into(),
            from_addr: "news@shop.example".into(),
            body_text: "Sale now on.".into(),
            body_html: Some(
                concat!(
                    r#"<p>Sale now on.</p><img src="https://shop.example/banner.png" width="600">"#,
                    r#"<img src="https://shop.example/o?u=7" width="1" height="1">"#,
                    r#"<img src="https://x.list-manage.com/track/open.php?u=1">"#,
                )
                .into(),
            ),
            ..Message::default()
        },
    );
    let original = model.showing_original().unwrap();
    assert_eq!(original.trackers, 2);
    assert!(original.html.contains("banner.png"));
    assert!(!original.html.contains("shop.example/o?u=7"));
    assert!(!original.html.contains("list-manage"));

    // The browser loads remote images too, so h leaves them out as well.
    model.apply(Action::OpenHtml).unwrap();
    let [Effect::OpenHtml { html, .. }] = model.effects.as_slice() else {
        panic!("expected HTML, got {:?}", model.effects);
    };
    assert!(html.contains("banner.png") && !html.contains("list-manage"));

    // The demo newsletter carries one.
    select(
        &mut model,
        "This week: async traits, SQLite tips and egui 0.36",
    );
    assert_eq!(model.showing_original().unwrap().trackers, 1);
}

#[test]
fn a_reply_folds_the_message_it_quotes_until_asked() {
    let (mut model, _folder) = model();
    select(&mut model, "Code review: offline queue for triage");
    let open = model.open.as_ref().unwrap();
    assert_eq!(open.quoted_from.len(), 3);
    assert_eq!(
        open.quoted_from[0], None,
        "the first message quotes nothing"
    );
    let last = &open.messages[2];
    let from = open.quoted_from[2].expect("the last reply quotes the one before");
    assert_eq!(
        last.body_text[..from].trim_end(),
        "Works for me. Approved. Ship it."
    );
    assert!(last.body_text[from..].starts_with("On Thu, 12 Nov 2026"));
    assert_eq!(open.quotes_shown, [false, false, false]);

    model.apply(Action::ToggleQuoted(2)).unwrap();
    assert_eq!(
        model.open.as_ref().unwrap().quotes_shown,
        [false, false, true]
    );
    // Sync reloading the same thread keeps what was unfolded.
    model.reload().unwrap();
    assert_eq!(
        model.open.as_ref().unwrap().quotes_shown,
        [false, false, true]
    );
    model.apply(Action::ToggleQuoted(2)).unwrap();
    model.apply(Action::ToggleQuoted(9)).unwrap();
    assert_eq!(
        model.open.as_ref().unwrap().quotes_shown,
        [false, false, false]
    );
}

#[test]
fn a_reply_quotes_the_whole_message_even_when_it_was_folded() {
    let (mut model, _folder) = model();
    select(&mut model, "Code review: offline queue for triage");
    model.apply(Action::Reply).unwrap();
    let body = &model.compose.as_ref().unwrap().body;
    assert!(body.contains("> Works for me. Approved. Ship it."));
    assert!(body.contains("> > Good question."));
}

fn confirming(model: &Model) -> &UnsubscribeSummary {
    model.confirm_unsubscribe.as_ref().expect("a confirmation")
}

#[test]
fn unsubscribing_from_a_list_asks_first_then_sends_one_request() {
    let (mut model, _folder) = model();
    select(
        &mut model,
        "This week: async traits, SQLite tips and egui 0.36",
    );
    assert!(model.open.as_ref().unwrap().unsubscribe.is_some());
    model.apply(Action::Unsubscribe).unwrap();
    assert!(model.effects.is_empty(), "nothing goes before you confirm");
    let summary = confirming(&model);
    assert_eq!(summary.sender, "Rust Weekly");
    assert_eq!(summary.account, demo::ADDRESS);
    assert_eq!(
        summary.method,
        Method::OneClick("https://rustweekly.example/leave/demo".into())
    );
    assert_eq!(summary.method.destination(), "rustweekly.example");

    model.apply(Action::CancelUnsubscribe).unwrap();
    assert!(model.confirm_unsubscribe.is_none());
    assert!(model.effects.is_empty());

    model.apply(Action::Unsubscribe).unwrap();
    model.apply(Action::ConfirmUnsubscribe).unwrap();
    assert!(model.confirm_unsubscribe.is_none());
    let [
        Effect::Unsubscribe {
            account,
            sender,
            method,
        },
    ] = model.effects.as_slice()
    else {
        panic!("expected one request, got {:?}", model.effects);
    };
    assert_eq!(
        (account.as_str(), sender.as_str()),
        (demo::ADDRESS, "Rust Weekly")
    );
    assert!(matches!(method, Method::OneClick(_)));
    assert_eq!(model.notice(), Some("Unsubscribing…"));
    // Confirming again has nothing left to confirm.
    model.apply(Action::ConfirmUnsubscribe).unwrap();
    assert_eq!(model.effects.len(), 1);

    model
        .apply(Action::Unsubscribed {
            sender: "Rust Weekly".into(),
            result: Ok(()),
        })
        .unwrap();
    assert_eq!(model.notice(), Some("Unsubscribed from Rust Weekly."));
    assert!(model.wants_sync);
    model
        .apply(Action::Unsubscribed {
            sender: "Rust Weekly".into(),
            result: Err("Their server answered 500".into()),
        })
        .unwrap();
    assert_eq!(
        model.notice(),
        Some("Could not unsubscribe: Their server answered 500")
    );
}

#[test]
fn mail_stored_before_the_headers_were_kept_is_asked_of_the_mailbox() {
    let (mut model, _folder) = model();
    // The rail ticket says nothing about lists, and its headers are unknown.
    select(&mut model, "Your e-ticket: London to Edinburgh");
    assert!(model.open.as_ref().unwrap().unsubscribe.is_none());
    model.apply(Action::Unsubscribe).unwrap();
    assert!(model.confirm_unsubscribe.is_none());
    let [
        Effect::FindUnsubscribe {
            account,
            account_id,
            message_id,
        },
    ] = model.effects.as_slice()
    else {
        panic!("expected a lookup, got {:?}", model.effects);
    };
    assert_eq!(account, demo::ADDRESS);
    let (account_id, message_id) = (*account_id, message_id.clone());
    assert_eq!(model.notice(), Some("Checking how to unsubscribe…"));
    model.effects.clear();

    model
        .apply(Action::UnsubscribeFound {
            account_id,
            message_id: message_id.clone(),
            headers: Ok(Some((
                "<mailto:leave@northline.example?subject=stop>".into(),
                false,
            ))),
        })
        .unwrap();
    assert_eq!(model.notice(), None, "the confirmation replaces the toast");
    let summary = confirming(&model);
    assert_eq!(summary.sender, "Northline Rail");
    assert_eq!(summary.method.destination(), "leave@northline.example");
    // Kept: the reader offers it now, and the next time needs no lookup.
    assert!(model.open.as_ref().unwrap().unsubscribe.is_some());
    let stored = model
        .store
        .message(account_id, &message_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.list_unsubscribe.as_deref(),
        Some("<mailto:leave@northline.example?subject=stop>")
    );
    model.apply(Action::CancelUnsubscribe).unwrap();
    model.apply(Action::Next).unwrap();
    model.apply(Action::Previous).unwrap();
    model.apply(Action::Unsubscribe).unwrap();
    assert!(model.effects.is_empty());
    assert!(matches!(confirming(&model).method, Method::Email(_)));

    // An email request goes from the account the mail came to.
    model.apply(Action::ConfirmUnsubscribe).unwrap();
    assert!(matches!(
        model.effects.as_slice(),
        [Effect::Unsubscribe { account, method: Method::Email(_), .. }] if account == demo::ADDRESS
    ));
}

#[test]
fn an_answer_for_a_message_no_longer_on_screen_asks_nothing() {
    let (mut model, _folder) = model();
    select(&mut model, "Your e-ticket: London to Edinburgh");
    model.apply(Action::Unsubscribe).unwrap();
    let Some(Effect::FindUnsubscribe {
        account_id,
        message_id,
        ..
    }) = model.effects.pop()
    else {
        panic!("expected a lookup");
    };
    model.apply(Action::Next).unwrap();
    model
        .apply(Action::UnsubscribeFound {
            account_id,
            message_id: message_id.clone(),
            headers: Ok(Some(("<https://northline.example/u>".into(), true))),
        })
        .unwrap();
    assert!(model.confirm_unsubscribe.is_none());
    assert_eq!(model.notice(), None);
    // What was learned is still kept.
    let stored = model
        .store
        .message(account_id, &message_id)
        .unwrap()
        .unwrap();
    assert!(stored.unsubscribe_one_click);
}

#[test]
fn without_headers_the_messages_own_unsubscribe_link_is_the_way() {
    let (mut model, _folder) = model();
    let account = open_new(
        &mut model,
        Message {
            gmail_id: "digest".into(),
            thread_id: "digest".into(),
            subject: "Weekly digest".into(),
            from_addr: "digest@forum.example".into(),
            body_text: "New posts this week.\nUnsubscribe: https://forum.example/leave?u=7".into(),
            ..Message::default()
        },
    );
    // Its own words link to a page, so the reader offers it at once.
    assert_eq!(
        model.open.as_ref().unwrap().unsubscribe,
        Some(Method::Page("https://forum.example/leave?u=7".into()))
    );
    model.apply(Action::Unsubscribe).unwrap();
    // Headers unknown: the mailbox is asked, and can't be reached.
    assert!(matches!(
        model.effects.pop(),
        Some(Effect::FindUnsubscribe { .. })
    ));
    model
        .apply(Action::UnsubscribeFound {
            account_id: account,
            message_id: "digest".into(),
            headers: Err("offline".into()),
        })
        .unwrap();
    assert_eq!(confirming(&model).sender, "digest@forum.example");
    model.apply(Action::ConfirmUnsubscribe).unwrap();
    assert_eq!(
        model.effects,
        [Effect::OpenLink("https://forum.example/leave?u=7".into())]
    );
    assert_eq!(
        model.notice(),
        Some("Opened their unsubscribe page in your browser.")
    );
}

#[test]
fn a_letter_gives_no_way_to_unsubscribe() {
    let (mut model, _folder) = model();
    model.apply(Action::Unsubscribe).unwrap();
    let Some(Effect::FindUnsubscribe {
        account_id,
        message_id,
        ..
    }) = model.effects.pop()
    else {
        panic!("expected a lookup");
    };
    model
        .apply(Action::UnsubscribeFound {
            account_id,
            message_id: message_id.clone(),
            headers: Ok(Some((String::new(), false))),
        })
        .unwrap();
    assert!(model.confirm_unsubscribe.is_none());
    assert_eq!(
        model.notice(),
        Some("This sender gives no way to unsubscribe.")
    );
    // Known now, so asking again says so at once.
    model.apply(Action::Unsubscribe).unwrap();
    assert!(model.effects.is_empty());

    // With no answer and no link, it says why it couldn't tell.
    select(&mut model, "Dinner Friday?");
    model.apply(Action::Unsubscribe).unwrap();
    let Some(Effect::FindUnsubscribe {
        account_id,
        message_id,
        ..
    }) = model.effects.pop()
    else {
        panic!("expected a lookup");
    };
    model
        .apply(Action::UnsubscribeFound {
            account_id,
            message_id,
            headers: Err("offline".into()),
        })
        .unwrap();
    assert_eq!(
        model.notice(),
        Some("Could not check how to unsubscribe: offline")
    );
}

#[test]
fn your_own_mail_is_not_something_to_unsubscribe_from() {
    let (mut model, _folder) = model();
    model.apply(Action::Show("SENT".into())).unwrap();
    // The thread has mail from Sam too; a thread of only yours has none.
    let account = model.accounts[0].id;
    let mine = Message {
        gmail_id: "mine".into(),
        thread_id: "mine".into(),
        internal_date: i64::MAX / 2,
        subject: "Note to self".into(),
        from_addr: demo::ADDRESS.into(),
        label_ids: vec!["SENT".into()],
        ..Message::default()
    };
    model.store.save_messages(account, &[mine]).unwrap();
    model.reload().unwrap();
    select(&mut model, "Note to self");
    model.apply(Action::Unsubscribe).unwrap();
    assert!(model.effects.is_empty());
    assert_eq!(model.notice(), Some("Nothing to unsubscribe from here."));
}

fn suggested(model: &Model) -> Vec<&str> {
    model
        .compose
        .as_ref()
        .and_then(|compose| compose.suggestions.as_ref())
        .map(|suggestions| {
            suggestions
                .people
                .iter()
                .map(|person| person.address.as_str())
                .collect()
        })
        .unwrap_or_default()
}

fn type_to(model: &mut Model, text: &str) {
    model.compose.as_mut().unwrap().to = text.into();
    model.apply(Action::SuggestRecipients(Field::To)).unwrap();
}

#[test]
fn typing_a_recipient_suggests_people_from_your_mail() {
    let (mut model, _folder) = model();
    model.apply(Action::NewMessage).unwrap();
    type_to(&mut model, "sa");
    // Sam, whom you have written to; nobody else starts with "sa".
    assert_eq!(suggested(&model), ["sam@buildkite.test"]);
    type_to(&mut model, "xyz");
    assert!(suggested(&model).is_empty());
    assert!(model.compose.as_ref().unwrap().suggestions.is_none());

    // A name's later word, an address or its site all find someone.
    type_to(&mut model, "okaf");
    assert_eq!(suggested(&model), ["sam@buildkite.test"]);
    type_to(&mut model, "fastmail");
    assert_eq!(suggested(&model), ["maya@fastmail.test"]);
    // Yourself is never suggested, nor is anyone until a letter is typed.
    type_to(&mut model, "you@");
    assert!(suggested(&model).is_empty());
    type_to(&mut model, "");
    assert!(suggested(&model).is_empty());
}

#[test]
fn choosing_a_suggestion_fills_it_in_ready_for_the_next() {
    let (mut model, _folder) = model();
    model.apply(Action::NewMessage).unwrap();
    type_to(&mut model, "l");
    let people = suggested(&model).len();
    assert!(people >= 2, "several match l: {:?}", suggested(&model));
    // The highlight stops at both ends.
    model.apply(Action::MoveSuggestion(-1)).unwrap();
    let highlighted = |model: &Model| {
        model
            .compose
            .as_ref()
            .unwrap()
            .suggestions
            .as_ref()
            .unwrap()
            .highlighted
    };
    assert_eq!(highlighted(&model), 0);
    for _ in 0..20 {
        model.apply(Action::MoveSuggestion(1)).unwrap();
    }
    assert_eq!(highlighted(&model), people - 1);

    type_to(&mut model, "may");
    model.apply(Action::AcceptSuggestion(None)).unwrap();
    let compose = model.compose.as_mut().unwrap();
    assert_eq!(compose.to, "Maya Chen <maya@fastmail.test>, ");
    assert!(compose.suggestions.is_none());
    assert_eq!(compose.cursor_to_end, Some(Field::To));
    assert_eq!(compose.focus, Some(Field::To));

    // The next recipient starts after the comma; Maya isn't offered twice.
    compose.to.push_str("ma");
    model.apply(Action::SuggestRecipients(Field::To)).unwrap();
    assert!(!suggested(&model).contains(&"maya@fastmail.test"));
    type_to(&mut model, "Maya Chen <maya@fastmail.test>, sam");
    model.apply(Action::AcceptSuggestion(Some(0))).unwrap();
    let compose = model.compose.as_ref().unwrap();
    assert_eq!(
        compose.to,
        "Maya Chen <maya@fastmail.test>, Sam Okafor <sam@buildkite.test>, "
    );
    // The trailing comma is no recipient: the message can go, and the
    // confirmation shows the two of them.
    model.apply(Action::RequestSend).unwrap();
    assert_eq!(
        model.confirm_send.as_ref().unwrap().to,
        "Maya Chen <maya@fastmail.test>, Sam Okafor <sam@buildkite.test>"
    );
}

#[test]
fn cc_has_suggestions_of_its_own_and_esc_closes_them() {
    let (mut model, _folder) = model();
    model.apply(Action::NewMessage).unwrap();
    model.compose.as_mut().unwrap().cc = "pri".into();
    model.apply(Action::SuggestRecipients(Field::Cc)).unwrap();
    assert_eq!(suggested(&model), ["priya@lumen.example"]);
    model.apply(Action::DismissSuggestions).unwrap();
    let compose = model.compose.as_ref().unwrap();
    assert!(compose.suggestions.is_none());
    assert_eq!(compose.cc, "pri", "what was typed stays");
    assert_eq!(compose.focus, Some(Field::Cc), "typing carries on there");

    model.apply(Action::SuggestRecipients(Field::Cc)).unwrap();
    model.apply(Action::AcceptSuggestion(None)).unwrap();
    assert_eq!(
        model.compose.as_ref().unwrap().cc,
        "Priya Raman <priya@lumen.example>, "
    );
}

#[test]
fn suggestions_come_from_the_account_being_written_from() {
    let (mut model, _folder) = two_accounts();
    model.apply(Action::NewMessage).unwrap();
    let from = model.compose.as_ref().unwrap().from.clone();
    assert_eq!(from, demo::ADDRESS);
    type_to(&mut model, "len");
    assert!(suggested(&model).is_empty(), "Lena writes to the other one");
    model
        .apply(Action::SetFrom(demo::WORK_ADDRESS.into()))
        .unwrap();
    type_to(&mut model, "len");
    assert_eq!(suggested(&model), ["lena@studio.example"]);
}

#[test]
fn quick_look_previews_the_files_of_the_message_on_screen() {
    let (mut model, _folder) = model();
    select(&mut model, "Your e-ticket: London to Edinburgh");
    model.apply(Action::QuickLook).unwrap();
    let [
        Effect::Preview {
            account,
            attachments,
            ..
        },
    ] = model.effects.as_slice()
    else {
        panic!("expected a preview, got {:?}", model.effects);
    };
    assert_eq!(account, demo::ADDRESS);
    assert_eq!(attachments[0].filename, "eticket-QX741K.pdf");
    model.effects.clear();

    // One file can be asked for by itself, as its menu does.
    model
        .apply(Action::Preview {
            message: 0,
            attachment: 0,
        })
        .unwrap();
    assert!(matches!(
        model.effects.as_slice(),
        [Effect::Preview { attachments, .. }] if attachments.len() == 1
    ));
    model.effects.clear();
    model
        .apply(Action::Preview {
            message: 0,
            attachment: 5,
        })
        .unwrap();
    assert!(model.effects.is_empty());

    select(&mut model, "Dinner Friday?");
    model.apply(Action::QuickLook).unwrap();
    assert!(model.effects.is_empty());
    assert_eq!(model.notice(), Some("No attachments to preview here."));
}

#[test]
fn quick_look_leaves_out_pictures_the_original_already_shows() {
    let (mut model, _folder) = model();
    let logo = Attachment {
        part_id: "1".into(),
        filename: "logo.png".into(),
        mime_type: "image/png".into(),
        shown_in_html: true,
        ..Attachment::default()
    };
    let ticket = Attachment {
        part_id: "2".into(),
        filename: "ticket.pdf".into(),
        mime_type: "application/pdf".into(),
        ..Attachment::default()
    };
    open_new(
        &mut model,
        Message {
            gmail_id: "booking".into(),
            thread_id: "booking".into(),
            subject: "Booking".into(),
            from_addr: "desk@hotel.example".into(),
            body_html: Some(r#"<img src="cid:logo"><p>See you soon.</p>"#.into()),
            attachments: vec![logo, ticket],
            ..Message::default()
        },
    );
    model.effects.clear();
    assert!(model.showing_original().is_some());
    model.apply(Action::QuickLook).unwrap();
    let names = |model: &Model| match model.effects.as_slice() {
        [Effect::Preview { attachments, .. }] => attachments
            .iter()
            .map(|file| file.filename.clone())
            .collect::<Vec<_>>(),
        other => panic!("expected a preview, got {other:?}"),
    };
    assert_eq!(names(&model), ["ticket.pdf"]);

    // Read as text, the picture is a file like any other.
    model.effects.clear();
    model.apply(Action::ToggleOriginal).unwrap();
    model.apply(Action::QuickLook).unwrap();
    assert_eq!(names(&model), ["logo.png", "ticket.pdf"]);
}
