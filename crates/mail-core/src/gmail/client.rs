//! [`GmailApi`] over HTTPS, with token refresh, request pacing and retries.

use std::{
    collections::VecDeque,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use base64::Engine;
use serde::{
    Deserialize,
    de::{DeserializeOwned, IgnoredAny},
};

use super::{
    ApiLabel, ApiMessage, GmailApi, HistoryPage, IdPage, Outcome, Profile, message::BASE64URL,
};
use crate::{
    auth::{self, AccessToken, OAuthClient},
    compose,
};

const API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const USERINFO: &str = "https://www.googleapis.com/oauth2/v3/userinfo";
/// Google serves profile photos only from here; any other address is ignored.
const PHOTO_HOST: &str = "https://lh3.googleusercontent.com/";
const MAX_ATTEMPTS: u32 = 8;

/// Gmail allows 6,000 quota units per user per minute. Mail spends at most
/// 5,700 in any 61 seconds: the margin covers requests that reach Google a
/// little later than they left, and the window's other Gmail clients
/// (attachments, drafts), which keep budgets of their own.
const QUOTA_UNITS: u32 = 5_700;
const QUOTA_WINDOW: Duration = Duration::from_secs(61);

/// What each call costs in quota units, from Gmail's usage limits page (costs
/// changed on 1 May 2026). A full message is by far the most expensive: the
/// quota allows about 100 a minute.
const PROFILE_UNITS: u32 = 1;
const LABELS_UNITS: u32 = 1;
const LIST_UNITS: u32 = 5;
const MESSAGE_UNITS: u32 = 60;
const MINIMAL_MESSAGE_UNITS: u32 = 20;
const METADATA_MESSAGE_UNITS: u32 = 20;
const ATTACHMENT_UNITS: u32 = 20;
const HISTORY_UNITS: u32 = 2;
const MODIFY_UNITS: u32 = 10;
const TRASH_UNITS: u32 = 20;
const UNTRASH_UNITS: u32 = 10;
const CREATE_DRAFT_UNITS: u32 = 10;
const UPDATE_DRAFT_UNITS: u32 = 15;
const DELETE_DRAFT_UNITS: u32 = 10;
const SEND_DRAFT_UNITS: u32 = 100;

pub struct GmailClient {
    http: ureq::Agent,
    oauth: OAuthClient,
    refresh_token: String,
    access: Mutex<Option<AccessToken>>,
    pacer: Pacer,
}

impl GmailClient {
    /// `access` can carry a token from a sign-in that just happened, saving one
    /// refresh.
    pub fn new(oauth: OAuthClient, refresh_token: String, access: Option<AccessToken>) -> Self {
        Self {
            http: http_agent(),
            oauth,
            refresh_token,
            access: Mutex::new(access),
            pacer: Pacer::new(QUOTA_UNITS, QUOTA_WINDOW),
        }
    }

    fn access_token(&self) -> anyhow::Result<String> {
        let mut access = self.access.lock().unwrap();
        if let Some(token) = access.as_ref().filter(|token| token.is_fresh()) {
            return Ok(token.value.clone());
        }
        let token = auth::refresh(&self.http, &self.oauth, &self.refresh_token)?;
        let value = token.value.clone();
        *access = Some(token);
        Ok(value)
    }

    fn forget_access_token(&self) {
        *self.access.lock().unwrap() = None;
    }

    /// GETs `path` under the user's mailbox. `None` means 404.
    fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
        units: u32,
    ) -> anyhow::Result<Option<T>> {
        let url = format!("{API}/{path}");
        let reply = self.send(path, units, |authorization| {
            self.http
                .get(&url)
                .header("Authorization", authorization)
                .query_pairs(query.iter().copied())
                .call()
        })?;
        match reply {
            Reply::Done(value) => Ok(Some(value)),
            Reply::NotFound => Ok(None),
            Reply::Rejected(message) => bail!("Gmail rejected {path}: {message}"),
        }
    }

    /// POSTs a thread change. Gmail answers with the thread, which Mail does
    /// not need: the next sync reads the result from history.
    fn post_thread(
        &self,
        thread_id: &str,
        action: &str,
        body: Option<serde_json::Value>,
        units: u32,
    ) -> anyhow::Result<Outcome> {
        let path = format!("threads/{thread_id}/{action}");
        let url = format!("{API}/{path}");
        let reply = self.send::<IgnoredAny>(&path, units, |authorization| {
            let request = self.http.post(&url).header("Authorization", authorization);
            match &body {
                Some(body) => request.send_json(body),
                None => request.send_empty(),
            }
        })?;
        Ok(match reply {
            Reply::Done(_) => Outcome::Done,
            Reply::NotFound => Outcome::Gone,
            Reply::Rejected(message) => Outcome::Rejected(message),
        })
    }

    /// Creates or replaces a draft. Returns the draft's ID.
    fn put_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> anyhow::Result<Reply<DraftRef>> {
        let mut message = serde_json::json!({"raw": compose::raw_for_api(raw)});
        if let Some(thread_id) = thread_id {
            message["threadId"] = thread_id.into();
        }
        match draft_id {
            Some(id) => {
                let path = format!("drafts/{id}");
                let url = format!("{API}/{path}");
                let body = serde_json::json!({"id": id, "message": message});
                self.send(&path, UPDATE_DRAFT_UNITS, |authorization| {
                    self.http
                        .put(&url)
                        .header("Authorization", authorization)
                        .send_json(&body)
                })
            }
            None => {
                let url = format!("{API}/drafts");
                let body = serde_json::json!({"message": message});
                self.send("drafts", CREATE_DRAFT_UNITS, |authorization| {
                    self.http
                        .post(&url)
                        .header("Authorization", authorization)
                        .send_json(&body)
                })
            }
        }
    }

    /// Sends one request with pacing, token refresh and retries. `request`
    /// receives the Authorization header value.
    fn send<T: DeserializeOwned>(
        &self,
        path: &str,
        units: u32,
        request: impl Fn(&str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> anyhow::Result<Reply<T>> {
        self.send_once_if(true, path, units, request)
    }

    /// Like [`Self::send`]. With `repeatable` false, an answer that might
    /// have come after Gmail acted (a timeout, a dropped connection or a 5xx)
    /// is not retried, because repeating the request could act twice or find
    /// its own earlier effect and misreport it. Refusals that Gmail gives
    /// before acting (quota, an expired token) are still retried.
    fn send_once_if<T: DeserializeOwned>(
        &self,
        repeatable: bool,
        path: &str,
        units: u32,
        request: impl Fn(&str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> anyhow::Result<Reply<T>> {
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            attempt += 1;
            self.pacer.wait(units);
            let authorization = format!("Bearer {}", self.access_token()?);
            let mut response = match request(&authorization) {
                Ok(response) => response,
                Err(error) if repeatable && attempt < MAX_ATTEMPTS && is_transient(&error) => {
                    thread::sleep(back_off(attempt));
                    continue;
                }
                Err(error) if !repeatable && is_transient(&error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "Gmail did not answer for {path}; the request may have gone through"
                        )
                    });
                }
                Err(error) => return Err(error).context("Could not reach Gmail"),
            };

            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                let text = response
                    .body_mut()
                    .with_config()
                    .limit(64 * 1024 * 1024)
                    .read_to_string()
                    .with_context(|| format!("Could not read Gmail's answer for {path}"))?;
                // A delete answers 204 with no body at all.
                let text = if text.trim().is_empty() {
                    "null"
                } else {
                    text.as_str()
                };
                let value = serde_json::from_str(text)
                    .with_context(|| format!("Unexpected response from Gmail for {path}"))?;
                return Ok(Reply::Done(value));
            }
            if status == 404 {
                return Ok(Reply::NotFound);
            }
            let body = response.body_mut().read_to_string().unwrap_or_default();
            if status == 401 && !refreshed {
                self.forget_access_token();
                refreshed = true;
                continue;
            }
            if !repeatable && (500..=504).contains(&status) {
                bail!(
                    "Gmail returned {status} for {path}, so the request may have gone through: {}",
                    error_message(&body)
                );
            }
            if attempt < MAX_ATTEMPTS && should_retry(status, &body) {
                // Every worker waits, not just this one: the quota is shared.
                self.pacer.hold(back_off(attempt));
                continue;
            }
            if status == 400 {
                return Ok(Reply::Rejected(error_message(&body)));
            }
            bail!(
                "Gmail returned {status} for {path}: {}",
                error_message(&body)
            );
        }
    }
}

