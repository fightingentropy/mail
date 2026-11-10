//! iCloud Mail, which has no API like Gmail's: Mail reads and files it over
//! IMAP and sends it over SMTP, signed in with an app-specific password.
//!
//! [`IcloudClient`] answers the same calls as Gmail's API, so sync, the
//! store, search and the window treat both alike. Folders stand for labels
//! (see [`folders`]): Inbox is INBOX, Archive is having no label, as
//! archived Gmail is, and folders you made are your labels. Gmail's history
//! has no IMAP equivalent, so every sync lists the folders again: a few
//! requests per folder, with headers only for messages Mail hasn't seen.

mod folders;
mod index;
mod mime;
mod smtp;
#[cfg(test)]
mod tests;
mod tls;

use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use imap::types::Flag;
use serde::{Deserialize, Serialize};

use crate::gmail::{
    ApiLabel, ApiMessage, GmailApi, HistoryPage, IdPage, MessageRef, Outcome, Profile,
};
use folders::{Folder, Role};
use index::{Entry, Index, Location, Refusal, Step};
use tls::TlsStream;

const IMAP_HOST: &str = "imap.mail.me.com";
const IMAP_PORT: u16 = 993;

/// Sync's history ID for iCloud. There is no history to replay, so each
/// sync is a full one, which for IMAP costs little.
const NO_HISTORY: &str = "icloud";

/// How far back the folders are read when sync hasn't said.
const DEFAULT_DAYS: u32 = 90;

/// How Mail signs in to an iCloud account: kept in the Keychain as JSON,
/// where a Gmail account keeps its refresh token.
#[derive(Clone, Serialize, Deserialize)]
pub struct Login {
    /// What IMAP accepted: the name part of the address, or all of it.
    pub user: String,
    /// The app-specific password.
    pub password: String,
}

impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Login")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl Login {
    /// The login, if a Keychain secret is one rather than a Google token.
    pub fn from_secret(secret: &str) -> Option<Self> {
        serde_json::from_str(secret).ok()
    }

    pub fn to_secret(&self) -> String {
        serde_json::to_string(self).expect("a login serializes")
    }
}

pub struct IcloudClient {
    address: String,
    login: Login,
    /// Signed-in connections not in use. Sync downloads on a few threads,
    /// each with a connection of its own.
    idle: Mutex<Vec<Connection>>,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    folders: Option<Vec<Folder>>,
    index: Option<Arc<Index>>,
    /// Threads this client has changed since the index was read. Their
    /// entries are out of date (moved copies have new UIDs); the rest of
    /// the index is still right.
    changed: HashSet<String>,
    /// How far back sync last asked for mail.
    days: Option<u32>,
    /// The last message read whole: attachments and inline pictures are
    /// read from it one after another.
    last: Option<(String, Arc<Vec<u8>>)>,
}

impl IcloudClient {
    pub fn new(address: &str, login: Login) -> Self {
        Self {
            address: address.to_owned(),
            login,
            idle: Mutex::new(Vec::new()),
            cache: Mutex::new(Cache::default()),
        }
    }

    /// Checks an address and app-specific password with iCloud. Apple asks
    /// IMAP for the name part of the address, or the whole address if that
    /// fails; the one that worked is in the returned login.
    pub fn sign_in(address: &str, password: &str) -> anyhow::Result<(Self, Login)> {
        let address = address.trim();
        let password = password.trim();
        anyhow::ensure!(
            address.contains('@') && !password.is_empty(),
            "Enter your iCloud address and an app-specific password."
        );
        let name = address.split('@').next().unwrap_or(address);
        for user in [name, address] {
            let login = Login {
                user: user.to_owned(),
                password: password.to_owned(),
            };
            match Connection::open(&login) {
                Ok(connection) => {
                    let client = Self::new(address, login.clone());
                    client.idle.lock().unwrap().push(connection);
                    return Ok((client, login));
                }
                Err(error) if is_refusal(&error) => {}
                Err(error) => return Err(error),
            }
        }
        // Both refused: almost always the Apple Account password, which
        // iCloud only takes in Apple's own apps.
        bail!(
            "iCloud didn't accept that password. It needs an app-specific password, \
             not the one you sign in to Apple with."
        )
    }

