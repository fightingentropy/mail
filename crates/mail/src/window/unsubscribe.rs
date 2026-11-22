//! Unsubscribing, on short-lived threads so the window never waits on the
//! network: reading how a sender takes the request, for mail stored before
//! Mail kept that, and sending the request you confirmed.

use std::{sync::mpsc::Sender, thread};

use mail_core::{
    compose,
    gmail::parse_message,
    paths::Paths,
    unsubscribe::{self, Method},
};

use crate::accounts;

pub enum Finished {
    /// A message's `List-Unsubscribe` header and whether one request is
    /// enough, `None` if the message is gone, or why it couldn't be read.
    Found {
        account_id: i64,
        message_id: String,
        headers: Result<Option<(String, bool)>, String>,
    },
    /// The request went, or why it didn't.
    Done {
        sender: String,
        result: Result<(), String>,
    },
}

/// Reads a message's headers from the mailbox of `account`, an address.
pub fn find(
    paths: &Paths,
    account: String,
    account_id: i64,
    message_id: String,
    done: Sender<Finished>,
    wake: impl Fn() + Send + 'static,
) {
    let paths = paths.clone();
    thread::spawn(move || {
        let headers = accounts::client(&paths, &account)
            .and_then(|mailbox| mailbox.message_headers(&message_id))
            .map(|found| {
                found.map(|api| {
                    let message = parse_message(api);
                    (
                        message.list_unsubscribe.unwrap_or_default(),
                        message.unsubscribe_one_click,
                    )
                })
            })
            .map_err(|error| format!("{error:#}"));
        let _ = done.send(Finished::Found {
            account_id,
            message_id,
            headers,
        });
        wake();
    });
}

/// Sends the request you confirmed: one POST to the sender's address, or an
/// email from `account`. A page is opened by the window itself.
pub fn request(
    paths: &Paths,
    account: String,
    sender: String,
    method: Method,
    done: Sender<Finished>,
    wake: impl Fn() + Send + 'static,
) {
    let paths = paths.clone();
    thread::spawn(move || {
        let result = match &method {
            Method::OneClick(url) => unsubscribe::post_one_click(url),
            Method::Email(request) => accounts::client(&paths, &account)
                .and_then(|mailbox| {
                    compose::send(&*mailbox, &account, &request.draft(), None, |_| {})
                })
                .map(|_| ()),
            Method::Page(_) => Ok(()),
        }
        .map_err(|error| format!("{error:#}"));
        let _ = done.send(Finished::Done { sender, result });
        wake();
    });
}