enum Reply<T> {
    Done(T),
    NotFound,
    /// 400: the request can never succeed as sent.
    Rejected(String),
}

impl GmailApi for GmailClient {
    /// Your Google profile photo as image bytes (usually JPEG). `None` when
    /// the sign-in didn't include [`auth::PROFILE_SCOPE`] or there is no
    /// photo. Costs no Gmail quota.
    fn profile_photo(&self) -> anyhow::Result<Option<Vec<u8>>> {
        #[derive(Deserialize)]
        struct UserInfo {
            picture: Option<String>,
        }
        let authorization = format!("Bearer {}", self.access_token()?);
        let mut response = self
            .http
            .get(USERINFO)
            .header("Authorization", &authorization)
            .call()
            .context("Could not reach Google")?;
        // 401 or 403: this sign-in may not see the profile.
        if response.status() != 200 {
            return Ok(None);
        }
        let info: UserInfo = response.body_mut().read_json()?;
        let Some(picture) = info.picture.filter(|url| url.starts_with(PHOTO_HOST)) else {
            return Ok(None);
        };
        // The URL ends in a size such as "=s96-c"; ask for one sharp on Retina.
        let url = match picture.rsplit_once('=') {
            Some((base, _)) => format!("{base}=s256-c"),
            None => picture,
        };
        let mut photo = self
            .http
            .get(&url)
            .call()
            .context("Could not download the profile photo")?;
        if photo.status() != 200 {
            return Ok(None);
        }
        let bytes = photo
            .body_mut()
            .with_config()
            .limit(5 * 1024 * 1024)
            .read_to_vec()?;
        Ok(Some(bytes))
    }