    /// Runs `work` on a signed-in connection: an idle one that still
    /// answers, or a new one. A connection that failed is closed rather than
    /// reused.
    fn with<T>(
        &self,
        work: impl FnOnce(&mut Connection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let mut connection = loop {
            let idle = self.idle.lock().unwrap().pop();
            match idle {
                Some(mut connection) => {
                    if connection.is_alive() {
                        break connection;
                    }
                    // Closed by iCloud while the Mac slept, say.
                }
                None => {
                    break Connection::open(&self.login).context("Could not sign in to iCloud")?;
                }
            }
        };
        let result = work(&mut connection);
        if result.is_ok() {
            connection.used = Instant::now();
            self.idle.lock().unwrap().push(connection);
        }
        result
    }

    fn folders(&self) -> anyhow::Result<Vec<Folder>> {
        if let Some(folders) = &self.cache.lock().unwrap().folders {
            return Ok(folders.clone());
        }
        let folders = self.with(|connection| {
            let names = connection.session.list(Some(""), Some("*"))?;
            Ok(folders::from_list(
                names.iter().map(|name| (name.name(), name.attributes())),
            ))
        })?;
        self.cache.lock().unwrap().folders = Some(folders.clone());
        Ok(folders)
    }

    fn folder(&self, role: Role) -> anyhow::Result<String> {
        self.folders()?
            .into_iter()
            .find(|folder| folder.role == role)
            .map(|folder| folder.name)
            .with_context(|| format!("iCloud has no {role:?} folder"))
    }

    /// Where every message is, read once and kept until something moves.
    fn index(&self) -> anyhow::Result<Arc<Index>> {
        let days = {
            let cache = self.cache.lock().unwrap();
            if let Some(index) = &cache.index {
                return Ok(index.clone());
            }
            cache.days.unwrap_or(DEFAULT_DAYS)
        };
        let folders = self.folders()?;
        let index =
            Arc::new(self.with(|connection| connection.index(folders, days, &self.address))?);
        self.cache.lock().unwrap().index = Some(index.clone());
        Ok(index)
    }

    fn forget_index(&self) {
        let mut cache = self.cache.lock().unwrap();
        cache.index = None;
        cache.changed.clear();
    }

    /// Like [`Self::index`], but read again first if the thread that
    /// `thread_of` names from the cached index has changed since. Changing
    /// one thread leaves the others' entries right, so filing many threads
    /// in a row does not rescan the mailbox for each.
    fn index_for(
        &self,
        thread_of: impl FnOnce(&Index) -> Option<String>,
    ) -> anyhow::Result<Arc<Index>> {
        {
            let mut cache = self.cache.lock().unwrap();
            if let Some(index) = cache.index.clone() {
                match thread_of(&index) {
                    Some(thread) if cache.changed.contains(&thread) => {
                        cache.index = None;
                        cache.changed.clear();
                    }
                    _ => return Ok(index),
                }
            }
        }
        self.index()
    }

    /// Notes that a thread is about to change, so its entries are not trusted.
    fn mark_changed(&self, thread_id: &str) {
        self.cache
            .lock()
            .unwrap()
            .changed
            .insert(thread_id.to_owned());
    }

    /// A whole message, or `None` if that copy is gone.
    fn read(&self, folder: &str, location: &Location) -> anyhow::Result<Option<Arc<Vec<u8>>>> {
        let key = format!("{}:{}@{folder}", location.uid_validity, location.uid);
        if let Some((last, raw)) = &self.cache.lock().unwrap().last
            && *last == key
        {
            return Ok(Some(raw.clone()));
        }
        let raw = self.with(|connection| {
            if !connection.open_copy(folder, location, false)? {
                return Ok(None);
            }
            connection.fetch_one(location.uid, "BODY.PEEK[]", |fetch| fetch.body())
        })?;
        let raw = raw.map(Arc::new);
        if let Some(raw) = &raw {
            self.cache.lock().unwrap().last = Some((key, raw.clone()));
        }
        Ok(raw)
    }

    /// Plans changes to each message of a thread and makes them.
    fn refile(
        &self,
        thread_id: &str,
        plan: impl Fn(&Index, &Entry) -> Result<Vec<Step>, Refusal>,
    ) -> anyhow::Result<Outcome> {
        let index = self.index_for(|_| Some(thread_id.to_owned()))?;
        let thread = index.thread(thread_id);
        if thread.is_empty() {
            return Ok(Outcome::Gone);
        }
        let mut steps = Vec::new();
        for entry in thread {
            match plan(&index, entry) {
                Ok(more) => steps.extend(more),
                Err(refusal) => return Ok(Outcome::Rejected(refusal)),
            }
        }
        if steps.is_empty() {
            return Ok(Outcome::Done);
        }
        // Even a change that stops halfway moves things.
        self.mark_changed(thread_id);
        self.with(|connection| {
            for step in &steps {
                connection.run(&index.folders, step)?;
            }
            Ok(())
        })?;
        Ok(Outcome::Done)
    }

    fn message_for(&self, id: &str, whole: bool) -> anyhow::Result<Option<ApiMessage>> {
        let index = self.index_for(|index| index.entries.get(id).map(|e| e.thread_id.clone()))?;
        let Some(entry) = index.entries.get(id) else {
            return Ok(None);
        };
        let Some(location) = index.best_location(entry) else {
            return Ok(None);
        };
        let folder = &index.folders[location.folder].name;
        let meta = |size: usize| mime::Meta {
            id: entry.id.clone(),
            thread_id: entry.thread_id.clone(),
            label_ids: index.labels(entry),
            internal_date: entry.date,
            size: size as i64,
        };
        if !whole {
            let headers = self.with(|connection| {
                if !connection.open_copy(folder, location, false)? {
                    return Ok(None);
                }
                connection.fetch_one(location.uid, "BODY.PEEK[HEADER]", |fetch| fetch.header())
            })?;
            return Ok(headers.map(|headers| mime::headers_only(&headers, meta(headers.len()))));
        }
        let Some(raw) = self.read(folder, location)? else {
            return Ok(None);
        };
        Ok(Some(mime::full(&raw, meta(raw.len()), &|path| {
            attachment_id(folder, location, path)
        })))
    }
}

impl GmailApi for IcloudClient {
    /// Each sync starts here, so folders and messages are read afresh.
    fn profile(&self) -> anyhow::Result<Profile> {
        {
            let mut cache = self.cache.lock().unwrap();
            cache.folders = None;
            cache.index = None;
        }
        Ok(Profile {
            email_address: self.address.clone(),
            history_id: NO_HISTORY.to_owned(),
            messages_total: 0,
        })
    }

