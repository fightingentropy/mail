//! Sync on a thread of its own while the window is open: at launch, every
//! couple of minutes, and right after triage so changes reach Gmail quickly.
//! The thread ends with the window; nothing keeps running after Mail quits.

use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

use anyhow::Context;
use mail_core::{
    auth::{OAuthClient, keychain},
    gmail::GmailClient,
    paths::Paths,
    store::Store,
    sync::{self, Progress, SyncOptions, SyncReport},
};

const EVERY: Duration = Duration::from_secs(120);

pub enum Event {
    Progress(Progress),
    Synced(SyncReport),
    Failed(String),
}

enum Request {
    SyncNow,
}

pub struct Syncer {
    requests: Sender<Request>,
    pub events: Receiver<Event>,
}

impl Syncer {
    /// Starts syncing at once. `wake` is called whenever there is news, so
    /// the window can repaint.
    pub fn start(paths: Paths, wake: impl Fn() + Send + 'static) -> Self {
        let (requests, incoming) = mpsc::channel();
        let (outgoing, events) = mpsc::channel();
        thread::Builder::new()
            .name("sync".into())
            .spawn(move || run(paths, incoming, outgoing, wake))
            .expect("could not start the sync thread");
        Self { requests, events }
    }

    pub fn sync_now(&self) {
        let _ = self.requests.send(Request::SyncNow);
    }
}

fn run(paths: Paths, requests: Receiver<Request>, events: Sender<Event>, wake: impl Fn()) {
    let mut connection: Option<(Store, GmailClient)> = None;
    loop {
        let result = sync_once(&paths, &mut connection, &events, &wake);
        let event = match result {
            Ok(report) => Event::Synced(report),
            Err(error) => {
                // Start fresh next time: the token or database may have changed.
                connection = None;
                Event::Failed(format!("{error:#}"))
            }
        };
        if events.send(event).is_err() {
            return;
        }
        wake();

        match requests.recv_timeout(EVERY) {
            Ok(Request::SyncNow) | Err(RecvTimeoutError::Timeout) => {
                // Several triage actions in a row need only one sync.
                while requests.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn sync_once(
    paths: &Paths,
    connection: &mut Option<(Store, GmailClient)>,
    events: &Sender<Event>,
    wake: &impl Fn(),
) -> anyhow::Result<SyncReport> {
    if connection.is_none() {
        *connection = Some(connect(paths)?);
    }
    let (store, gmail) = connection.as_mut().expect("connected above");
    // Read the account each time: the sync point moves.
    let account = store
        .account()?
        .context("Not signed in. Run `mail auth`.")?;
    sync::sync(
        store,
        gmail,
        &account,
        &SyncOptions::default(),
        &mut |progress| {
            let _ = events.send(Event::Progress(progress));
            wake();
        },
    )
}

/// Opens the database and a Gmail client for the signed-in account.
pub fn connect(paths: &Paths) -> anyhow::Result<(Store, GmailClient)> {
    let store = Store::open(&paths.database())?;
    let account = store
        .account()?
        .context("Not signed in. Run `mail auth`.")?;
    let gmail = GmailClient::new(
        OAuthClient::load(&paths.oauth_client())?,
        keychain::load(&account.email)?,
        None,
    );
    Ok((store, gmail))
}
