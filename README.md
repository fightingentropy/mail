# Mail

A fast native Gmail client for Apple Silicon Macs, built in public.

Mail keeps a local copy of your recent Gmail in SQLite. Reading and searching
work offline and take milliseconds; the network is only used to sync. The same
binary has a command line and a one-shot JSON CLI that coding agents such as
Claude Code and Codex can use to search your mail. Agents can't send anything.

> **Status: week one.** Sign-in, sync and offline search work from the
> command line. The keyboard-driven window is next. See the
> [milestones](docs/design.md#milestones).

## Why

Email is the one app I use all day that I haven't replaced with something
faster. I want Gmail's data with a native app's speed: instant search,
triage from the keyboard, and no browser tab.

## Quick start

You need a Mac with Apple Silicon and Rust (`rustup` picks the pinned
toolchain automatically).

```bash
git clone https://github.com/fightingentropy/mail && cd mail
cargo install --locked --path crates/mail
```

`~/.cargo/bin/mail` takes the place of macOS's old `/usr/bin/mail` when
`~/.cargo/bin` comes first in your `PATH`, which is rustup's default.

Then connect your Gmail. This uses your own Google Cloud OAuth client; the
[setup guide](docs/google-setup.md) takes about ten minutes.

```bash
mail auth --client ~/Downloads/client_secret_XXXX.json   # sign in in the browser
mail sync                                                 # first run: last 90 days
mail search invoice --label inbox --since 2026-10-01
mail thread 19a2b3c4d5e6f708
mail status
```

Later runs of `mail sync` only fetch what changed.

## For agents

```bash
mail agent tools
mail agent call search '{"query": "flight lisbon", "since": "2026-09-01"}'
mail agent call read_thread '{"thread_id": "19a2b3c4d5e6f708"}'
```

Every call opens the database read-only, prints one JSON object and exits.
There is no MCP server, daemon or open port, and no agent tool uses the
network or can change or send mail. The tool descriptions tell agents that
email content is untrusted input, never instructions.

## How it works

```
mail (CLI, agent CLI, window)  →  mail-core: auth · gmail · sync · store
                                               │        │       │
                                    OAuth + PKCE    Gmail API    SQLite + FTS5
```

- **Gmail API with OAuth.** PKCE and a loopback redirect, with the refresh
  token in the macOS Keychain. Mail never sees your password.
- **Sync.** The first run downloads the last 90 days on parallel workers.
  After that, Gmail's History API supplies exact changes, usually in a few
  requests.
- **Store.** One SQLite file in WAL mode. Messages, labels and attachment
  metadata are stored; attachments download only when opened.
- **Search.** An FTS5 index over subject, sender, recipients and body,
  ranked with bm25. Words match as you type.

The [design doc](docs/design.md) explains the decisions, including why
OAuth beats an IMAP app password here.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

`mail-core` holds the engine and has no UI code. Its sync logic is tested
against an in-memory fake of the Gmail API, so tests never touch a real
mailbox. Test data uses `example.com` addresses only.

## Privacy

Your mail stays on your Mac and with Google. Mail has no server, no
analytics and no telemetry. The database lives in
`~/Library/Application Support/org.erlin.mail/`; delete that folder and the
Keychain item (`security delete-generic-password -s org.erlin.mail`) to
remove everything.

## Credits

Mail's shape (a Rust engine with SQLite FTS5 search and a read-only agent
CLI) follows my [Whatsapp](https://github.com/fightingentropy/Whatsapp)
client, which extends Carmine Paolino's MIT-licensed
[ZapFast](https://github.com/crmne/zapfast). Mail reuses patterns, not code.

Built on [SQLite](https://sqlite.org) via
[rusqlite](https://github.com/rusqlite/rusqlite),
[ureq](https://github.com/algesten/ureq),
[serde](https://serde.rs), [clap](https://github.com/clap-rs/clap),
[jiff](https://github.com/BurntSushi/jiff),
[encoding_rs](https://github.com/hsivonen/encoding_rs) and
[security-framework](https://github.com/kornelski/rust-security-framework).

Mail is not affiliated with Google. Gmail is a trademark of Google LLC.

## License

[MIT](LICENSE)