    fn labels(&self) -> anyhow::Result<Vec<ApiLabel>> {
        let mut labels: Vec<ApiLabel> = ["UNREAD", "STARRED"]
            .into_iter()
            .map(|id| ApiLabel {
                id: id.to_owned(),
                name: id.to_owned(),
                kind: "system".to_owned(),
            })
            .collect();
        for folder in self.folders()? {
            let Some(id) = folder.label() else {
                continue;
            };
            let user = folder.role == Role::Other;
            labels.push(ApiLabel {
                name: if user {
                    folder.display_name()
                } else {
                    id.clone()
                },
                id,
                kind: if user { "user" } else { "system" }.to_owned(),
            });
        }
        Ok(labels)
    }

    /// Everything since `newer_than:Nd`, read afresh: this is where a sync
    /// starts.
    fn message_ids(&self, query: &str, _page_token: Option<&str>) -> anyhow::Result<IdPage> {
        let days = query
            .strip_prefix("newer_than:")
            .and_then(|rest| rest.strip_suffix('d'))
            .and_then(|days| days.parse().ok())
            .unwrap_or(DEFAULT_DAYS);
        {
            let mut cache = self.cache.lock().unwrap();
            cache.days = Some(days);
            cache.index = None;
            cache.changed.clear();
            cache.folders = None;
        }
        let index = self.index()?;
        let mut listed: Vec<&Entry> = index.listed().collect();
        listed.sort_by_key(|entry| std::cmp::Reverse(entry.date));
        Ok(IdPage {
            messages: listed
                .into_iter()
                .map(|entry| MessageRef {
                    id: entry.id.clone(),
                    thread_id: entry.thread_id.clone(),
                    label_ids: index.labels(entry),
                })
                .collect(),
            next_page_token: None,
        })
    }

    fn message(&self, id: &str) -> anyhow::Result<Option<ApiMessage>> {
        self.message_for(id, true)
    }

    fn message_headers(&self, id: &str) -> anyhow::Result<Option<ApiMessage>> {
        self.message_for(id, false)
    }

