//! Builds a message's original away from the window thread, and keeps the
//! last few built. Inflating the HTML, reading the pictures sent inside it
//! and embedding them can take long enough on a large newsletter to make a
//! held j or k stutter, so the window asks for an original and carries on.

use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use mail_core::{gmail::inline, store::Store};

/// What showing a message's original needs, before the remote-images choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Built {
    /// With the pictures sent inside it in place, as far as Mail has them.
    pub html: Arc<str>,
    /// How many of those pictures are in place.
    pub inline: usize,
    /// The Content-IDs of pictures it shows that Mail doesn't have yet.
    pub missing: Vec<String>,
}

/// A message's HTML with the pictures sent inside it that Mail has. The
/// worker and the window's own fallback both call this, so both give the
/// same page.
pub(super) fn build(
    store: &Store,
    account_id: i64,
    message_id: &str,
) -> anyhow::Result<Option<Built>> {
    let Some(html) = store.message_html(account_id, message_id)? else {
        return Ok(None);
    };
    let wanted = inline::content_ids(&html);
    let images = if wanted.is_empty() {
        Vec::new()
    } else {
        store.inline_images(account_id, message_id)?
    };
    let missing = wanted
        .into_iter()
        .filter(|id| !images.iter().any(|image| image.content_id == *id))
        .collect();
    Ok(Some(Built {
        html: Arc::from(inline::embed(&html, &images).as_ref()),
        inline: images.len(),
        missing,
    }))
}

/// One original to build, tagged so a late answer can be told apart.
pub(super) struct Request {
    pub generation: u64,
    pub account_id: i64,
    pub message_id: String,
}

/// The answer to a [`Request`]. An error is already worded for a notice.
pub(super) struct Loaded {
    pub generation: u64,
    pub account_id: i64,
    pub message_id: String,
    pub built: Result<Option<Built>, String>,
}

/// The thread that builds originals, and the way to talk to it. Dropping
/// it ends the thread.
pub(super) struct Loader {
    requests: Sender<Request>,
    pub results: Receiver<Loaded>,
    /// Keeps a test's request channel open with nothing reading it.
    #[cfg(test)]
    _held: Option<Receiver<Request>>,
}

impl Loader {
    /// `store` is a read-only connection opened by the caller; it moves to a
    /// thread named "originals". `wake` runs after each result, so the
    /// window draws it.
    pub fn start(store: Store, wake: impl Fn() + Send + 'static) -> anyhow::Result<Self> {
        Self::spawn(store, wake, || {})
    }

    /// Like [`Loader::start`], with `before_send` run after each build and
    /// before its result goes back. Tests use it to hold a result.
    fn spawn(
        store: Store,
        wake: impl Fn() + Send + 'static,
        mut before_send: impl FnMut() + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (outbox, results) = mpsc::channel();
        thread::Builder::new()
            .name("originals".into())
            .spawn(move || {
                while let Ok(mut request) = inbox.recv() {
                    // Only the newest matters: a held j has moved past the rest.
                    while let Ok(newer) = inbox.try_recv() {
                        request = newer;
                    }
                    let built = catch_unwind(AssertUnwindSafe(|| {
                        build(&store, request.account_id, &request.message_id)
                    }))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Mail could not read it")))
                    .map_err(|error| format!("{error:#}"));
                    before_send();
                    let loaded = Loaded {
                        generation: request.generation,
                        account_id: request.account_id,
                        message_id: request.message_id,
                        built,
                    };
                    if outbox.send(loaded).is_err() {
                        return;
                    }
                    wake();
                }
            })?;
        Ok(Self {
            requests,
            results,
            #[cfg(test)]
            _held: None,
        })
    }

    /// Asks for an original. Fails when the thread has gone.
    pub fn send(&self, request: Request) -> Result<(), ()> {
        self.requests.send(request).map_err(|_| ())
    }

    /// A loader whose thread is gone before it was asked anything.
    #[cfg(test)]
    pub fn dead() -> Self {
        let (requests, _) = mpsc::channel();
        let (_, results) = mpsc::channel();
        Self {
            requests,
            results,
            _held: None,
        }
    }

    /// A loader that takes a request, then goes without answering.
    #[cfg(test)]
    pub fn dies_after_asking() -> Self {
        let (requests, held) = mpsc::channel();
        let (_, results) = mpsc::channel();
        Self {
            requests,
            results,
            _held: Some(held),
        }
    }

    /// A loader that holds each result until told to let it go. `built`
    /// hears of each build as it finishes; each message on `release` sends
    /// one result.
    #[cfg(test)]
    pub fn gated(store: Store) -> (Self, Receiver<()>, Sender<()>) {
        let (reached, built) = mpsc::channel();
        let (release, gate) = mpsc::channel::<()>();
        let loader = Self::spawn(
            store,
            || {},
            move || {
                let _ = reached.send(());
                let _ = gate.recv();
            },
        )
        .unwrap();
        (loader, built, release)
    }
}