    fn profile(&self) -> anyhow::Result<Profile> {
        self.get("profile", &[], PROFILE_UNITS)?
            .context("Gmail profile not found")
    }

    fn labels(&self) -> anyhow::Result<Vec<ApiLabel>> {
        #[derive(Deserialize)]
        struct Labels {
            #[serde(default)]
            labels: Vec<ApiLabel>,
        }
        let labels: Labels = self
            .get("labels", &[], LABELS_UNITS)?
            .context("Gmail labels not found")?;
        Ok(labels.labels)
    }

    fn message_ids(&self, query: &str, page_token: Option<&str>) -> anyhow::Result<IdPage> {
        let mut params = vec![("q", query), ("maxResults", "500")];
        if let Some(token) = page_token {
            params.push(("pageToken", token));
        }
        Ok(self
            .get("messages", &params, LIST_UNITS)?
            .unwrap_or_default())
    }

    fn message(&self, id: &str) -> anyhow::Result<Option<ApiMessage>> {
        // `full` returns the parsed MIME tree with text parts inline and
        // attachments by reference, so sync never downloads attachments.
        self.get(
            &format!("messages/{id}"),
            &[("format", "full")],
            MESSAGE_UNITS,
        )
    }

    fn attachment(&self, message_id: &str, attachment_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
        #[derive(Deserialize)]
        struct Contents {
            #[serde(default)]
            data: String,
        }
        let path = format!("messages/{message_id}/attachments/{attachment_id}");
        let url = format!("{API}/{path}");
        let reply = self.send::<Contents>(&path, ATTACHMENT_UNITS, |authorization| {
            self.http
                .get(&url)
                .header("Authorization", authorization)
                .call()
        })?;
        match reply {
            Reply::Done(contents) => {
                let bytes = BASE64URL
                    .decode(contents.data.trim_end())
                    .context("Gmail sent an attachment Mail could not decode")?;
                Ok(Some(bytes))
            }
            // An attachment ID Gmail has replaced comes back as 400 or 404.
            Reply::NotFound | Reply::Rejected(_) => Ok(None),
        }
    }

    fn message_headers(&self, id: &str) -> anyhow::Result<Option<ApiMessage>> {
        // Only the headers Mail stores; Gmail sends all of them otherwise.
        let query = [
            ("format", "metadata"),
            ("metadataHeaders", "From"),
            ("metadataHeaders", "To"),
            ("metadataHeaders", "Cc"),
            ("metadataHeaders", "Subject"),
            ("metadataHeaders", "Message-ID"),
            ("metadataHeaders", "In-Reply-To"),
            ("metadataHeaders", "References"),
        ];
        self.get(&format!("messages/{id}"), &query, METADATA_MESSAGE_UNITS)
    }

