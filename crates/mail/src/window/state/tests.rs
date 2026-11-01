//! Drives the window model against the demo mailbox, as a person would.

use std::time::{Duration, Instant};

use super::*;
use crate::window::demo;

fn model() -> (Model, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mail.sqlite3");
    demo::seed(&path).unwrap();
    let store = Store::open(&path).unwrap();
    let account = store.account().unwrap().unwrap();
    (Model::new(store, account).unwrap(), folder)
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
    assert_eq!(model.unread.get("INBOX"), Some(&3));
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
    assert_eq!(model.notice.as_deref(), Some("Undone."));
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
    assert_eq!(model.notice.as_deref(), Some("Not in the inbox."));
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
    assert_eq!(
        model.notice.as_deref(),
        Some("Labeled Travel. Press z to undo.")
    );

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
