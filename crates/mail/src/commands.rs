//! The commands people type. Output is plain text; agents use `mail agent`.

use std::{
    collections::HashMap,
    fs,
    io::{IsTerminal, Write},
    path::Path,
    process,
    time::Instant,
};

use anyhow::Context;
use clap::Args;
use mail_core::{
    auth::{self, OAuthClient, keychain},
    gmail::{self, GmailApi, GmailClient},
    model::{Account, Label},
    paths::Paths,
    store::{Order, SearchQuery, Store},
    sync::{self, Progress, SyncKind, SyncOptions},
};

use crate::dates;

pub fn auth(paths: &Paths, client_file: Option<&Path>) -> anyhow::Result<()> {
    let client = match client_file {
        Some(file) => {
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
    let sign_in = auth::sign_in(&gmail::http_agent(), &client, |url| {
        let opened = process::Command::new("open").arg(url).status();
        if !opened.is_ok_and(|status| status.success()) {
            println!("Open this link to sign in:\n{url}");
        }
        Ok(())
    })?;

    let gmail = GmailClient::new(client, sign_in.refresh_token.clone(), Some(sign_in.access));
    let profile = gmail.profile()?;
    Store::open(&paths.database())?.add_account(&profile.email_address)?;
    keychain::save(&profile.email_address, &sign_in.refresh_token)?;
    println!(
        "Signed in as {}. Run `mail sync` to download your mail.",
        profile.email_address
    );
    Ok(())
}

pub fn sync(paths: &Paths, days: u32) -> anyhow::Result<()> {
    let mut store = Store::open(&paths.database())?;
    let account = signed_in(&store)?;
    let gmail = GmailClient::new(
        OAuthClient::load(&paths.oauth_client())?,
        keychain::load(&account.email)?,
        None,
    );

    let started = Instant::now();
    let show_progress = std::io::stderr().is_terminal();
    let options = SyncOptions {
        days,
        ..SyncOptions::default()
    };
    let report = sync::sync(&mut store, &gmail, &account, &options, &mut |progress| {
        if show_progress {
            let line = match progress {
                Progress::Listing { found } => format!("Listing messages… {found}"),
                Progress::Downloading { done, total } => format!("Downloading {done} of {total}"),
            };
            eprint!("\r\x1b[2K{line}");
            let _ = std::io::stderr().flush();
        }
    })?;
    if show_progress {
        eprint!("\r\x1b[2K");
    }

    let seconds = started.elapsed().as_secs_f64();
    match report.kind {
        SyncKind::Full => println!(
            "Synced the last {days} days in {seconds:.1}s: {} messages downloaded, {} removed.",
            report.downloaded, report.deleted
        ),
        SyncKind::Incremental => println!(
            "Up to date in {seconds:.1}s: {} new, {} relabeled, {} removed.",
            report.downloaded, report.relabeled, report.deleted
        ),
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
    let store = Store::open_read_only(&paths.database())?;
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
    let store = Store::open_read_only(&paths.database())?;
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
    let store = Store::open_read_only(&paths.database())?;
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
    let store = Store::open_read_only(&database)?;
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