    fn message_labels(&self, id: &str) -> anyhow::Result<Option<Vec<String>>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Minimal {
            #[serde(default)]
            label_ids: Vec<String>,
        }
        let minimal: Option<Minimal> = self.get(
            &format!("messages/{id}"),
            &[("format", "minimal")],
            MINIMAL_MESSAGE_UNITS,
        )?;
        Ok(minimal.map(|minimal| minimal.label_ids))
    }

    fn history(
        &self,
        start_history_id: &str,
        page_token: Option<&str>,
    ) -> anyhow::Result<Option<HistoryPage>> {
        let mut params = vec![("startHistoryId", start_history_id), ("maxResults", "500")];
        if let Some(token) = page_token {
            params.push(("pageToken", token));
        }
        self.get("history", &params, HISTORY_UNITS)
    }

    fn modify_thread(
        &self,
        thread_id: &str,
        add: &[String],
        remove: &[String],
    ) -> anyhow::Result<Outcome> {
        let body = serde_json::json!({"addLabelIds": add, "removeLabelIds": remove});
        self.post_thread(thread_id, "modify", Some(body), MODIFY_UNITS)
    }

    fn trash_thread(&self, thread_id: &str) -> anyhow::Result<Outcome> {
        self.post_thread(thread_id, "trash", None, TRASH_UNITS)
    }

    fn untrash_thread(&self, thread_id: &str) -> anyhow::Result<Outcome> {
        self.post_thread(thread_id, "untrash", None, UNTRASH_UNITS)
    }

    fn create_draft(&self, raw: &[u8], thread_id: Option<&str>) -> anyhow::Result<String> {
        match self.put_draft(None, raw, thread_id)? {
            Reply::Done(draft) => Ok(draft.id),
            Reply::NotFound => bail!("Gmail could not find the thread for this draft"),
            Reply::Rejected(message) => bail!("Gmail refused the draft: {message}"),
        }
    }

    fn update_draft(
        &self,
        draft_id: &str,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> anyhow::Result<Outcome> {
        Ok(match self.put_draft(Some(draft_id), raw, thread_id)? {
            Reply::Done(_) => Outcome::Done,
            Reply::NotFound => Outcome::Gone,
            Reply::Rejected(message) => Outcome::Rejected(message),
        })
    }

    fn delete_draft(&self, draft_id: &str) -> anyhow::Result<Outcome> {
        let path = format!("drafts/{draft_id}");
        let url = format!("{API}/{path}");
        let reply = self.send::<IgnoredAny>(&path, DELETE_DRAFT_UNITS, |authorization| {
            self.http
                .delete(&url)
                .header("Authorization", authorization)
                .call()
        })?;
        Ok(match reply {
            Reply::Done(_) => Outcome::Done,
            Reply::NotFound => Outcome::Gone,
            Reply::Rejected(message) => Outcome::Rejected(message),
        })
    }

    fn send_draft(&self, draft_id: &str) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Sent {
            id: String,
        }
        let url = format!("{API}/drafts/send");
        let body = serde_json::json!({"id": draft_id});
        // Not repeatable: if the answer is lost after Gmail sent the draft, a
        // second try would find it gone and report that nothing was sent.
        let reply =
            self.send_once_if::<Sent>(false, "drafts/send", SEND_DRAFT_UNITS, |authorization| {
                self.http
                    .post(&url)
                    .header("Authorization", authorization)
                    .send_json(&body)
            })?;
        match reply {
            Reply::Done(sent) => Ok(sent.id),
            Reply::NotFound => bail!("The draft is gone from Gmail; nothing was sent"),
            Reply::Rejected(message) => bail!("Gmail refused to send it: {message}"),
        }
    }
}

#[derive(Deserialize)]
struct DraftRef {
    id: String,
}

/// Keeps every worker's requests within Gmail's per-user quota. A request
/// goes at once while the last window's spending leaves room for it, so a
/// catch-up of up to a window's budget (about 95 full messages) runs at
/// network speed. A longer sync waits for the oldest requests to leave the
/// window, so it goes in bursts a window apart: the most the quota allows.
struct Pacer {
    budget: u32,
    window: Duration,
    spent: Mutex<Spent>,
}

impl Pacer {
    fn new(budget: u32, window: Duration) -> Self {
        Self {
            budget,
            window,
            spent: Mutex::new(Spent::default()),
        }
    }

    /// Waits for this request's turn.
    fn wait(&self, units: u32) {
        let start =
            self.spent
                .lock()
                .unwrap()
                .book(Instant::now(), units, self.budget, self.window);
        thread::sleep(start.saturating_duration_since(Instant::now()));
    }

    /// After Gmail refused a request: pushes every worker's next request back
    /// by at least `delay`, then keeps them to the steady pace, since
    /// something else may be spending this account's quota too.
    fn hold(&self, delay: Duration) {
        self.spent.lock().unwrap().hold(Instant::now() + delay);
    }
}

