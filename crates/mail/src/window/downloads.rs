//! Attachments download on a thread of their own: a large file never stalls
//! the window, and a download never waits behind a long first sync. Like the
//! sync thread, it ends with the window.

use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use anyhow::Context;
use mail_core::{
    gmail::{GmailClient, attachments},
    model::Attachment,
    paths::{self, Paths},
};

use super::background;

pub enum Event {
    /// Saved at `path`, which may have a number added to the name.
    Saved {
        path: PathBuf,
    },
    Failed {
        filename: String,
        error: String,
    },
}

struct Job {
    message_id: String,
    attachment: Attachment,
}

pub struct Downloader {
    jobs: Sender<Job>,
    pub events: Receiver<Event>,
}

impl Downloader {
    /// `wake` is called when a download finishes, so the window can repaint.
    pub fn start(paths: Paths, wake: impl Fn() + Send + 'static) -> Self {
        let (jobs, incoming) = mpsc::channel();
        let (outgoing, events) = mpsc::channel();
        thread::Builder::new()
            .name("downloads".into())
            .spawn(move || run(paths, incoming, outgoing, wake))
            .expect("could not start the download thread");
        Self { jobs, events }
    }

    /// Queues a download. Downloads run one at a time, in order.
    pub fn download(&self, message_id: String, attachment: Attachment) {
        let _ = self.jobs.send(Job {
            message_id,
            attachment,
        });
    }
}

fn run(paths: Paths, jobs: Receiver<Job>, events: Sender<Event>, wake: impl Fn()) {
    // A Gmail client of its own: the sync thread's may be busy with a long
    // sync. Each paces itself; an attachment costs 20 quota units, a third
    // of a message.
    let mut gmail: Option<GmailClient> = None;
    for job in jobs {
        let event = match download(&paths, &mut gmail, &job) {
            Ok(path) => Event::Saved { path },
            Err(error) => {
                // Connect afresh next time: the token may have changed.
                gmail = None;
                Event::Failed {
                    filename: job.attachment.filename.clone(),
                    error: format!("{error:#}"),
                }
            }
        };
        if events.send(event).is_err() {
            return;
        }
        wake();
    }
}

fn download(paths: &Paths, gmail: &mut Option<GmailClient>, job: &Job) -> anyhow::Result<PathBuf> {
    if gmail.is_none() {
        *gmail = Some(background::connect(paths)?.1);
    }
    let gmail = gmail.as_ref().expect("connected above");
    let bytes = attachments::fetch(gmail, &job.message_id, &job.attachment)?;
    let folder = paths::downloads()?;
    attachments::save(&folder, &job.attachment.filename, &bytes)
        .with_context(|| format!("Could not save to {}", folder.display()))
}
