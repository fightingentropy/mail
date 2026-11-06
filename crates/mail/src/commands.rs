//! The commands people type. Output is plain text; agents use `mail agent`.

use std::{
    collections::HashMap,
    fs,
    io::{IsTerminal, Write},
    path::Path,
    process,
    time::{Duration, Instant},
};

use anyhow::Context;
use clap::Args;
use mail_core::{
    auth::OAuthClient,
    compose::{self, Draft},
    model::{Account, Label},
    paths::Paths,
    store::{Order, SearchQuery, Store},
    sync::{self, Progress, SyncKind, SyncOptions},
};

use crate::{accounts, dates};

pub fn auth(paths: &Paths, client_file: Option<&Path>) -> anyhow::Result<()> {
    let client = match client_file {
        Some(file) => {
            if !file.exists() {
                anyhow::bail!(
                    "No file at {}. Create a Desktop app OAuth client in Google Cloud and download \
                     its JSON first: https://github.com/fightingentropy/mail/blob/main/docs/google-setup.md",
                    file.display()
                );
            }
            let text = fs::read_to_string(file)
                .with_context(|| format!("Could not read {}", file.display()))?;
            let client = OAuthClient::from_google_json(&text)?;
            client.save(&paths.oauth_client())?;
            println!(
                "Saved the OAuth client. You can delete {} now.",
                file.display()
            );
            client
        }
        None => OAuthClient::load(&paths.oauth_client())?,
    };

    println!(
        "Opening Google sign-in in your browser. Waiting for you to finish there (Ctrl-C to cancel)…"
    );
    let email = accounts::sign_in(paths, client, |url| {
        let opened = process::Command::new("open").arg(url).status();
        if !opened.is_ok_and(|status| status.success()) {
            println!("Open this link to sign in:\n{url}");
        }
        Ok(())
    })?;
    println!("Signed in as {email}. Run `mail sync` to download your mail.");
    Ok(())
}

/// Saves an image as an account's photo. An open window shows it after it
/// is opened again.
pub fn photo(paths: &Paths, file: &Path, account: Option<&str>) -> anyhow::Result<()> {
    let store = Store::open(&paths.database())?;
    let email = match account {
        Some(email) => {
            store
                .accounts()?
                .into_iter()
                .find(|stored| stored.email.eq_ignore_ascii_case(email))
                .with_context(|| format!("{email} is not signed in. Run `mail auth` to add it."))?
                .email
        }
        None => signed_in(&store)?.email,
    };
    accounts::choose_photo(paths, &email, file)?;
    println!("Set the photo for {email}.");
    Ok(())
}

/// Syncs every signed-in account, one after another.
pub fn sync(paths: &Paths, days: u32) -> anyhow::Result<()> {
    let mut store = Store::open(&paths.database())?;
    let accounts = store.accounts()?;
    if accounts.is_empty() {
        signed_in(&store)?;
    }
    for account in &accounts {
        if accounts.len() > 1 {
            println!("{}", account.email);
        }
        sync_account(paths, &mut store, account, days)?;
    }
    Ok(())
}