/// What the pacer has booked: requests still inside the window, and when
/// the next may go.
#[derive(Default)]
struct Spent {
    /// When each request in the window started and what it cost, oldest
    /// first. Starts never go backwards, so this stays in order.
    booked: VecDeque<(Instant, u32)>,
    /// The sum of `booked`.
    total: u32,
    /// After Gmail refused a request: the earliest the next may go, keeping
    /// to the budget's average rate. Cleared once requests stop queuing.
    steady: Option<Instant>,
}

impl Spent {
    /// No request goes before `until`, and the ones after keep a steady pace.
    fn hold(&mut self, until: Instant) {
        self.steady = Some(self.steady.map_or(until, |next| next.max(until)));
    }

    /// Books a request of `units` asked for at `now` and returns when it may
    /// start. Within any `window`, the requests booked never cost more than
    /// `budget`.
    fn book(&mut self, now: Instant, units: u32, budget: u32, window: Duration) -> Instant {
        // A request dearer than the whole budget goes once the window is empty.
        let units = units.min(budget);
        let mut start = now;
        if let Some(&(last, _)) = self.booked.back() {
            start = start.max(last);
        }
        match self.steady {
            Some(next) if next > now => start = start.max(next),
            // Caught up with the pace: requests no longer queue, so a later
            // burst may go at once again.
            _ => self.steady = None,
        }
        loop {
            // Requests that left the window before `start` no longer count;
            // later requests start later still, so they never will again.
            while let Some(&(at, cost)) = self.booked.front()
                && at + window <= start
            {
                self.booked.pop_front();
                self.total -= cost;
            }
            if self.total + units <= budget {
                break;
            }
            // Wait for the oldest request to leave the window.
            start = self.booked.front().expect("over budget, so not empty").0 + window;
        }
        if self.steady.is_some() {
            let gap = window.mul_f64(f64::from(units) / f64::from(budget));
            self.steady = Some(start + gap);
        }
        self.booked.push_back((start, units));
        self.total += units;
        start
    }
}

/// One HTTP agent configuration for Gmail and Google sign-in. Status codes
/// are returned, not raised, so error bodies can be read.
pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn is_transient(error: &ureq::Error) -> bool {
    matches!(
        error,
        ureq::Error::Io(_) | ureq::Error::Timeout(_) | ureq::Error::ConnectionFailed
    )
}

/// Server errors and quota errors are worth retrying. Gmail reports quota as
/// 429, or as 403 with a rate-limit reason or a "Quota exceeded" message.
fn should_retry(status: u16, body: &str) -> bool {
    match status {
        429 | 500 | 502 | 503 | 504 => true,
        403 => {
            let body = body.to_ascii_lowercase();
            body.contains("ratelimitexceeded") || body.contains("quota exceeded")
        }
        _ => false,
    }
}

/// Exponential backoff from 2 seconds up to a minute, the length of Gmail's
/// quota window, with jitter so workers don't retry in step.
fn back_off(attempt: u32) -> Duration {
    let base = Duration::from_secs(2) * 2u32.pow(attempt.saturating_sub(1).min(5));
    let jitter = Duration::from_millis(u64::from(getrandom::u32().unwrap_or(0) % 500));
    base.min(Duration::from_secs(60)) + jitter
}

