//! Sync on a thread of its own while the window is open: at launch, every
//! couple of minutes, and right after triage so changes reach Gmail quickly.
//! The thread ends with the window; nothing keeps running after Mail quits.

use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    path::Path,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, SystemTime},
};

use anyhow::bail;
use mail_core::{
    model::Account,
    paths::Paths,
    store::Store,
    sync::{self, Progress, SyncOptions, SyncReport},
};

use crate::accounts;

const EVERY: Duration = Duration::from_secs(120);
/// Profile photos are fetched again after this, in case they changed.
const PHOTO_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub enum Event {
    Progress(Progress),
    /// The accounts that synced, added together.
    Synced(SyncReport),
    Failed(String),
    /// An account's profile photo was saved.
    Photo(String),
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

/// The database and a Gmail client per account, kept between syncs.
struct Connection {
    store: Store,
    clients: HashMap<String, accounts::Client>,
}

fn run(paths: Paths, requests: Receiver<Request>, events: Sender<Event>, wake: impl Fn()) {
    let mut connection: Option<Connection> = None;
    // Each account's photo is tried at most once while the window is open.
    let mut photos_tried = HashSet::new();
    loop {
        let (report, failures) =
            match sync_all(&paths, &mut connection, &mut photos_tried, &events, &wake) {
                Ok(result) => result,
                Err(error) => (None, vec![format!("{error:#}")]),
            };
        // While a big first sync still owes bodies, go again straight away.
        let mut more_to_fill = false;
        if let Some(report) = report {
            more_to_fill = report.bodies_pending > 0 && report.bodies_filled > 0;
            if events.send(Event::Synced(report)).is_err() {
                return;
            }
        }
        if !failures.is_empty() {
            // Start fresh next time: a token or the database may have changed.
            connection = None;
            if events.send(Event::Failed(failures.join("; "))).is_err() {
                return;
            }
        }
        wake();

        let wait = if more_to_fill { Duration::ZERO } else { EVERY };
        match requests.recv_timeout(wait) {
            Ok(Request::SyncNow) | Err(RecvTimeoutError::Timeout) => {
                // Several triage actions in a row need only one sync.
                while requests.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Syncs each account in turn. Returns the combined report of the accounts
/// that synced, and a line for each that didn't, so one failing account
/// doesn't hold up the others.
fn sync_all(
    paths: &Paths,
    connection: &mut Option<Connection>,
    photos_tried: &mut HashSet<String>,
    events: &Sender<Event>,
    wake: &impl Fn(),
) -> anyhow::Result<(Option<SyncReport>, Vec<String>)> {
    if connection.is_none() {
        *connection = Some(Connection {
            store: Store::open(&paths.database())?,
            clients: HashMap::new(),
        });
    }
    let Connection { store, clients } = connection.as_mut().expect("connected above");
    // Read the accounts each time: sync points move, and accounts get added.
    let accounts = store.accounts()?;
    if accounts.is_empty() {
        bail!("Not signed in. Run `mail auth`.");
    }
    let mut combined: Option<SyncReport> = None;
    let mut failures = Vec::new();
    for account in &accounts {
        match sync_account(paths, store, clients, account, events, wake) {
            Ok(report) => {
                combined = Some(match combined {
                    Some(total) => add(total, report),
                    None => report,
                });
            }
            Err(error) => {
                clients.remove(&account.email);
                failures.push(if accounts.len() > 1 {
                    format!("{}: {error:#}", account.email)
                } else {
                    format!("{error:#}")
                });
                continue;
            }
        }
        let photo = paths.photo(&account.email);
        if is_stale(&photo)
            && photos_tried.insert(account.email.clone())
            && let Some(gmail) = clients.get(&account.email)
            && accounts::save_photo(paths, &**gmail, &account.email).unwrap_or(false)
        {
            let _ = events.send(Event::Photo(account.email.clone()));
        }
    }
    Ok((combined, failures))
}

fn sync_account(
    paths: &Paths,
    store: &mut Store,
    clients: &mut HashMap<String, accounts::Client>,
    account: &Account,
    events: &Sender<Event>,
    wake: &impl Fn(),
) -> anyhow::Result<SyncReport> {
    let gmail = match clients.entry(account.email.clone()) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => entry.insert(accounts::client(paths, &account.email)?),
    };
    sync::sync(
        store,
        &**gmail,
        account,
        &SyncOptions::default(),
        &mut |progress| {
            let _ = events.send(Event::Progress(progress));
            wake();
        },
    )
}

/// Two accounts' reports as one, for the status bar.
fn add(mut total: SyncReport, report: SyncReport) -> SyncReport {
    total.downloaded += report.downloaded;
    total.relabeled += report.relabeled;
    total.deleted += report.deleted;
    total.pushed += report.pushed;
    total.rejected += report.rejected;
    total.bodies_filled += report.bodies_filled;
    total.bodies_pending += report.bodies_pending;
    total
}

/// Missing, or older than [`PHOTO_FOR`].
fn is_stale(photo: &Path) -> bool {
    let modified = photo.metadata().and_then(|metadata| metadata.modified());
    match modified {
        Ok(modified) => SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age > PHOTO_FOR),
        Err(_) => true,
    }
}
