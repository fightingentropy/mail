//! Google sign-in for a desktop app: OAuth 2.0 with PKCE and a loopback
//! redirect (RFC 8252).
//!
//! The browser handles the Google password and 2-Step Verification; Mail only
//! ever receives an authorization code. The long-lived refresh token goes into
//! the login Keychain, and access tokens live in memory for an hour.

use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Read, label, archive, trash, draft and send. Cannot permanently delete.
/// Mail sends only from the window, after you confirm.
pub const GMAIL_SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify";

/// Your name and profile photo, for the account menu. Google counts it as
/// non-sensitive. Accounts signed in before it was asked for show initials
/// until they sign in again.
pub const PROFILE_SCOPE: &str = "https://www.googleapis.com/auth/userinfo.profile";

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const KEYCHAIN_SERVICE: &str = "org.erlin.mail";

/// A "Desktop app" OAuth client from your own Google Cloud project. Google
/// says the secret of an installed app "is obviously not treated as a secret";
/// it is still kept out of the repository.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuthClient {
    pub client_id: String,
    pub client_secret: String,
}

impl OAuthClient {
    /// Reads the JSON file Google Cloud offers to download for a client.
    pub fn from_google_json(text: &str) -> anyhow::Result<Self> {
        #[derive(Deserialize)]
        struct Download {
            installed: Option<OAuthClient>,
            web: Option<serde_json::Value>,
        }
        let download: Download = serde_json::from_str(text)
            .context("This is not an OAuth client file from Google Cloud")?;
        match (download.installed, download.web) {
            (Some(client), _) => Ok(client),
            (None, Some(_)) => bail!(
                "This client is a web application. Create an OAuth client of type \"Desktop app\" instead."
            ),
            (None, None) => bail!("This is not an OAuth client file from Google Cloud"),
        }
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = fs::read_to_string(path).with_context(|| {
            format!(
                "No OAuth client at {}. Run `mail auth --client <file>` with the JSON from Google Cloud.",
                path.display()
            )
        })?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Saves the client readable only by you.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct AccessToken {
    pub value: String,
    pub expires_at: Instant,
}

impl AccessToken {
    /// True while the token has more than a minute left.
    pub fn is_fresh(&self) -> bool {
        self.expires_at > Instant::now() + Duration::from_secs(60)
    }
}

pub struct SignIn {
    pub refresh_token: String,
    pub access: AccessToken,
}

/// Runs the browser sign-in. `open_browser` receives the Google URL; Mail
/// passes macOS `open`, tests could pass anything.
pub fn sign_in(
    http: &ureq::Agent,
    client: &OAuthClient,
    open_browser: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<SignIn> {
    // Port 0 lets macOS pick a free port; Google accepts any loopback port
    // for desktop clients.
    let listener = TcpListener::bind("127.0.0.1:0").context("Could not open a local port")?;
    let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr()?.port());

    let verifier = random_token(32);
    let state = random_token(16);
    let url = format!(
        "{AUTH_URL}?{}",
        form_encode(&[
            ("client_id", client.client_id.as_str()),
            ("redirect_uri", &redirect_uri),
            ("response_type", "code"),
            ("scope", &format!("{GMAIL_SCOPE} {PROFILE_SCOPE}")),
            ("code_challenge", &pkce_challenge(&verifier)),
            ("code_challenge_method", "S256"),
            ("state", &state),
            // Ask for a refresh token, and for one again on a repeat sign-in.
            ("access_type", "offline"),
            ("prompt", "consent"),
        ])
    );
    open_browser(&url)?;

    let code = wait_for_redirect(&listener, &state, SIGN_IN_TIMEOUT)?;
    let response = http.post(TOKEN_URL).send_form([
        ("code", code.as_str()),
        ("client_id", &client.client_id),
        ("client_secret", &client.client_secret),
        ("redirect_uri", &redirect_uri),
        ("grant_type", "authorization_code"),
        ("code_verifier", &verifier),
    ])?;
    let tokens = read_token_response(response)?;
    let access = tokens.access();
    let refresh_token = tokens
        .refresh_token
        .context("Google did not return a refresh token. Run `mail auth` again.")?;
    Ok(SignIn {
        refresh_token,
        access,
    })
}

/// Exchanges the refresh token for a new access token.
pub fn refresh(
    http: &ureq::Agent,
    client: &OAuthClient,
    refresh_token: &str,
) -> anyhow::Result<AccessToken> {
    let response = http.post(TOKEN_URL).send_form([
        ("client_id", client.client_id.as_str()),
        ("client_secret", &client.client_secret),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ])?;
    match read_token_response(response) {
        Ok(tokens) => Ok(tokens.access()),
        Err(error) if error.to_string().contains("invalid_grant") => bail!(
            "Google no longer accepts Mail's sign-in. This happens after a password change, \
             six months without use, revoking access, or after 7 days if the Google Cloud app \
             is still in Testing. Run `mail auth` again."
        ),
        Err(error) => Err(error),
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

impl TokenResponse {
    fn access(&self) -> AccessToken {
        AccessToken {
            value: self.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(self.expires_in),
        }
    }
}

fn read_token_response(
    mut response: ureq::http::Response<ureq::Body>,
) -> anyhow::Result<TokenResponse> {
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string()?;
    if status != 200 {
        #[derive(Deserialize)]
        struct TokenError {
            error: String,
            error_description: Option<String>,
        }
        let detail = serde_json::from_str::<TokenError>(&body)
            .map(|e| format!("{}: {}", e.error, e.error_description.unwrap_or_default()))
            .unwrap_or_else(|_| format!("status {status}"));
        bail!("Google sign-in failed ({detail})");
    }
    serde_json::from_str(&body).context("Unexpected token response from Google")
}

/// How long Mail waits for the browser to come back before giving up.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Serves the loopback redirect until Google sends the browser back with a
/// code for this sign-in, or `timeout` passes.
fn wait_for_redirect(
    listener: &TcpListener,
    state: &str,
    timeout: Duration,
) -> anyhow::Result<String> {
    let deadline = Instant::now() + timeout;
    // Poll instead of blocking in accept, so a closed browser tab cannot
    // hold this thread and the port forever.
    listener.set_nonblocking(true)?;
    loop {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("Timed out waiting for the browser to finish signing in. Try again.");
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        // Some platforms hand back a nonblocking socket from a nonblocking
        // listener; reads below rely on the timeout.
        if stream.set_nonblocking(false).is_err()
            || stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .is_err()
        {
            continue;
        }
        // Browsers sometimes open a connection and never send on it; the
        // timeout moves on to the next one instead of waiting forever.
        let Ok(request_line) = read_request(&stream) else {
            continue;
        };

        // A stray connection (preconnect, favicon) may already be gone, so a
        // failed reply to it must not end the sign-in.
        // "GET /?state=...&code=... HTTP/1.1"
        let target = request_line.split_whitespace().nth(1).unwrap_or_default();
        let Some(query) = target.strip_prefix("/?") else {
            let _ = respond(&mut stream, "404 Not Found", "Not found.");
            continue;
        };
        let params = parse_query(query);
        if params.get("state").map(String::as_str) != Some(state) {
            let _ = respond(
                &mut stream,
                "400 Bad Request",
                "This sign-in link has expired.",
            );
            continue;
        }
        if let Some(error) = params.get("error") {
            let _ = respond(
                &mut stream,
                "200 OK",
                "Sign-in was cancelled. You can close this tab.",
            );
            bail!("Google sign-in was cancelled ({error})");
        }
        let Some(code) = params.get("code") else {
            let _ = respond(&mut stream, "400 Bad Request", "Google sent no code.");
            continue;
        };
        // The matching request has its code; a failed page write does not
        // change that.
        let _ = respond(
            &mut stream,
            "200 OK",
            "Mail is signed in. You can close this tab.",
        );
        return Ok(code.clone());
    }
}

/// Returns the request line, after reading the headers so the browser gets a
/// clean response.
fn read_request(stream: &TcpStream) -> std::io::Result<String> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    Ok(request_line)
}

fn respond(stream: &mut TcpStream, status: &str, message: &str) -> std::io::Result<()> {
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>Mail</title>\
         <body style=\"font: 20px -apple-system, sans-serif; margin: 4em; background: #1c1c1e; color: #f2f2f7\">\
         {message}</body>"
    );
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    )
}

fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the system random number generator failed");
    URL_SAFE_NO_PAD.encode(buffer)
}

