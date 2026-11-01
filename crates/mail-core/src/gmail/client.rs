//! [`GmailApi`] over HTTPS, with token refresh, request pacing and retries.

use std::{
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use serde::{
    Deserialize,
    de::{DeserializeOwned, IgnoredAny},
};

use super::{ApiLabel, ApiMessage, GmailApi, HistoryPage, IdPage, Outcome, Profile};
use crate::auth::{self, AccessToken, OAuthClient};

const API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const MAX_ATTEMPTS: u32 = 8;

/// Gmail allows 6,000 quota units per user per minute. Spacing requests
/// evenly at 95 units a second stays just under it, so Gmail rarely has to
/// refuse one.
const UNITS_PER_SECOND: f64 = 95.0;

/// What each call costs in quota units, from Gmail's usage limits page (costs
/// changed on 1 May 2026). A full message is by far the most expensive: the
/// quota allows about 100 a minute.
const PROFILE_UNITS: u32 = 1;
const LABELS_UNITS: u32 = 1;
const LIST_UNITS: u32 = 5;
const MESSAGE_UNITS: u32 = 60;
const HISTORY_UNITS: u32 = 2;
const MODIFY_UNITS: u32 = 10;
const TRASH_UNITS: u32 = 20;
const UNTRASH_UNITS: u32 = 10;

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
            pacer: Pacer::new(UNITS_PER_SECOND),
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

    /// Sends one request with pacing, token refresh and retries. `request`
    /// receives the Authorization header value.
    fn send<T: DeserializeOwned>(
        &self,
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
                Err(error) if attempt < MAX_ATTEMPTS && is_transient(&error) => {
                    thread::sleep(back_off(attempt));
                    continue;
                }
                Err(error) => return Err(error).context("Could not reach Gmail"),
            };

            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                let value = response
                    .body_mut()
                    .with_config()
                    .limit(64 * 1024 * 1024)
                    .read_json()
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
}

/// Spreads requests from all workers evenly over time, so a burst never
/// trips Gmail's per-user quota.
struct Pacer {
    units_per_second: f64,
    next: Mutex<Instant>,
}

impl Pacer {
    fn new(units_per_second: f64) -> Self {
        Self {
            units_per_second,
            next: Mutex::new(Instant::now()),
        }
    }

    /// Waits for this request's turn. Each request books the slot after it in
    /// proportion to its cost.
    fn wait(&self, units: u32) {
        let start = {
            let mut next = self.next.lock().unwrap();
            let start = (*next).max(Instant::now());
            *next = start + Duration::from_secs_f64(f64::from(units) / self.units_per_second);
            start
        };
        thread::sleep(start.saturating_duration_since(Instant::now()));
    }

    /// Pushes every worker's next request back by at least `delay`.
    fn hold(&self, delay: Duration) {
        let mut next = self.next.lock().unwrap();
        *next = (*next).max(Instant::now() + delay);
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

    #[test]
    fn pacer_spreads_requests_by_cost() {
        let pacer = Pacer::new(1_000.0);
        let started = Instant::now();
        for _ in 0..11 {
            pacer.wait(10);
        }
        // Ten gaps of 10 units at 1,000 units a second.
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    #[test]
    fn hold_delays_the_next_request() {
        let pacer = Pacer::new(1_000_000.0);
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
