//! Attachments download on a thread of their own: a large file never stalls
//! the window, and a download never waits behind a long first sync. Pictures
//! sent inside a message are read here too. Like the sync thread, it ends
//! with the window.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use anyhow::Context;
use mail_core::{
    gmail::{attachments, inline},
    model::{Attachment, InlineImage},
    paths::{self, Paths},
};

use crate::accounts;

pub enum Event {
    /// Saved at `path`, which may have a number added to the name.
    Saved {
        path: PathBuf,
    },
    Failed {
        filename: String,
        error: String,
    },
    /// Saved somewhere temporary, for Quick Look to show.
    Previewed {
        paths: Vec<PathBuf>,
    },
    PreviewFailed {
        error: String,
    },
    /// The pictures sent inside a message that Gmail had, or why none came.
    InlineImages {
        account_id: i64,
        message_id: String,
        images: Result<Vec<InlineImage>, String>,
    },
}

struct Job {
    /// The address of the account the message belongs to.
    account: String,
    message_id: String,
    work: Work,
}

enum Work {
    Attachment(Attachment),
    /// These files, to look at without keeping them.
    Preview(Vec<Attachment>),
    /// The pictures with these Content-IDs, for the account with this ID.
    InlineImages {
        account_id: i64,
        wanted: Vec<String>,
    },
}

pub struct Downloader {
    jobs: Sender<Job>,
    pub events: Receiver<Event>,
}

impl Downloader {
    /// `wake` is called when a download finishes, so the window can repaint.
    /// Previewed files go under `preview_folder`.
    pub fn start(paths: Paths, preview_folder: PathBuf, wake: impl Fn() + Send + 'static) -> Self {
        let (jobs, incoming) = mpsc::channel();
        let (outgoing, events) = mpsc::channel();
        thread::Builder::new()
            .name("downloads".into())
            .spawn(move || run(paths, preview_folder, incoming, outgoing, wake))
            .expect("could not start the download thread");
        Self { jobs, events }
    }

    /// Queues a download. Downloads run one at a time, in order.
    pub fn download(&self, account: String, message_id: String, attachment: Attachment) {
        let _ = self.jobs.send(Job {
            account,
            message_id,
            work: Work::Attachment(attachment),
        });
    }

    /// Queues downloading files somewhere temporary, to show in Quick
    /// Look.
    pub fn preview(&self, account: String, message_id: String, attachments: Vec<Attachment>) {
        let _ = self.jobs.send(Job {
            account,
            message_id,
            work: Work::Preview(attachments),
        });
    }

    /// Queues reading the pictures sent inside a message.
    pub fn inline_images(
        &self,
        account: String,
        account_id: i64,
        message_id: String,
        wanted: Vec<String>,
    ) {
        let _ = self.jobs.send(Job {
            account,
            message_id,
            work: Work::InlineImages { account_id, wanted },
        });
    }
}

fn run(
    paths: Paths,
    preview_folder: PathBuf,
    jobs: Receiver<Job>,
    events: Sender<Event>,
    wake: impl Fn(),
) {
    // A Gmail client of its own: the sync thread's may be busy with a long
    // sync. Each keeps its own quota budget; an attachment costs 20 quota
    // units, a third of a message.
    let mut clients: HashMap<String, accounts::Client> = HashMap::new();
    for job in jobs {
        let event = match carry_out(&paths, &preview_folder, &mut clients, &job) {
            Ok(event) => event,
            Err(error) => {
                // Connect afresh next time: the token may have changed.
                clients.remove(&job.account);
                let error = format!("{error:#}");
                match &job.work {
                    Work::Attachment(attachment) => Event::Failed {
                        filename: attachment.filename.clone(),
                        error,
                    },
                    Work::Preview(_) => Event::PreviewFailed { error },
                    Work::InlineImages { account_id, .. } => Event::InlineImages {
                        account_id: *account_id,
                        message_id: job.message_id.clone(),
                        images: Err(error),
                    },
                }
            }
        };
        if events.send(event).is_err() {
            return;
        }
        wake();
    }
}

fn carry_out(
    paths: &Paths,
    preview_folder: &Path,
    clients: &mut HashMap<String, accounts::Client>,
    job: &Job,
) -> anyhow::Result<Event> {
    if !clients.contains_key(&job.account) {
        clients.insert(job.account.clone(), accounts::client(paths, &job.account)?);
    }
    let gmail = &clients[&job.account];
    match &job.work {
        Work::Attachment(attachment) => {
            let bytes = attachments::fetch(&**gmail, &job.message_id, attachment)?;
            let folder = paths::downloads()?;
            let path = attachments::save(&folder, &attachment.filename, &bytes)
                .with_context(|| format!("Could not save to {}", folder.display()))?;
            Ok(Event::Saved { path })
        }
        Work::Preview(attachments) => {
            // A folder per message, made afresh, so a file shown before is
            // never shown in place of this one.
            let name: String = job
                .message_id
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            let folder = preview_folder.join(format!("message-{name}"));
            let _ = fs::remove_dir_all(&folder);
            fs::create_dir_all(&folder)
                .with_context(|| format!("Could not create {}", folder.display()))?;
            let mut saved = Vec::new();
            for attachment in attachments {
                let bytes = attachments::fetch(&**gmail, &job.message_id, attachment)?;
                let path = attachments::save(&folder, &attachment.filename, &bytes)
                    .with_context(|| format!("Could not save to {}", folder.display()))?;
                saved.push(path);
            }
            Ok(Event::Previewed { paths: saved })
        }
        Work::InlineImages { account_id, wanted } => Ok(Event::InlineImages {
            account_id: *account_id,
            message_id: job.message_id.clone(),
            images: Ok(inline::fetch(&**gmail, &job.message_id, wanted)?),
        }),
    }
}