    fn message_labels(&self, id: &str) -> anyhow::Result<Option<Vec<String>>> {
        let index = self.index_for(|index| index.entries.get(id).map(|e| e.thread_id.clone()))?;
        Ok(index.entries.get(id).map(|entry| index.labels(entry)))
    }

    fn attachment(
        &self,
        _message_id: &str,
        attachment_id: &str,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let Some((folder, location, path)) = parse_attachment_id(attachment_id) else {
            return Ok(None);
        };
        let Some(raw) = self.read(&folder, &location)? else {
            return Ok(None);
        };
        Ok(mime::part_data(&raw, &path))
    }

    fn headers_first(&self) -> bool {
        false
    }

    fn history(
        &self,
        _start_history_id: &str,
        _page_token: Option<&str>,
    ) -> anyhow::Result<Option<HistoryPage>> {
        Ok(None)
    }

    fn modify_thread(
        &self,
        thread_id: &str,
        add: &[String],
        remove: &[String],
    ) -> anyhow::Result<Outcome> {
        self.refile(thread_id, |index, entry| {
            index::file(index, entry, add, remove)
        })
    }

    fn trash_thread(&self, thread_id: &str) -> anyhow::Result<Outcome> {
        self.refile(thread_id, index::trash)
    }

    fn untrash_thread(&self, thread_id: &str) -> anyhow::Result<Outcome> {
        self.refile(thread_id, index::untrash)
    }

    /// The draft ID is the draft's Message-ID, which each saved version
    /// keeps, so the draft is found again in Drafts.
    fn create_draft(&self, raw: &[u8], _thread_id: Option<&str>) -> anyhow::Result<String> {
        let drafts = self.folder(Role::Drafts)?;
        let draft_id = mime::new_message_id(&self.address);
        self.with(|connection| {
            connection.append(&drafts, &mime::stamped(raw, &draft_id, false), true)
        })?;
        self.forget_index();
        Ok(draft_id)
    }

    fn update_draft(
        &self,
        draft_id: &str,
        raw: &[u8],
        _thread_id: Option<&str>,
    ) -> anyhow::Result<Outcome> {
        let drafts = self.folder(Role::Drafts)?;
        let outcome = self.with(|connection| {
            let saved = connection.find(&drafts, draft_id)?;
            if saved.is_empty() {
                return Ok(Outcome::Gone);
            }
            connection.append(&drafts, &mime::stamped(raw, draft_id, false), true)?;
            connection.remove_all(&drafts, &saved)?;
            Ok(Outcome::Done)
        })?;
        self.forget_index();
        Ok(outcome)
    }

    fn delete_draft(&self, draft_id: &str) -> anyhow::Result<Outcome> {
        let drafts = self.folder(Role::Drafts)?;
        let outcome = self.with(|connection| {
            let saved = connection.find(&drafts, draft_id)?;
            if saved.is_empty() {
                return Ok(Outcome::Gone);
            }
            connection.remove_all(&drafts, &saved)?;
            Ok(Outcome::Done)
        })?;
        self.forget_index();
        Ok(outcome)
    }

    /// Sends the saved draft over SMTP, then files a copy in Sent and
    /// removes the draft, as iCloud does neither for a client that sends.
    fn send_draft(&self, draft_id: &str) -> anyhow::Result<String> {
        let drafts = self.folder(Role::Drafts)?;
        let raw = self.with(|connection| {
            let Some(&uid) = connection.find(&drafts, draft_id)?.first() else {
                bail!("The draft is no longer in iCloud's Drafts");
            };
            connection
                .fetch_one(uid, "BODY.PEEK[]", |fetch| fetch.body())?
                .context("The draft is no longer in iCloud's Drafts")
        })?;
        let copy = mime::stamped(&raw, draft_id, false);
        let outgoing = mime::stamped(&copy, draft_id, true);
        smtp::send(
            &self.address,
            &self.login.password,
            &mime::recipients(&raw),
            &outgoing,
        )?;
        // It went. Filing must not fail the send, or you would send it twice.
        let sent = self.folder(Role::Sent).ok();
        let _ = self.with(|connection| {
            if let Some(sent) = &sent {
                connection.append(sent, &copy, false)?;
            }
            let saved = connection.find(&drafts, draft_id)?;
            connection.remove_all(&drafts, &saved)
        });
        self.forget_index();
        Ok(index::key(draft_id))
    }
}

/// An attachment is found by the copy it is in and its part number:
/// `uidvalidity:uid:part@folder`.
fn attachment_id(folder: &str, location: &Location, path: &str) -> String {
    format!("{}:{}:{path}@{folder}", location.uid_validity, location.uid)
}

fn parse_attachment_id(id: &str) -> Option<(String, Location, String)> {
    let (copy, folder) = id.split_once('@')?;
    let mut fields = copy.splitn(3, ':');
    let uid_validity = fields.next()?.parse().ok()?;
    let uid = fields.next()?.parse().ok()?;
    let path = fields.next()?.to_owned();
    let location = Location {
        folder: 0,
        uid_validity,
        uid,
        seen: true,
        flagged: false,
    };
    Some((folder.to_owned(), location, path))
}

/// iCloud said no, as to a wrong password, rather than failing to answer.
fn is_refusal(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| matches!(cause.downcast_ref(), Some(imap::Error::No(_))))
}