/// Pulls Google's human-readable message out of an error body.
fn error_message(body: &str) -> String {
    #[derive(Deserialize)]
    struct Envelope {
        error: Detail,
    }
    #[derive(Deserialize)]
    struct Detail {
        message: String,
    }
    serde_json::from_str::<Envelope>(body)
        .map(|envelope| envelope.error.message)
        .unwrap_or_else(|_| body.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_errors_are_retried() {
        let quota = r#"{"error": {"code": 403, "message": "Quota exceeded for quota metric 'Total Query Cost' and limit 'Units per minute per user' of service 'gmail.googleapis.com'.", "status": "PERMISSION_DENIED"}}"#;
        assert!(should_retry(403, quota));
        assert!(should_retry(
            403,
            r#"{"error": {"errors": [{"reason": "userRateLimitExceeded"}]}}"#
        ));
        assert!(should_retry(429, ""));
        assert!(!should_retry(
            403,
            r#"{"error": {"message": "Insufficient Permission"}}"#
        ));
        assert!(!should_retry(400, ""));
    }

    fn client_with_token() -> GmailClient {
        let token = AccessToken {
            value: "fake".into(),
            expires_at: Instant::now() + Duration::from_secs(3600),
        };
        GmailClient::new(
            OAuthClient {
                client_id: "id".into(),
                client_secret: "secret".into(),
            },
            "refresh".into(),
            Some(token),
        )
    }

    #[test]
    fn sending_a_draft_is_not_repeated_after_a_lost_answer() {
        let calls = std::cell::Cell::new(0);
        let lost = |_: &str| {
            calls.set(calls.get() + 1);
            Err(ureq::Error::Io(std::io::Error::other("connection reset")))
        };
        let client = client_with_token();
        let error = match client.send_once_if::<IgnoredAny>(false, "drafts/send", 1, lost) {
            Err(error) => error,
            Ok(_) => panic!("a lost answer cannot succeed"),
        };
        assert_eq!(calls.get(), 1, "drafts/send must not be repeated");
        assert!(format!("{error:#}").contains("may have gone through"));
    }

    const WINDOW: Duration = Duration::from_secs(60);

    /// Books `count` requests of `units`, all asked for at `now`, as busy
    /// workers do, and returns their starts.
    fn book_all(spent: &mut Spent, now: Instant, count: usize, units: u32) -> Vec<Instant> {
        (0..count)
            .map(|_| spent.book(now, units, 6_000, WINDOW))
            .collect()
    }

    #[test]
    fn a_small_sync_goes_at_once() {
        let now = Instant::now();
        let starts = book_all(&mut Spent::default(), now, 100, 60);
        assert!(starts.iter().all(|start| *start == now));
    }

    #[test]
    fn a_full_window_waits_for_the_oldest_to_leave() {
        let now = Instant::now();
        let starts = book_all(&mut Spent::default(), now, 201, 60);
        // A hundred fill the window; the next hundred go as they leave it.
        assert!(starts[..100].iter().all(|start| *start == now));
        assert!(starts[100..200].iter().all(|start| *start == now + WINDOW));
        assert_eq!(starts[200], now + WINDOW * 2);
    }

    #[test]
    fn no_window_spends_more_than_the_budget() {
        let now = Instant::now();
        let mut spent = Spent::default();
        // Mixed costs, as when sync downloads while labels are read.
        let costs: Vec<u32> = (0..600).map(|i| [60, 20, 5][i % 3]).collect();
        let starts: Vec<Instant> = costs
            .iter()
            .enumerate()
            .map(|(i, units)| {
                // Some arrive late, as workers finish at different times.
                let asked = now + Duration::from_millis(37 * i as u64);
                spent.book(asked, *units, 6_000, WINDOW)
            })
            .collect();
        for (i, start) in starts.iter().enumerate() {
            let in_window: u32 = starts
                .iter()
                .zip(&costs)
                .filter(|(other, _)| **other <= *start && **other + WINDOW > *start)
                .map(|(_, units)| units)
                .sum();
            assert!(in_window <= 6_000, "request {i}: {in_window} units");
        }
        assert!(starts.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn a_burst_is_allowed_again_after_a_quiet_spell() {
        let now = Instant::now();
        let mut spent = Spent::default();
        book_all(&mut spent, now, 150, 60);
        // The long sync has finished; the next sync, minutes later, has the
        // whole window again.
        let later = now + Duration::from_secs(300);
        let starts = book_all(&mut spent, later, 100, 60);
        assert!(starts.iter().all(|start| *start == later));
    }

    #[test]
    fn after_a_refusal_requests_wait_then_keep_a_steady_pace() {
        let now = Instant::now();
        let mut spent = Spent::default();
        let until = now + Duration::from_secs(2);
        spent.hold(until);
        let starts = book_all(&mut spent, now, 3, 60);
        let gap = Duration::from_millis(600);
        assert_eq!(starts, [until, until + gap, until + gap * 2]);
    }

    #[test]
    fn hold_delays_the_next_request() {
        let pacer = Pacer::new(6_000, WINDOW);
        pacer.hold(Duration::from_millis(50));
        let started = Instant::now();
        pacer.wait(1);
        assert!(started.elapsed() >= Duration::from_millis(45));
    }

    #[test]
    fn backoff_grows_to_a_minute() {
        assert!(back_off(1) >= Duration::from_secs(2));
        assert!(back_off(1) < Duration::from_secs(3));
        assert!(back_off(8) >= Duration::from_secs(60));
        assert!(back_off(8) < Duration::from_secs(61));
    }
}
