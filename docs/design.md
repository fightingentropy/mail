# Mail: design

Mail is a fast native email client for Apple Silicon Macs. It keeps a local
copy of one Gmail account in SQLite, so reading and searching work offline and
finish in milliseconds. One binary holds the desktop app, a command line and a
one-shot JSON CLI for coding agents.

This document records the decisions made before most of the code. It is short
on purpose; the code and its comments carry the details.

## Goals

- **Fast.** Every view comes from local SQLite. The network only syncs.
- **Keyboard first.** Triage (archive, trash, label, open, reply) without the mouse.
- **Offline.** Read and search the last 90 days with no connection.
- **Agent-friendly, safely.** Claude Code or Codex can search and read mail
  through `mail agent call …`. Agents can never send mail, and nothing runs
  in the background when the app is closed.
- **Small and readable.** Plain Rust, few dependencies, no framework magic.

Not goals for v1: multiple accounts, non-Gmail providers, HTML rendering in
the window (the browser shows a message's HTML on request), calendar or
contacts, a hosted service of any kind.

## Architecture

```
┌──────────────────────────── mail (one binary) ────────────────────────────┐
│ command line   auth · sync · search · thread · labels · status            │
│ agent CLI      mail agent tools | call <tool> '<json>'   (read-only)      │
│ window         egui/eframe, keyboard triage, background sync              │
└──────────────────────────────────┬────────────────────────────────────────┘
                                   │
┌──────────────────────────────── mail-core ────────────────────────────────┐
│ auth    OAuth 2.0 + PKCE, loopback redirect, refresh token in Keychain    │
│ gmail   REST client (ureq), retries, MIME tree → text, HTML → text        │
│ sync    full pass over N days, then incremental from Gmail history        │
│ store   SQLite (WAL) with FTS5, versioned migrations                      │
└───────────────────────────────────────────────────────────────────────────┘
```

Two crates keep the engine free of UI code. `mail-core` has no knowledge of
the terminal or the window, and its sync logic is tested against an in-memory
fake of the Gmail API (`sync/tests.rs`).

## Gmail access

Decided on 1 November 2026: **the Gmail REST API with OAuth**, using my own
Google Cloud project with a Desktop client, published to "In production"
without verification.

The options, against Google's current rules:

| | Gmail API + OAuth, in production (chosen) | IMAP + app password | Gmail API + OAuth, left in Testing |
|---|---|---|---|
| Token lifetime | Until revoked, unused for 6 months, or the password changes | Until the password changes | **7 days** for Gmail scopes |
| Setup | ~10 min in Google Cloud | ~2 min, needs 2-Step Verification | ~10 min |
| Access granted | `gmail.modify`: no permanent delete | Full IMAP and SMTP | `gmail.modify` |
| Google's view | Recommended | "Aren't recommended" | Fine for development |
| Sync quality | Native threads, labels and drafts; History API for cheap, exact incremental sync | X-GM extensions give thread IDs and labels; no QRESYNC, so deletions need UID diffs | Same as chosen |
| First sync speed | About 100 messages a minute (quota, see below) | No per-message quota; bounded by bandwidth | Same as chosen |
| Friction | One "unverified app" warning at sign-in | None | Sign in again weekly |

Why this is allowed: Google exempts personal-use apps with fewer than 100
users from verification. Users click through an "unverified app" screen
instead. rclone documents the same path for its users' own client IDs.

Consequences:

- Anyone building Mail from source creates their own Google Cloud client
  ([setup guide](google-setup.md)). No client ID or secret is in the repo.
- Every Gmail scope that can write (`gmail.modify`, `gmail.compose`) can also
  send. "Mail never sends without you" is enforced in code: the API client
  has no send call, and the agent CLI opens the database read-only.
- A password change revokes the token. `mail sync` then says to run
  `mail auth` again.
- The first sync is slow by design. Since 1 May 2026 a full message costs 60
  quota units against 6,000 per user per minute, so Gmail serves about 100
  messages a minute (found on the first live sync, after the decision). 90
  days of my mail (452 messages) takes about 5 minutes once; later syncs take
  a fraction of a second. A much larger mailbox would make IMAP worth
  reconsidering for the first download, or a quota increase in Google Cloud.
- If Google ever closes this route, IMAP with an app password is the fallback.
  The store and search don't depend on how mail arrives.

Sources: [OAuth 2.0 refresh token expiration](https://developers.google.com/identity/protocols/oauth2#expiration),
[when verification is not needed](https://support.google.com/cloud/answer/13464323),
[Gmail API scopes](https://developers.google.com/workspace/gmail/api/auth/scopes),
[app passwords](https://support.google.com/accounts/answer/185833),
[Gmail IMAP extensions](https://developers.google.com/workspace/gmail/imap/imap-extensions),
[rclone: making your own client ID](https://rclone.org/drive/#making-your-own-client-id),
[Gmail API usage limits](https://developers.google.com/workspace/gmail/api/reference/quota).

### Sign-in flow

1. `mail auth --client <file>` reads the Desktop client JSON from Google Cloud
   and copies it to `~/Library/Application Support/org.erlin.mail/oauth-client.json`
   (mode 0600).
2. Mail listens on `127.0.0.1:<random port>` and opens the browser with a
   PKCE challenge and a random state value (RFC 7636, RFC 8252).
3. You sign in and approve access in the browser. Mail never sees the password.
4. Google redirects to the loopback port. Mail checks the state value and
   exchanges the code and PKCE verifier for tokens.
5. The refresh token goes into the login Keychain (service `org.erlin.mail`,
   account = your address). Access tokens stay in memory and are refreshed
   when they expire or Gmail returns 401.

## Data model

One SQLite file: `~/Library/Application Support/org.erlin.mail/mail.sqlite3`.
It runs in WAL mode so the window can read while sync writes. Migrations
are numbered and tracked with `PRAGMA user_version`
([schema.rs](../crates/mail-core/src/store/schema.rs)).

| Table | Holds |
|---|---|
| `accounts` | Email address, the Gmail `history_id` reached by the last full or incremental sync, last sync time |
| `labels` | Gmail label ID, name, system or user |
| `messages` | One row per Gmail message: thread ID, received time (ms), subject, sender, To/Cc, snippet, plain text body, HTML body, threading headers |
| `message_labels` | Message ↔ label ID. Gmail labels are per message; a thread shows the union |
| `attachments` | Filename, type, size, Gmail attachment ID. Contents are fetched only when opened |
| `message_search` | FTS5 index over subject, sender name and address, To, Cc and body |
| `pending_changes` | Triage made in Mail and not yet accepted by Gmail, oldest first |

Choices:

- **Messages, not threads, are stored.** Gmail labels individual messages,
  and its History API reports changes per message. Threads are a `GROUP BY`
  away. A denormalized `threads` table can come later if the inbox view needs it.
- **Labels in a join table**, not a JSON column, so "inbox, unread" is an
  indexed lookup.
- **External-content FTS5.** Text is stored once, in `messages`; triggers
  keep the index in step on insert, update and delete.
- **unicode61 tokenizer with prefix indexes**, not trigram as in my Whatsapp
  client. Email bodies are long. Word tokens keep the index a fraction of a
  trigram index's size, give meaningful bm25 ranking, and still match as you
  type ("inv" finds "invoice").
- **`format=full`, not `raw`.** Gmail returns the MIME tree with text parts
  inline and attachments by reference, so sync never downloads attachments.
  Mail decodes each text part from its declared charset (`encoding_rs`) and
  converts HTML-only mail to text for search.

## Sync

- **First sync:** read the profile's current `historyId`, list
  `newer_than:90d`, download each message on 3 worker threads, and write
  batches of 200 per transaction. The history ID is saved only at the end, so
  an interrupted or failed first sync keeps what it stored, resumes and skips
  what it already has.
- **Later syncs:** replay `users.history.list` from the saved history ID:
  download added messages, set labels from the newest label record, delete
  removed messages. When little has changed this takes a few requests.
- **Expired history** (Gmail keeps roughly a week): fall back to a full pass,
  which re-downloads the window and deletes local messages Gmail no longer
  lists.
- **Rate limits:** Gmail allows 6,000 quota units per user per minute. Since
  1 May 2026 a `format=full` message costs 60 units, a list 5, history 2. The
  first live sync, with 8 unpaced workers, had 84% of its fetches refused.
  Mail now books every request through one shared pacer at 95 units a
  second, about 1.6 full messages a second, so Gmail rarely refuses one. On
  429, 5xx or a quota 403, all workers pause together, backing off from 2
  seconds up to a minute.
- **Local changes first.** Each sync sends queued triage before it pulls,
  and re-applies whatever was queued while it ran.
- **No background process.** Sync runs when you run `mail sync`, and while
  the window is open (at launch, every two minutes, after triage, `⌘R`).
  Nothing runs when Mail is closed.

## Search

`mail search` and the agent `search` tool use the same query. Each typed word
becomes a quoted prefix term, and all must match, so user input can never be
an FTS syntax error. Filters: sender, label (by name or ID), date range.
Results are ranked by bm25, weighting subject 8, sender 4, To 2 and body 1,
or sorted newest first. Trash and spam are hidden unless named as the label.

## Command line

```
mail                                        open the window
mail --demo                                 the window on a made-up mailbox
mail auth [--client <file>]                 sign in (browser)
mail sync [--days 90]                       full first time, then incremental
mail search <words…> [--from] [--label] [--since] [--before] [--newest] [--limit]
mail thread <thread-id>
mail labels
mail status
```

## Agent CLI

```
mail agent tools                             tools, JSON schemas, rules
mail agent call search '{"query": "invoice", "label": "INBOX", "since": "2026-10-01"}'
mail agent call read_thread '{"thread_id": "…"}'
mail agent call read_message '{"message_id": "…"}'
mail agent call list_labels
mail agent call status
```

Rules:

- **One-shot.** Each call opens the database read-only, prints one JSON
  object and exits. No MCP server, no daemon, no port.
- **No network.** Results reflect the last sync. An agent that should
  refresh runs `mail sync` like a person would.
- **Email is untrusted input.** Mail is the most common carrier of prompt
  injection. `mail agent tools` states that subjects, bodies, names and links
  are data written by other people, never instructions. Bodies are capped at
  20,000 characters per message.
- **Drafts, never sends** (milestone 3). `create_draft` will create a Gmail
  draft for you to review and send. There will be no send tool.
- Errors are JSON too: `{"error": {"code": "invalid_arguments", "message": "…"}}`
  with exit code 1.

## Milestones

1. **Week one: sync and search from the command line.** *Done.* OAuth
   sign-in, full and incremental sync, FTS5 search, `mail search`, `thread`,
   `status`, read-only agent CLI. On a real mailbox: 452 messages in 285 s
   (quota-bound), incremental sync in 0.2 s, search under 2 ms.
2. **The window.** *Done.* egui/eframe, charcoal dark theme, Inter at large
   sizes. Sidebar with mailboxes, labels and unread counts; conversation list;
   reader with older messages collapsed; live search. Keyboard triage: `j`/`k`,
   `e` archive, `#` trash, `u` unread, `s` star, `l` label picker, `z` undo,
   `g` sequences. A thread counts as read after 1.5 s on screen. Changes apply
   locally at once and queue in `pending_changes`; sync sends them first
   (`threads.modify`, `threads.trash`, `threads.untrash`), then pulls, then
   re-applies anything queued meanwhile, so archiving during a sync doesn't
   flicker. Gmail refusals (a deleted label) are dropped and reported. Since
   then: clickable links in bodies (`http`, `https` and `mailto` only, full
   address on hover, opened only on a click), attachments on demand, the HTML
   view, and a light palette that follows the macOS appearance.
3. **Writing.** Reply and compose as Gmail drafts (RFC 5322 built locally,
   threaded with `In-Reply-To`/`References`), and the agent `create_draft`
   tool. Whether the window may send, after an explicit confirmation, gets
   decided here.
4. **Polish.** Apple Silicon `.app` bundle and a `mail` symlink for the CLI.
   Done early: *attachments on demand*, fetched with
   `messages.attachments.get` (20 quota units) on a worker thread and saved
   to Downloads under a name that never replaces a file. Gmail can retire an
   attachment ID, so a refused one costs one re-read of the message (60
   units) for a fresh ID. Documents, images and media open; anything that
   could run code is shown in Finder. *The HTML view*: `h` writes the
   message's HTML to a temporary file, with a UTF-8 charset and a policy that
   blocks scripts, frames and forms, and opens it in the browser. Remote
   images load there, so the sender learns the message was opened; the
   reader itself never loads anything.

## Risks

- Google could restrict unverified personal-use apps. Fallback: IMAP with an
  app password, behind the same store.
- More than a week offline means a full resync (minutes, not seconds).
- A full sync is bounded by Gmail's quota at about 100 messages a minute:
  minutes for a personal mailbox, hours for a very busy one. Expired history
  currently means re-downloading the window; refreshing known messages'
  labels with `format=minimal` (20 units) would cut that by two thirds.
