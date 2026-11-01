//! [`GmailApi`] over HTTPS, with token refresh and polite retries.

use std::{sync::Mutex, thread, time::Duration};

use anyhow::{Context, bail};
use serde::{Deserialize, de::DeserializeOwned};

use super::{ApiLabel, ApiMessage, GmailApi, HistoryPage, IdPage, Profile};
use crate::auth::{self, AccessToken, OAuthClient};

const API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const MAX_ATTEMPTS: u32 = 6;

pub struct GmailClient {
    http: ureq::Agent,
    oauth: OAuthClient,
    refresh_token: String,
    access: Mutex<Option<AccessToken>>,
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
    ) -> anyhow::Result<Option<T>> {
        let url = format!("{API}/{path}");
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let request = self
                .http
                .get(&url)
                .header("Authorization", &format!("Bearer {}", self.access_token()?))
                .query_pairs(query.iter().copied());
            let mut response = match request.call() {
                Ok(response) => response,
                Err(error) if attempt < MAX_ATTEMPTS && is_transient(&error) => {
                    back_off(attempt);
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
                return Ok(Some(value));
            }
            if status == 404 {
                return Ok(None);
            }
            let body = response.body_mut().read_to_string().unwrap_or_default();
            if status == 401 && !refreshed {
                self.forget_access_token();
                refreshed = true;
                continue;
            }
            if attempt < MAX_ATTEMPTS && is_rate_limited(status, &body) {
                back_off(attempt);
                continue;
            }
            bail!(
                "Gmail returned {status} for {path}: {}",
                error_message(&body)
            );
        }
    }
}

impl GmailApi for GmailClient {
    fn profile(&self) -> anyhow::Result<Profile> {
        self.get("profile", &[])?.context("Gmail profile not found")
    }

    fn labels(&self) -> anyhow::Result<Vec<ApiLabel>> {
        #[derive(Deserialize)]
        struct Labels {
            #[serde(default)]
            labels: Vec<ApiLabel>,
        }
        let labels: Labels = self.get("labels", &[])?.context("Gmail labels not found")?;
        Ok(labels.labels)
    }

    fn message_ids(&self, query: &str, page_token: Option<&str>) -> anyhow::Result<IdPage> {
        let mut params = vec![("q", query), ("maxResults", "500")];
        if let Some(token) = page_token {
            params.push(("pageToken", token));
        }
        Ok(self.get("messages", &params)?.unwrap_or_default())
    }

    fn message(&self, id: &str) -> anyhow::Result<Option<ApiMessage>> {
        // `full` returns the parsed MIME tree with text parts inline and
        // attachments by reference, so sync never downloads attachments.
        self.get(&format!("messages/{id}"), &[("format", "full")])
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
        self.get("history", &params)
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

/// Gmail allows 250 quota units per user per second; a message fetch costs 5.
/// Over that it answers 429, or 403 with a rate-limit reason.
fn is_rate_limited(status: u16, body: &str) -> bool {
    match status {
        429 | 500 | 502 | 503 | 504 => true,
        403 => body.contains("rateLimitExceeded") || body.contains("userRateLimitExceeded"),
        _ => false,
    }
}

/// Exponential backoff with jitter, so parallel fetches don't retry in step.
fn back_off(attempt: u32) {
    let base = Duration::from_millis(500) * 2u32.pow(attempt.min(5));
    let jitter = Duration::from_millis(u64::from(getrandom::u32().unwrap_or(0) % 250));
    thread::sleep(base + jitter);
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
