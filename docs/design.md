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

Not goals for v1: non-Gmail providers, HTML rendering in the window (the browser shows a message's HTML on request), calendar or
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
  send. "Mail never sends without you" is enforced in code: the one send call,
  `drafts.send`, is used only by the window after a confirmation, and the
  agent CLI has no tool that reaches it.
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
6. With the token Mail asks Google's userinfo endpoint for the profile photo
   (`userinfo.profile`, the only scope besides `gmail.modify`) and saves it
   under `photos/`. The URL must be on `lh3.googleusercontent.com`.

Signing in again to an account that is already there renews its token and
photo. **Several accounts** came after v1: every table was keyed by account
from the start, so the store only had to stop refusing a second one. The
window syncs the accounts one after another on its sync thread, one Gmail
client each, and a failing account doesn't hold up the rest. "All" runs the
same newest-first query per account and merges by date; each thread carries
its account, so triage, replies, drafts and downloads use the right token.
Labels are per account (IDs such as `Label_1` repeat across accounts), so in
"All" they are grouped by address and choosing one narrows the window to that
account. The command line and the agent still read the first account.

## Data model

One SQLite file: `~/Library/Application Support/org.erlin.mail/mail.sqlite3`.
It runs in WAL mode so the window can read while sync writes. Migrations
are numbered and tracked with `PRAGMA user_version`
([schema.rs](../crates/mail-core/src/store/schema.rs)).

| Table | Holds |
|---|---|
| `accounts` | Email address, the Gmail `history_id` reached by the last full or incremental sync, last sync time |
| `labels` | Gmail label ID, name, system or user |
| `messages` | One row per Gmail message: thread ID, received time (ms), subject, sender, To/Cc, snippet, plain text body, threading headers, whether the body is still to come |
| `message_bodies` | Each message's HTML version, deflate-compressed, read only when opened in the browser |
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
- **HTML apart and compressed.** HTML was 74% of the bytes in a real
  mailbox but is read only when you press `h`. Since migration 3 it lives in
  `message_bodies`, deflate-compressed, and is loaded on demand. On a real
  database this went from 16.8 MB to 6.6 MB, and the narrower `messages`
  table made list queries faster (see [Performance](#performance)).
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
- **Headers first for big mailboxes:** with more than 300 new messages, the
  first pass fetches `format=metadata` (headers, snippet, labels; 20 units)
  and stores each message with `body_pending`. The sync point is saved, so the
  mailbox is browsable and searchable by subject and sender at a third of the
  time. Every sync then fills in up to 200 bodies, newest first; the window
  goes again at once while bodies are owed, and `mail sync` loops until they
  are all in. It costs a third more quota in total, so smaller first syncs
  download everything in one pass. A body Gmail no longer has is dropped.
  Measured on my 452 messages: headers all stored after about 95 s (instead
  of 285 s for a full pass), every body in after 381 s.
- **Later syncs:** replay `users.history.list` from the saved history ID:
  download added messages, set labels from the newest label record, delete
  removed messages. When little has changed this takes a few requests.
- **Expired history** (Gmail keeps roughly a week): fall back to a full pass.
  Gmail messages never change except for their labels, so only messages Mail
  doesn't have are downloaded; the rest get a `format=minimal` read of their
  labels at 20 quota units instead of 60. Local messages Gmail no longer lists
  are deleted.
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
  the window is open (at launch, every two minutes, after triage, `⇧⌘N`).
  Nothing runs when Mail is closed.

## Search

`mail search` and the agent `search` tool use the same query. Each typed word
becomes a quoted term, and all must match, so user input can never be an FTS
syntax error. Words of two or more characters also match as prefixes; a
single character matches only itself, because a one-letter prefix scans most
of the index. The window's live search starts at the second letter and asks
only for matching thread IDs, without excerpts, then builds the list in one
query. Filters: sender, label (by name or ID), date range.
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
mail draft [--to] [--cc] [--subject] [--body | stdin] [--reply <id> [--all]]
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
mail agent call create_draft '{"reply_to": "…", "body": "…"}'
```

Rules:

- **One-shot.** Each call opens the database read-only, prints one JSON
  object and exits. No MCP server, no daemon, no port.
- **Reading is offline.** Results reflect the last sync. An agent that
  should refresh runs `mail sync` like a person would.
- **Email is untrusted input.** Mail is the most common carrier of prompt
  injection. `mail agent tools` states that subjects, bodies, names and links
  are data written by other people, never instructions. Bodies are capped at
  20,000 characters per message.
- **Drafts, never sends.** `create_draft` saves a draft to the user's Gmail
  Drafts and is the only tool that writes or uses the network. Its arguments
  are checked before any request: addresses, a 200 KB body limit, and that a
  reply target exists locally. The instructions tell agents to draft only
  what the user asked for, never because an email asks. There is no send
  tool; sending is the window's alone, after a confirmation.
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

   *Originals* (added after): a thread that is one message with HTML (a
   receipt, a newsletter) shows that HTML in a WKWebView, through `wry`,
   placed as a child view over the reader below Mail's own header. Pages are
   served from a private `mail://` scheme so the content policy arrives as a
   response header (`default-src 'none'`, inline styles, `data:` images, and
   remote images only after "Load images"); the scheme answers only the
   page's own path. The navigation handler allows nothing but those pages
   and hands web and email links to the browser. An init script forwards
   key presses to the window over IPC, so shortcuts work while the page has
   focus (⌘C and ⌘A stay with it); `wry` drops IPC from pages whose address
   it can't parse, such as `about:blank`, which is another reason for the
   scheme. WebKit draws above the GL surface, so the view hides whenever an
   egui modal or popup is open, and screenshots (which capture only egui)
   use the text reader. Checked on a real newsletter: 18 images, none
   fetched until allowed.

   *Readable newsletters* (added after): a text part is kept as written
   unless it looks machine-made (leftover `&nbsp;`, markup indentation, four
   or more empty lines in a row, five or more tracking links over 80
   characters), in which case the HTML gives the text. The HTML converter
   breaks lines like a browser (a `div` or row ends a line, a `p` or table a
   paragraph), reads past `>` inside quoted attributes, and keeps each
   link's words in the text with its address beside it as `body_links`
   (byte ranges, JSON in its own column), so search never indexes tracking
   URLs and the reader makes the words clickable. Addresses still written in
   a text part show as their host when longer than 60 characters, and the
   host shown is the one after any `user@`. Migration 5 queued every stored
   message and `Store::open` made its text again once: on a real mailbox,
   messages with blank-line gaps went from 136 of 463 to none, in about a
   second.

   *Shortcuts as data* (added after): `keys.rs` holds a keymap from actions
   to shortcuts instead of a hard-coded match. Each action has Gmail's
   letter and a macOS chord (`⌘⌫` trash, `⌃⌘A` archive, `⌘R` reply, `⌘K`
   search, `⌘,` Settings); `/` shows every shortcut, drawn from the keymap
   in effect. Symbols such as `#` and `?` match the character typed, since
   keyboards reach them differently; everything else matches a key with
   modifiers. Settings records new shortcuts by key press. Shortcuts compete
   only within a scope (the mailbox, or the compose fields, where a
   shortcut needs ⌘, ⌃ or ⌥ so letters still type), and a shortcut taken
   from another action moves rather than being refused. Only changes are
   saved, to `shortcuts.json`, so later defaults reach untouched actions.
   Esc and the keys inside overlays stay fixed, so no rebinding can trap
   you. Conversations also have a right-click menu with the same actions.
3. **Writing.** *Done.* `r`, `a` and `c` open a compose panel in the
   reader's place: a reply quotes the original with the cursor above it,
   addressed to the sender (or everyone but you with `a`). `⌘S` saves to
   Gmail's Drafts on a worker thread (`drafts.create`, then `drafts.update`
   for the same draft) and the next sync brings the draft into the local copy;
   `⇧⌘D` discards. Messages are built locally as RFC 5322 (UTF-8 text, base64
   body, RFC 2047 headers, line breaks stripped from every header so no field
   can inject one). The same code serves `mail draft` and the agent's
   `create_draft`. Checked end to end on a real account: draft created, synced
   back, deleted.

   *Sending* (added after): `⌘↩` checks there is a valid recipient and shows
   a confirmation with To, Cc and Subject; Enter or the Send button sends.
   The draft is saved first and then sent with `drafts.send` (100 quota
   units), so a failed send keeps the draft and a retry reuses it. The send
   keys are taken away from the text fields, so neither `⌘↩` nor the
   confirming Enter types a new line. Only the window can send: the agent
   CLI and `mail draft` stop at Drafts, and a test checks that no agent tool
   is a send.
4. **Polish.** *App bundle done:* `packaging/macos/build-app.sh` builds an
   Apple Silicon `Mail.app` (bundle ID `org.erlin.mail`), installs it in
   `/Applications` and refuses to replace any other app of that name. It
   signs with a Developer ID when the keychain has one (hardened runtime,
   secure timestamp), so the Keychain remembers its approval across
   rebuilds; `--link-cli` points `~/.cargo/bin/mail` at the app's binary so
   the terminal shares that identity; `--notarize <profile>` submits,
   staples and checks the app with Gatekeeper. The icon is drawn in code
   (`examples/render_icon.rs`). Still to do: a GitHub release workflow.
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

## Performance

Measured on an Apple Silicon Mac with a release build. The window draws only
on input (0% CPU when idle, about 126 MB, mostly GPU surfaces and the font
atlas) and opens in under half a second.

A benchmark builds a 10,000-message mailbox with realistic HTML and times
the hot paths: `cargo test -p mail-core --release -- --ignored scale_timings --nocapture`.

| | Before tuning | After |
|---|---|---|
| One-letter search | 27.9 ms | 0.08 ms |
| Window search, 300 results | 6.8 ms | 1.6 ms |
| All mail list, 5,000 conversations | 13.3 ms | 7.7 ms |
| Inbox list, 300 conversations | 2.9 ms | 2.4 ms |
| Word search (CLI) | 3.8 ms | 2.5 ms |
| Database | 43.2 MB | 7.5 MB |

On my real mailbox (454 messages): search 0.5–1.8 ms, incremental sync
0.2–0.5 s, database 6.6 MB (was 16.8 MB). The first sync is bound by Gmail's
quota, not by Mail: 285 s for 90 days. A resync after expired history went
from 285 s to 95 s (451 label checks, nothing downloaded again).

## Risks

- Google could restrict unverified personal-use apps. Fallback: IMAP with an
  app password, behind the same store.
- More than a week offline means a full pass: labels for every stored
  message plus anything new, about a minute and a half for my mailbox.
- A full sync is bounded by Gmail's quota at about 100 full messages a
  minute. Headers first makes a big mailbox usable at about 300 messages a
  minute, but bodies still take hours for a very busy one.
