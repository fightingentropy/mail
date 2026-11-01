//! `mail`: a fast, local-first Gmail client for Apple Silicon Macs.
//!
//! Today this is the command line: sign in, sync and search offline. The
//! desktop window arrives in milestone 2 and will open when `mail` runs with
//! no command.

mod agent;
mod commands;
mod dates;

use std::{path::PathBuf, process::ExitCode};

use clap::{CommandFactory, Parser, Subcommand};
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
    #[command(subcommand)]
    command: Option<Command>,
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
    let cli = Cli::parse();
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
        None => {
            let _ = Cli::command().print_help();
            println!(
                "\nThe Mail window arrives in milestone 2. Until then, use the commands above."
            );
            Ok(())
        }
        Some(Command::Auth { client }) => commands::auth(&paths, client.as_deref()),
        Some(Command::Sync { days }) => commands::sync(&paths, days),
        Some(Command::Search(args)) => commands::search(&paths, &args),
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