/// One signed-in IMAP connection and the folder it has open.
struct Connection {
    session: imap::Session<TlsStream>,
    /// The open folder, whether it is open for changes, and its UIDVALIDITY.
    open: Option<(String, bool, u32)>,
    can_move: bool,
    uidplus: bool,
    used: Instant,
}

impl Connection {
    fn open(login: &Login) -> anyhow::Result<Self> {
        let stream = tls::wrap(tls::tcp(IMAP_HOST, IMAP_PORT)?, IMAP_HOST)?;
        let mut client = imap::Client::new(stream);
        client.read_greeting().context("iCloud didn't answer")?;
        let mut session = client
            .login(&login.user, &login.password)
            .map_err(|(error, _)| error)?;
        let capabilities = session.capabilities()?;
        Ok(Self {
            can_move: capabilities.has_str("MOVE"),
            uidplus: capabilities.has_str("UIDPLUS"),
            session,
            open: None,
            used: Instant::now(),
        })
    }

    /// Whether the server still answers, asked only after a quiet minute.
    fn is_alive(&mut self) -> bool {
        self.used.elapsed() < Duration::from_secs(60) || self.session.noop().is_ok()
    }

    /// Opens `folder`, read-only unless `write`, and returns its
    /// UIDVALIDITY and message count.
    fn select(&mut self, folder: &str, write: bool) -> anyhow::Result<(u32, u32)> {
        let mailbox = if write {
            self.session.select(folder)
        } else {
            self.session.examine(folder)
        }
        .with_context(|| format!("Could not open {}", folders::decode_utf7(folder)))?;
        let uid_validity = mailbox.uid_validity.unwrap_or_default();
        self.open = Some((folder.to_owned(), write, uid_validity));
        Ok((uid_validity, mailbox.exists))
    }

    /// Opens the folder a copy is in, unless it is open already. False if
    /// the folder's UIDs were reset since, so the copy's UID means nothing.
    fn open_copy(
        &mut self,
        folder: &str,
        location: &Location,
        write: bool,
    ) -> anyhow::Result<bool> {
        let uid_validity = match &self.open {
            Some((open, writable, uid_validity)) if open == folder && (*writable || !write) => {
                *uid_validity
            }
            _ => self.select(folder, write)?.0,
        };
        Ok(uid_validity == location.uid_validity)
    }

    fn fetch_one(
        &mut self,
        uid: u32,
        item: &str,
        take: impl for<'f> Fn(&'f imap::types::Fetch<'f>) -> Option<&'f [u8]>,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let fetches = self
            .session
            .uid_fetch(uid.to_string(), format!("(UID {item})"))?;
        Ok(fetches
            .iter()
            .filter(|fetch| fetch.uid == Some(uid))
            .find_map(|fetch| take(fetch).map(<[u8]>::to_vec)))
    }

