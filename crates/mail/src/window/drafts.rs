//! Saves and discards drafts on short-lived threads, so the window never
//! waits on the network. Each call starts its own Gmail client: drafts are
//! rare, and the sync thread may be busy with a long download.

use std::{sync::mpsc::Sender, thread};

use mail_core::{
    auth::{OAuthClient, keychain},
    compose::{self, Draft},
    gmail::{GmailApi, GmailClient, Outcome},
    paths::Paths,
};

pub enum Finished {
    Saved(String),
    Discarded,
    Failed(String),
}

pub fn save(
    paths: &Paths,
    from: &str,
    draft: Draft,
    existing: Option<String>,
    done: Sender<Finished>,
    wake: impl Fn() + Send + 'static,
) {
    let paths = paths.clone();
    let from = from.to_owned();
    thread::spawn(move || {
        let result = client(&paths, &from)
            .and_then(|gmail| compose::save(&gmail, &from, &draft, existing.as_deref()));
        let _ = done.send(match result {
            Ok(id) => Finished::Saved(id),
            Err(error) => Finished::Failed(format!("Could not save the draft: {error:#}")),
        });
        wake();
    });
}

pub fn discard(
    paths: &Paths,
    from: &str,
    draft_id: String,
    done: Sender<Finished>,
    wake: impl Fn() + Send + 'static,
) {
    let paths = paths.clone();
    let from = from.to_owned();
    thread::spawn(move || {
        let result = client(&paths, &from).and_then(|gmail| gmail.delete_draft(&draft_id));
        let _ = done.send(match result {
            Ok(Outcome::Done | Outcome::Gone) => Finished::Discarded,
            Ok(Outcome::Rejected(message)) => {
                Finished::Failed(format!("Gmail kept the draft: {message}"))
            }
            Err(error) => Finished::Failed(format!("Could not discard the draft: {error:#}")),
        });
        wake();
    });
}

fn client(paths: &Paths, email: &str) -> anyhow::Result<GmailClient> {
    Ok(GmailClient::new(
        OAuthClient::load(&paths.oauth_client())?,
        keychain::load(email)?,
        None,
    ))
}