/// RFC 7636 S256: base64url(SHA-256(verifier)).
fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Encodes everything except RFC 3986 unreserved characters.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (percent_decode(key), percent_decode(value)))
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let hex = (byte == b'%')
            .then(|| text.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (byte, hex) {
            (_, Some(decoded)) => {
                out.push(decoded);
                index += 3;
            }
            (b'+', None) => {
                out.push(b' ');
                index += 1;
            }
            (_, None) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The refresh token, stored in the login Keychain under the account email.
pub mod keychain {
    use security_framework::passwords;

    use super::KEYCHAIN_SERVICE;

    pub fn save(email: &str, refresh_token: &str) -> anyhow::Result<()> {
        passwords::set_generic_password(KEYCHAIN_SERVICE, email, refresh_token.as_bytes())?;
        Ok(())
    }

    pub fn load(email: &str) -> anyhow::Result<String> {
        let bytes = passwords::get_generic_password(KEYCHAIN_SERVICE, email)
            .map_err(|_| anyhow::anyhow!("Mail is not signed in to {email}. Run `mail auth`."))?;
        Ok(String::from_utf8(bytes)?)
    }

    pub fn delete(email: &str) -> anyhow::Result<()> {
        passwords::delete_generic_password(KEYCHAIN_SERVICE, email)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_length_is_valid() {
        // RFC 7636 requires 43 to 128 characters.
        assert_eq!(random_token(32).len(), 43);
    }

    #[test]
    fn query_round_trip() {
        let encoded = form_encode(&[("scope", GMAIL_SCOPE), ("state", "a b&c")]);
        assert_eq!(
            encoded,
            "scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.modify&state=a%20b%26c"
        );
        let decoded = parse_query(&encoded);
        assert_eq!(decoded["scope"], GMAIL_SCOPE);
        assert_eq!(decoded["state"], "a b&c");
        assert_eq!(parse_query("code=4%2F0A+x")["code"], "4/0A x");
    }

    #[test]
    fn loopback_waits_for_the_matching_state() {
        use std::io::Read;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let browser = std::thread::spawn(move || {
            let mut pages = Vec::new();
            for path in [
                "/favicon.ico",
                "/?state=old&code=stale",
                "/?state=s1&code=4%2F0Ab",
            ] {
                let mut stream = TcpStream::connect(address).unwrap();
                write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
                let mut page = String::new();
                stream.read_to_string(&mut page).unwrap();
                pages.push(page);
            }
            pages
        });

        assert_eq!(wait_for_redirect(&listener, "s1", Duration::from_secs(30)).unwrap(), "4/0Ab");
        let pages = browser.join().unwrap();
        assert!(pages[0].starts_with("HTTP/1.1 404"));
        assert!(pages[1].starts_with("HTTP/1.1 400"));
        assert!(pages[2].contains("Mail is signed in"));
    }

    #[test]
    fn loopback_survives_stray_connections_that_vanish() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let browser = std::thread::spawn(move || {
            // Send a request and hang up without reading the reply.
            for _ in 0..100 {
                let mut stream = TcpStream::connect(address).unwrap();
                let _ = write!(stream, "GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n");
                std::thread::sleep(Duration::from_millis(2));
            }
            let mut stream = TcpStream::connect(address).unwrap();
            write!(
                stream,
                "GET /?state=s1&code=ok HTTP/1.1\r\nHost: x\r\n\r\n"
            )
            .unwrap();
        });
        assert_eq!(
            wait_for_redirect(&listener, "s1", Duration::from_secs(30)).unwrap(),
            "ok"
        );
        browser.join().unwrap();
    }

    #[test]
    fn loopback_gives_up_after_the_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let started = Instant::now();
        let error = wait_for_redirect(&listener, "s1", Duration::from_millis(200)).unwrap_err();
        assert!(error.to_string().contains("Timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn reads_desktop_client_file() {
        let client = OAuthClient::from_google_json(
            r#"{"installed":{"client_id":"123.apps.googleusercontent.com","client_secret":"not-secret","redirect_uris":["http://localhost"]}}"#,
        )
        .unwrap();
        assert_eq!(client.client_id, "123.apps.googleusercontent.com");
        assert!(OAuthClient::from_google_json(r#"{"web":{}}"#).is_err());
    }
}