fn sync_account(
    paths: &Paths,
    store: &mut Store,
    account: &Account,
    days: u32,
) -> anyhow::Result<()> {
    let gmail = accounts::client(paths, &account.email)?;
    let started = Instant::now();
    let show_progress = std::io::stderr().is_terminal();
    let options = SyncOptions {
        days,
        ..SyncOptions::default()
    };
    let mut downloading_since: Option<Instant> = None;
    let mut filling_since: Option<Instant> = None;
    let mut show = |progress: Progress| {
        if !show_progress {
            return;
        }
        let line = match progress {
            Progress::Listing { found } => format!("Listing messages… {found}"),
            Progress::Checking { done, total } => format!("Checking labels {done} of {total}"),
            Progress::Downloading { done, total } => {
                let since = *downloading_since.get_or_insert_with(Instant::now);
                format!(
                    "Downloading {done} of {total}{}",
                    time_left(since, done, total)
                )
            }
            Progress::Filling { done, total } => {
                let since = *filling_since.get_or_insert_with(Instant::now);
                format!(
                    "Filling in message bodies {done} of {total}{}",
                    time_left(since, done, total)
                )
            }
        };
        eprint!("\r\x1b[2K{line}");
        let _ = std::io::stderr().flush();
    };

    let report = sync::sync(store, &*gmail, account, &options, &mut show)?;
    // A big first sync stores headers first; keep going until every body is
    // here. Each round is short, and Ctrl-C leaves the rest for next time.
    let mut filled = report.bodies_filled;
    let mut pending = report.bodies_pending;
    while pending > 0 {
        // Read the account again: the sync point moved.
        let account = store
            .accounts()?
            .into_iter()
            .find(|stored| stored.id == account.id)
            .context("The account went away during sync")?;
        let next = sync::sync(store, &*gmail, &account, &options, &mut show)?;
        filled += next.bodies_filled;
        pending = next.bodies_pending;
        if next.bodies_filled == 0 {
            break;
        }
    }
    if show_progress {
        eprint!("\r\x1b[2K");
    }

    let seconds = started.elapsed().as_secs_f64();
    match report.kind {
        SyncKind::Full if report.relabeled > 0 => println!(
            "Synced the last {days} days in {seconds:.1}s: {} messages downloaded, {} labels checked, {} removed.",
            report.downloaded, report.relabeled, report.deleted
        ),
        SyncKind::Full => println!(
            "Synced the last {days} days in {seconds:.1}s: {} messages downloaded, {} removed.",
            report.downloaded, report.deleted
        ),
        SyncKind::Incremental => println!(
            "Up to date in {seconds:.1}s: {} new, {} relabeled, {} removed.",
            report.downloaded, report.relabeled, report.deleted
        ),
    }
    if filled > 0 {
        println!("Filled in {filled} message bodies.");
    }
    if pending > 0 {
        println!("{pending} message bodies are still to come; run `mail sync` again.");
    }
    Ok(())
}

#[derive(Args)]
pub struct SearchArgs {
    /// Words to find. Each also matches as a prefix; all must match.
    words: Vec<String>,
    /// Only mail from this sender: part of the name or address.
    #[arg(long)]
    from: Option<String>,
    /// Only mail with this label, by name or ID: inbox, unread, Travel, Label_42.
    #[arg(long)]
    label: Option<String>,
    /// Received on or after this day (YYYY-MM-DD).
    #[arg(long, value_name = "DATE")]
    since: Option<String>,
    /// Received before this day (YYYY-MM-DD).
    #[arg(long, value_name = "DATE")]
    before: Option<String>,
    /// Newest first instead of best match first.
    #[arg(long)]
    newest: bool,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=500))]
    limit: u32,
}

pub fn search(paths: &Paths, args: &SearchArgs) -> anyhow::Result<()> {
    let store = Store::open_existing(&paths.database())?;
    let account = signed_in(&store)?;
    let query = SearchQuery {
        text: args.words.join(" "),
        from: args.from.clone(),
        label_id: label_id(&store, &account, args.label.as_deref())?,
        since: args.since.as_deref().map(dates::day_start).transpose()?,
        before: args.before.as_deref().map(dates::day_start).transpose()?,
        order: if args.newest {
            Order::Newest
        } else {
            Order::Relevance
        },
        limit: args.limit,
        offset: 0,
    };

    let started = Instant::now();
    let hits = store.search(account.id, &query)?;
    let elapsed = started.elapsed();
    let names = label_names(&store.labels(account.id)?);

    for hit in &hits {
        let sender = if hit.from_name.is_empty() {
            &hit.from_addr
        } else {
            &hit.from_name
        };
        println!(
            "{sender} · {}",
            or_placeholder(&hit.subject, "(no subject)")
        );
        println!(
            "  {} · thread {}{}",
            dates::human(hit.internal_date),
            hit.thread_id,
            shown_labels(&hit.label_ids, &names)
        );
        if !hit.excerpt.trim().is_empty() {
            println!("  {}", one_line(&hit.excerpt));
        }
        println!();
    }
    let plural = if hits.len() == 1 { "" } else { "es" };
    eprintln!(
        "{} match{plural} in {:.1} ms",
        hits.len(),
        elapsed.as_secs_f64() * 1000.0
    );
    Ok(())
}