/// How many originals are kept, and how many bytes of HTML at most.
const ENTRIES: usize = 8;
const BYTES: usize = 32 << 20;

/// The last few originals built, newest first, so going back to a message
/// shows it at once.
pub(super) struct Cache {
    entries: VecDeque<((i64, String), Built)>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for Cache {
    fn default() -> Self {
        Self::with_limits(ENTRIES, BYTES)
    }
}

impl Cache {
    fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    fn position(&self, account_id: i64, message_id: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|((account, id), _)| *account == account_id && id == message_id)
    }

    /// The original kept for a message, which becomes the newest.
    pub fn get(&mut self, account_id: i64, message_id: &str) -> Option<Built> {
        let at = self.position(account_id, message_id)?;
        let entry = self.entries.remove(at)?;
        let built = entry.1.clone();
        self.entries.push_front(entry);
        Some(built)
    }

    /// Keeps an original, dropping the least recently used beyond the
    /// limits. The one just kept always stays, however large.
    pub fn insert(&mut self, account_id: i64, message_id: &str, built: Built) {
        self.remove(account_id, message_id);
        self.bytes += built.html.len();
        self.entries
            .push_front(((account_id, message_id.to_owned()), built));
        while self.entries.len() > 1
            && (self.entries.len() > self.max_entries || self.bytes > self.max_bytes)
        {
            if let Some((_, old)) = self.entries.pop_back() {
                self.bytes -= old.html.len();
            }
        }
    }

    pub fn remove(&mut self, account_id: i64, message_id: &str) {
        if let Some(at) = self.position(account_id, message_id)
            && let Some((_, old)) = self.entries.remove(at)
        {
            self.bytes -= old.html.len();
        }
    }

    #[cfg(test)]
    pub fn contains(&self, account_id: i64, message_id: &str) -> bool {
        self.position(account_id, message_id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(bytes: usize) -> Built {
        Built {
            html: Arc::from("x".repeat(bytes).as_str()),
            inline: 0,
            missing: Vec::new(),
        }
    }

    fn ids(cache: &Cache) -> Vec<&str> {
        cache
            .entries
            .iter()
            .map(|((_, id), _)| id.as_str())
            .collect()
    }

    #[test]
    fn the_limits_are_eight_originals_and_32_mib() {
        let cache = Cache::default();
        assert_eq!((cache.max_entries, cache.max_bytes), (8, 32 << 20));
    }

    #[test]
    fn getting_an_original_makes_it_the_newest() {
        let mut cache = Cache::default();
        for id in ["a", "b", "c"] {
            cache.insert(1, id, page(1));
        }
        assert_eq!(ids(&cache), ["c", "b", "a"]);
        assert_eq!(cache.get(1, "a"), Some(page(1)));
        assert_eq!(ids(&cache), ["a", "c", "b"]);
        assert_eq!(cache.get(2, "a"), None, "another account's message");
    }

    #[test]
    fn more_than_eight_drops_the_least_recently_used() {
        let mut cache = Cache::default();
        for index in 0..8 {
            cache.insert(1, &index.to_string(), page(1));
        }
        cache.get(1, "0");
        cache.insert(1, "8", page(1));
        assert_eq!(cache.entries.len(), 8);
        assert!(cache.contains(1, "0"), "used recently, so kept");
        assert!(!cache.contains(1, "1"), "the least recently used goes");
        assert_eq!(cache.bytes, 8);
    }

    #[test]
    fn too_many_bytes_drops_older_but_keeps_an_oversized_newest() {
        let mut cache = Cache::with_limits(8, 100);
        cache.insert(1, "a", page(40));
        cache.insert(1, "b", page(40));
        cache.insert(1, "c", page(40));
        assert_eq!(ids(&cache), ["c", "b"]);
        assert_eq!(cache.bytes, 80);

        cache.insert(1, "huge", page(500));
        assert_eq!(ids(&cache), ["huge"]);
        assert_eq!(cache.bytes, 500);
    }

    #[test]
    fn replacing_or_removing_an_original_gives_its_bytes_back() {
        let mut cache = Cache::default();
        cache.insert(1, "a", page(10));
        cache.insert(1, "a", page(30));
        assert_eq!((cache.entries.len(), cache.bytes), (1, 30));
        cache.insert(1, "b", page(5));
        cache.remove(1, "a");
        assert!(!cache.contains(1, "a"));
        assert_eq!(cache.bytes, 5);
        cache.remove(1, "missing");
        assert_eq!(cache.bytes, 5);
    }
}