    /// Lists every folder sync reads since `days` ago, with the few headers
    /// that say what each message is and which thread it belongs to.
    fn index(&mut self, folders: Vec<Folder>, days: u32, address: &str) -> anyhow::Result<Index> {
        let since = mime::imap_date(days);
        let mut index = Index {
            folders,
            entries: HashMap::new(),
        };
        for number in 0..index.folders.len() {
            if index.folders[number].role == Role::Junk {
                continue;
            }
            let name = index.folders[number].name.clone();
            let (uid_validity, exists) = self.select(&name, false)?;
            if exists == 0 {
                continue;
            }
            let mut uids: Vec<u32> = self
                .session
                .uid_search(format!("SINCE {since}"))?
                .into_iter()
                .collect();
            uids.sort_unstable();
            for chunk in uids.chunks(500) {
                let set: Vec<String> = chunk.iter().map(u32::to_string).collect();
                let fetches = self.session.uid_fetch(
                    set.join(","),
                    "(UID FLAGS INTERNALDATE \
                     BODY.PEEK[HEADER.FIELDS (MESSAGE-ID IN-REPLY-TO REFERENCES FROM)])",
                )?;
                for fetch in fetches.iter() {
                    let Some(uid) = fetch.uid else {
                        continue;
                    };
                    let location = Location {
                        folder: number,
                        uid_validity,
                        uid,
                        seen: fetch.flags().contains(&Flag::Seen),
                        flagged: fetch.flags().contains(&Flag::Flagged),
                    };
                    let date = fetch
                        .internal_date()
                        .map(|date| date.timestamp_millis())
                        .unwrap_or_default();
                    index.add(location, fetch.header().unwrap_or_default(), date, address);
                }
            }
        }
        Ok(index)
    }

    fn run(&mut self, folders: &[Folder], step: &Step) -> anyhow::Result<()> {
        match step {
            Step::Flag { location, flag, on } => {
                if self.open_copy(&folders[location.folder].name, location, true)? {
                    let sign = if *on { '+' } else { '-' };
                    self.session.uid_store(
                        location.uid.to_string(),
                        format!("{sign}FLAGS.SILENT ({flag})"),
                    )?;
                }
            }
            Step::Copy { location, to } => {
                if self.open_copy(&folders[location.folder].name, location, false)? {
                    self.session
                        .uid_copy(location.uid.to_string(), quoted(&folders[*to].name))?;
                }
            }
            Step::Move { location, to } => {
                if self.open_copy(&folders[location.folder].name, location, true)? {
                    if self.can_move {
                        self.session
                            .uid_mv(location.uid.to_string(), &folders[*to].name)?;
                    } else {
                        self.session
                            .uid_copy(location.uid.to_string(), quoted(&folders[*to].name))?;
                        self.remove(location.uid)?;
                    }
                }
            }
            Step::Remove { location } => {
                if self.open_copy(&folders[location.folder].name, location, true)? {
                    self.remove(location.uid)?;
                }
            }
        }
        Ok(())
    }

    /// Removes a message from the open folder for good.
    fn remove(&mut self, uid: u32) -> anyhow::Result<()> {
        self.session
            .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")?;
        if self.uidplus {
            self.session.uid_expunge(uid.to_string())?;
        } else {
            self.session.expunge()?;
        }
        Ok(())
    }

    fn remove_all(&mut self, folder: &str, uids: &[u32]) -> anyhow::Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.select(folder, true)?;
        for &uid in uids {
            self.remove(uid)?;
        }
        Ok(())
    }

    /// The UIDs in `folder` of messages with this Message-ID. Read from the
    /// headers rather than searched for, since a draft saved a moment ago
    /// may not be in iCloud's search yet.
    fn find(&mut self, folder: &str, message_id: &str) -> anyhow::Result<Vec<u32>> {
        let (_, exists) = self.select(folder, true)?;
        if exists == 0 {
            return Ok(Vec::new());
        }
        let fetches = self
            .session
            .uid_fetch("1:*", "(UID BODY.PEEK[HEADER.FIELDS (MESSAGE-ID)])")?;
        Ok(fetches
            .iter()
            .filter(|fetch| {
                fetch
                    .header()
                    .and_then(|headers| mime::header_value(headers, "Message-ID"))
                    .is_some_and(|found| found == message_id)
            })
            .filter_map(|fetch| fetch.uid)
            .collect())
    }

    /// Saves a message to `folder`, read, and as a draft if `draft`.
    fn append(&mut self, folder: &str, raw: &[u8], draft: bool) -> anyhow::Result<()> {
        let mut flags = vec![Flag::Seen];
        if draft {
            flags.push(Flag::Draft);
        }
        self.session
            .append(folder, raw)
            .flags(flags)
            .finish()
            .with_context(|| format!("Could not save to {}", folders::decode_utf7(folder)))?;
        Ok(())
    }
}

/// A folder name as an IMAP quoted string.
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}