pub fn thread(paths: &Paths, thread_id: &str) -> anyhow::Result<()> {
    let store = Store::open_existing(&paths.database())?;
    let account = signed_in(&store)?;
    let messages = store.thread(account.id, thread_id)?;
    anyhow::ensure!(
        !messages.is_empty(),
        "No thread {thread_id} in downloaded mail."
    );
    let names = label_names(&store.labels(account.id)?);

    let count = messages.len();
    println!(
        "{}  ({count} message{})\n",
        or_placeholder(&messages[0].subject, "(no subject)"),
        if count == 1 { "" } else { "s" }
    );
    for (index, message) in messages.iter().enumerate() {
        let from = if message.from_name.is_empty() {
            message.from_addr.clone()
        } else {
            format!("{} <{}>", message.from_name, message.from_addr)
        };
        println!("From  {from}");
        println!("To    {}", message.to);
        if !message.cc.is_empty() {
            println!("Cc    {}", message.cc);
        }
        println!(
            "Date  {}{}",
            dates::human(message.internal_date),
            shown_labels(&message.label_ids, &names)
        );
        for attachment in &message.attachments {
            println!("File  {} ({})", attachment.filename, size(attachment.size));
        }
        println!("\n{}", message.body_text);
        if index + 1 < count {
            println!("\n{}\n", "─".repeat(60));
        }
    }
    Ok(())
}

pub fn labels(paths: &Paths) -> anyhow::Result<()> {
    let store = Store::open_existing(&paths.database())?;
    let account = signed_in(&store)?;
    for label in store.labels(account.id)? {
        if label.name == label.id {
            println!("{}", label.name);
        } else {
            println!("{}  ({})", label.name, label.id);
        }
    }
    Ok(())
}

pub fn status(paths: &Paths) -> anyhow::Result<()> {
    let database = paths.database();
    let store = Store::open_existing(&database)?;
    let account = signed_in(&store)?;
    let stats = store.stats(account.id)?;

    println!("Account   {}", account.email);
    match account.synced_at {
        Some(synced_at) => println!("Synced    {}", dates::human(synced_at)),
        None => println!("Synced    not yet. Run `mail sync`."),
    }
    let range = match (stats.oldest, stats.newest) {
        (Some(oldest), Some(newest)) => {
            format!(", {} to {}", dates::day(oldest), dates::day(newest))
        }
        _ => String::new(),
    };
    println!(
        "Stored    {} messages in {} threads{range}",
        stats.messages, stats.threads
    );
    let bytes = fs::metadata(&database).map(|meta| meta.len()).unwrap_or(0);
    println!("Database  {} ({})", database.display(), size(bytes as i64));
    Ok(())
}

#[derive(Args)]
pub struct DraftArgs {
    /// Recipients, comma-separated. For a reply, defaults to the sender.
    #[arg(long)]
    to: Option<String>,
    /// Copy recipients, comma-separated.
    #[arg(long)]
    cc: Option<String>,
    /// For a reply, defaults to "Re: " and the original subject.
    #[arg(long)]
    subject: Option<String>,
    /// The text. Read from standard input when left out.
    #[arg(long)]
    body: Option<String>,
    /// Reply to this message or thread ID (from `mail search`), quoting it.
    #[arg(long, value_name = "ID")]
    reply: Option<String>,
    /// With --reply: copy everyone on the original.
    #[arg(long, requires = "reply")]
    all: bool,
}

pub fn draft(paths: &Paths, args: DraftArgs) -> anyhow::Result<()> {
    let body = match args.body {
        Some(body) => body,
        None => std::io::read_to_string(std::io::stdin())
            .context("Could not read the text from standard input")?,
    };
    let store = Store::open_existing(&paths.database())?;
    let account = signed_in(&store)?;
    let request = DraftRequest {
        to: args.to,
        cc: args.cc,
        subject: args.subject,
        body,
        reply_to: args.reply,
        reply_all: args.all,
    };
    let draft = build_draft(&store, &account, request)?;
    let draft_id = save_draft(paths, &account, &draft)?;
    println!(
        "Saved to Drafts ({draft_id}) for {}. Send it from Mail's window or Gmail.",
        if draft.to.is_empty() {
            "no recipients yet"
        } else {
            &draft.to
        }
    );
    Ok(())
}

