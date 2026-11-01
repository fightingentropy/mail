//! `mail`: a fast, local-first Gmail client for Apple Silicon Macs.
//!
//! `mail` with no command opens the window. The commands sign in, sync and
//! search from the terminal, and `mail agent` serves coding agents.

mod accounts;
mod agent;
mod commands;
mod dates;
mod window;

use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use mail_core::paths::Paths;

#[derive(Parser)]
#[command(
    name = "mail",
    version,
    about = "A fast, local-first Gmail client for your Mac"
)]
struct Cli {
    /// Keep the database and OAuth client in this folder instead of
    /// ~/Library/Application Support/org.erlin.mail.
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,
    /// Open the window on a made-up mailbox, without a Google account.
    #[arg(long)]
    demo: bool,
    /// Save a PNG of the window to this path once it has rendered, then quit.
    #[arg(long, value_name = "FILE", hide = true)]
    screenshot: Option<PathBuf>,
    /// Keys to press in the demo before a screenshot, e.g. "j e cmd+k text:seat".
    #[arg(long, value_name = "KEYS", hide = true, requires = "demo")]
    demo_keys: Option<String>,
    /// Draw the window light or dark instead of following macOS.
    #[arg(long, value_enum, hide = true)]
    appearance: Option<Appearance>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Appearance {
    Light,
    Dark,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in to Gmail in your browser. Pass --client the first time.
    Auth {
        /// The OAuth client JSON downloaded from your Google Cloud project.
        #[arg(long, value_name = "FILE")]
        client: Option<PathBuf>,
    },
    /// Download new mail and changes. The first sync fetches the last 90 days.
    Sync {
        /// How far back a full sync reaches. Later syncs only fetch changes.
        #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u32).range(1..=3650))]
        days: u32,
    },
    /// Search downloaded mail. Works offline.
    Search(commands::SearchArgs),
    /// Save a reply or new message to Gmail's Drafts. It is not sent.
    Draft(commands::DraftArgs),
    /// Print a whole conversation.
    Thread {
        /// A thread ID from `mail search`.
        thread_id: String,
    },
    /// List Gmail labels.
    Labels,
    /// Show the account, the last sync and what is stored.
    Status,
    /// JSON tools for coding agents. Each call runs once and exits.
    Agent(agent::AgentArgs),
}

fn main() -> ExitCode {
    // Finder may pass a process serial number when it opens Mail.app.
    let cli = Cli::parse_from(std::env::args().filter(|argument| !argument.starts_with("-psn_")));
    let paths = match cli.data_dir {
        Some(root) => Paths::under(&root),
        None => match Paths::discover() {
            Ok(paths) => paths,
            Err(error) => {
                eprintln!("mail: {error:#}");
                return ExitCode::FAILURE;
            }
        },
    };

    let result = match cli.command {
        None => window::run(
            &paths,
            window::Options {
                demo: cli.demo,
                screenshot: cli.screenshot,
                demo_keys: cli.demo_keys,
                appearance: cli.appearance.map(|appearance| match appearance {
                    Appearance::Light => egui::Theme::Light,
                    Appearance::Dark => egui::Theme::Dark,
                }),
            },
        ),
        Some(Command::Auth { client }) => commands::auth(&paths, client.as_deref()),
        Some(Command::Sync { days }) => commands::sync(&paths, days),
        Some(Command::Search(args)) => commands::search(&paths, &args),
        Some(Command::Draft(args)) => commands::draft(&paths, args),
        Some(Command::Thread { thread_id }) => commands::thread(&paths, &thread_id),
        Some(Command::Labels) => commands::labels(&paths),
        Some(Command::Status) => commands::status(&paths),
        // The agent CLI reports errors as JSON on stdout.
        Some(Command::Agent(args)) => return agent::run(&paths, args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mail: {error:#}");
            ExitCode::FAILURE
        }
    }
}