/// What the `draft` command and the agent's `create_draft` tool ask for.
pub(crate) struct DraftRequest {
    pub to: Option<String>,
    pub cc: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    /// A message or thread ID to reply to.
    pub reply_to: Option<String>,
    pub reply_all: bool,
}

/// Turns a request into a draft: a threaded reply with the original quoted
/// below the text, or a new message.
pub(crate) fn build_draft(
    store: &Store,
    account: &Account,
    request: DraftRequest,
) -> anyhow::Result<Draft> {
    let non_empty = |value: Option<String>| value.filter(|text| !text.trim().is_empty());
    let Some(id) = request.reply_to else {
        return Ok(Draft {
            to: non_empty(request.to).context("Say who the message is to.")?,
            cc: request.cc.unwrap_or_default(),
            subject: request.subject.unwrap_or_default(),
            body: request.body,
            reply: None,
        });
    };
    let original = match store.message(account.id, &id)? {
        Some(message) => message,
        None => store
            .thread(account.id, &id)?
            .into_iter()
            .rev()
            .find(|message| !message.has_label("DRAFT"))
            .with_context(|| format!("No message or thread {id} in downloaded mail."))?,
    };
    let mut draft = Draft::reply(
        &original,
        &account.email,
        request.reply_all,
        &dates::human(original.internal_date),
    );
    draft.body = format!("{}{}", request.body.trim_end(), draft.body);
    if let Some(to) = non_empty(request.to) {
        draft.to = to;
    }
    if let Some(cc) = request.cc {
        draft.cc = cc;
    }
    if let Some(subject) = non_empty(request.subject) {
        draft.subject = subject;
    }
    Ok(draft)
}

/// Saves a draft to Gmail's Drafts. This is the network step; nothing here
/// sends mail.
pub(crate) fn save_draft(
    paths: &Paths,
    account: &Account,
    draft: &Draft,
) -> anyhow::Result<String> {
    let gmail = accounts::client(paths, &account.email)?;
    compose::save(&*gmail, &account.email, draft, None)
}

pub(crate) fn signed_in(store: &Store) -> anyhow::Result<Account> {
    store
        .account()?
        .context("Not signed in. Run `mail auth --client <file>` first.")
}

/// Resolves a label name such as "inbox" or "Travel" to its Gmail ID.
pub(crate) fn label_id(
    store: &Store,
    account: &Account,
    label: Option<&str>,
) -> anyhow::Result<Option<String>> {
    label
        .map(|name| {
            store
                .resolve_label(account.id, name)?
                .with_context(|| format!("No label named {name:?}. See `mail labels`."))
        })
        .transpose()
}

pub(crate) fn label_names(labels: &[Label]) -> HashMap<String, String> {
    labels
        .iter()
        .map(|label| (label.id.clone(), label.name.clone()))
        .collect()
}

/// ` · INBOX, UNREAD, Travel`, leaving out Gmail's inbox categories.
fn shown_labels(label_ids: &[String], names: &HashMap<String, String>) -> String {
    let shown: Vec<&str> = label_ids
        .iter()
        .filter(|id| !id.starts_with("CATEGORY_"))
        .map(|id| names.get(id).map_or(id.as_str(), String::as_str))
        .collect();
    if shown.is_empty() {
        String::new()
    } else {
        format!(" · {}", shown.join(", "))
    }
}

/// `, about 4 min left`, once enough messages have arrived to estimate. Gmail
/// messages arrive in bursts a minute apart, so the rate means little before
/// the first minute is over, and is rough after it.
fn time_left(since: Instant, done: usize, total: usize) -> String {
    if since.elapsed() < Duration::from_secs(60) || done < 10 || done >= total {
        return String::new();
    }
    let per_message = since.elapsed().as_secs_f64() / done as f64;
    let seconds = per_message * (total - done) as f64;
    if seconds < 90.0 {
        format!(", about {} s left", seconds.ceil() as u64)
    } else {
        format!(", about {} min left", (seconds / 60.0).round() as u64)
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn or_placeholder<'a>(text: &'a str, placeholder: &'a str) -> &'a str {
    if text.trim().is_empty() {
        placeholder
    } else {
        text
    }
}

fn size(bytes: i64) -> String {
    if bytes < 1_000 {
        format!("{bytes} B")
    } else if bytes < 1_000_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}
