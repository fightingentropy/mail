//! Leaving a mailing list. A message says how in its `List-Unsubscribe`
//! header (RFC 2369): an address to email, a page to visit, or, with
//! `List-Unsubscribe-Post` (RFC 8058), an address that takes the request in
//! one POST with nothing more to click.
//!
//! The header is the sender's, so it is untrusted: Mail shows where a
//! request would go and waits for you to confirm before any of it happens,
//! posts only to a public HTTPS host, and never follows a redirect.

use std::time::Duration;

use anyhow::{Context, bail, ensure};

use crate::{compose::Draft, model::BodyLink};

/// How a message's sender takes an unsubscribe request, best first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    /// One HTTPS POST to this address; nothing more to do.
    OneClick(String),
    /// An email to the list's unsubscribe address, sent from your account.
    Email(Request),
    /// A page to finish on in your browser.
    Page(String),
}

/// The email a list asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// The longest request body a header may ask Mail to send.
const MAX_BODY: usize = 2_000;

impl Method {
    /// Where the request goes, for the confirmation: a site or an address.
    pub fn destination(&self) -> &str {
        match self {
            Method::OneClick(url) | Method::Page(url) => host(url).unwrap_or(url),
            Method::Email(request) => &request.to,
        }
    }
}

impl Request {
    /// The request as a message to send.
    pub fn draft(&self) -> Draft {
        Draft {
            to: self.to.clone(),
            subject: self.subject.clone(),
            body: self.body.clone(),
            ..Draft::default()
        }
    }
}

/// Whether a `List-Unsubscribe-Post` header offers the one-click request.
pub fn offers_one_click(post: &str) -> bool {
    post.trim()
        .eq_ignore_ascii_case("List-Unsubscribe=One-Click")
}

/// The best way a `List-Unsubscribe` header gives: the one-click address
/// when `one_click` says the sender takes it, else an email, else a page.
pub fn from_headers(list_unsubscribe: &str, one_click: bool) -> Option<Method> {
    // Each target sits in angle brackets; one with a space or a control
    // character in it is not an address.
    let targets: Vec<&str> = list_unsubscribe
        .split('<')
        .skip(1)
        .filter_map(|rest| rest.split_once('>').map(|(target, _)| target.trim()))
        .filter(|target| !target.chars().any(|c| c.is_whitespace() || c.is_control()))
        .collect();
    let starts = |target: &str, scheme: &str| {
        target
            .get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    };
    if one_click && let Some(url) = targets.iter().find(|url| public_host(url).is_some()) {
        return Some(Method::OneClick((*url).to_owned()));
    }
    let email = targets
        .iter()
        .filter(|target| starts(target, "mailto:"))
        .find_map(|target| request(&target["mailto:".len()..]));
    if let Some(request) = email {
        return Some(Method::Email(request));
    }
    targets
        .iter()
        .find(|target| {
            (starts(target, "https://") || starts(target, "http://")) && host(target).is_some()
        })
        .map(|url| Method::Page((*url).to_owned()))
}

/// The unsubscribe page a body links to, for mail whose headers name none:
/// words that link and say "unsubscribe", or an address on a line that does.
pub fn from_body(text: &str, links: &[BodyLink]) -> Option<Method> {
    let says_so = |words: &str| {
        let words = words.to_lowercase();
        words.contains("unsubscribe") || words.contains("opt out") || words.contains("opt-out")
    };
    let is_page = |url: &str| {
        let lower = url.to_ascii_lowercase();
        (lower.starts_with("https://") || lower.starts_with("http://")) && host(url).is_some()
    };
    let linked = links
        .iter()
        .find(|link| is_page(&link.url) && text.get(link.start..link.end).is_some_and(&says_so));
    if let Some(link) = linked {
        return Some(Method::Page(link.url.clone()));
    }
    text.lines()
        .filter(|line| says_so(line))
        .flat_map(str::split_whitespace)
        .map(|word| word.trim_matches(|c: char| "<>()[]\"'.,;".contains(c)))
        .find(|word| is_page(word))
        .map(|url| Method::Page(url.to_owned()))
}

/// Sends the one-click request of RFC 8058. Nothing of yours goes with it:
/// no cookies, no account, only the words the standard asks for. A redirect
/// is not followed, so the request reaches only the host you were shown.
pub fn post_one_click(url: &str) -> anyhow::Result<()> {
    ensure!(
        public_host(url).is_some(),
        "The unsubscribe address isn't a public HTTPS site"
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let response = agent
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send("List-Unsubscribe=One-Click")
        .context("Their server did not answer")?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        bail!("Their server answered {status}");
    }
    Ok(())
}

/// The email a `mailto:` target asks for: `list@x.example?subject=leave`.
fn request(target: &str) -> Option<Request> {
    let (address, query) = target.split_once('?').unwrap_or((target, ""));
    let to = percent_decode(address).trim().to_owned();
    let (local, domain) = to.split_once('@')?;
    let plain = !to
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || ",;<>\"".contains(c));
    if local.is_empty() || !domain.contains('.') || !plain {
        return None;
    }
    let mut subject = String::new();
    let mut body = String::new();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key.eq_ignore_ascii_case("subject") {
            subject = percent_decode(value).replace(['\r', '\n'], " ");
        } else if key.eq_ignore_ascii_case("body") {
            body = percent_decode(value).chars().take(MAX_BODY).collect();
        }
    }
    if subject.trim().is_empty() {
        subject = "unsubscribe".to_owned();
    }
    Some(Request { to, subject, body })
}

/// The host of a web address, without any `user@` or port.
fn host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = match host.strip_prefix('[') {
        Some(_) => host,
        None => host.split(':').next()?,
    };
    (!host.is_empty()).then_some(host)
}

/// The host of an HTTPS address when it names a site on the internet: no
/// IP address, no `user@`, nothing local to this Mac or its network.
fn public_host(url: &str) -> Option<&str> {
    let start = url.get(.."https://".len())?;
    if !start.eq_ignore_ascii_case("https://") {
        return None;
    }
    let authority = url["https://".len()..].split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let host = host(url)?;
    let lower = host.to_ascii_lowercase();
    let numeric = lower.starts_with('[')
        || lower
            .rsplit('.')
            .next()
            .is_some_and(|last| last.chars().all(|c| c.is_ascii_digit()));
    let local = [".local", ".localhost", ".internal", ".lan", ".home.arpa"]
        .iter()
        .any(|suffix| lower.ends_with(suffix));
    let named = lower.contains('.')
        && lower
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    (named && !numeric && !local).then_some(host)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = (bytes[index] == b'%')
            .then(|| text.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                index += 3;
            }
            None => {
                out.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: &str =
        "<mailto:leave@news.example?subject=Leave%20list>, <https://news.example/u/abc?x=1>";

    #[test]
    fn one_click_wins_when_the_sender_offers_it() {
        assert!(offers_one_click(" list-unsubscribe=one-click "));
        assert!(!offers_one_click("something else"));
        assert_eq!(
            from_headers(BOTH, true),
            Some(Method::OneClick("https://news.example/u/abc?x=1".into()))
        );
    }

    #[test]
    fn without_one_click_an_email_comes_before_a_page() {
        let method = from_headers(BOTH, false).unwrap();
        assert_eq!(
            method,
            Method::Email(Request {
                to: "leave@news.example".into(),
                subject: "Leave list".into(),
                body: String::new(),
            })
        );
        assert_eq!(method.destination(), "leave@news.example");
        assert_eq!(
            from_headers("<https://news.example/u/abc>", false),
            Some(Method::Page("https://news.example/u/abc".into()))
        );
        assert_eq!(
            from_headers("<http://news.example/u>", true),
            Some(Method::Page("http://news.example/u".into())),
            "one click needs HTTPS"
        );
    }

    #[test]
    fn an_email_request_gets_a_subject_and_keeps_headers_out() {
        let Some(Method::Email(request)) = from_headers(
            "<mailto:leave@news.example?body=stop%0D%0Aplease&subject=a%0ABcc:x>",
            false,
        ) else {
            panic!("expected an email");
        };
        assert_eq!(request.subject, "a Bcc:x", "no line break reaches a header");
        assert_eq!(request.body, "stop\r\nplease");
        let draft = request.draft();
        assert_eq!(draft.to, "leave@news.example");
        assert!(draft.invalid_addresses().is_empty());

        let Some(Method::Email(request)) = from_headers("<mailto:leave@news.example>", false)
        else {
            panic!("expected an email");
        };
        assert_eq!(request.subject, "unsubscribe");
    }

    #[test]
    fn targets_that_are_not_an_address_or_a_site_are_skipped() {
        for header in [
            "",
            "no brackets",
            "<javascript:alert(1)>",
            "<mailto:nobody>",
            "<mailto:a@b.example,c@d.example>",
            "<file:///etc/passwd>",
            "<https://>",
        ] {
            assert_eq!(from_headers(header, true), None, "{header}");
        }
    }

    #[test]
    fn one_click_only_goes_to_public_sites() {
        for url in [
            "https://127.0.0.1/u",
            "https://[::1]/u",
            "https://localhost/u",
            "https://printer.local/u",
            "https://intranet/u",
            "https://news.example@evil.example/u",
            "http://news.example/u",
        ] {
            assert_eq!(public_host(url), None, "{url}");
            assert!(post_one_click(url).is_err(), "{url}");
        }
        assert_eq!(
            public_host("https://News.Example:8443/u"),
            Some("News.Example")
        );
        // A local one-click address falls back to the next best way.
        assert_eq!(
            from_headers("<https://127.0.0.1/u>, <mailto:leave@news.example>", true),
            Some(Method::Email(Request {
                to: "leave@news.example".into(),
                subject: "unsubscribe".into(),
                body: String::new(),
            }))
        );
    }

    #[test]
    fn a_body_gives_the_page_its_unsubscribe_words_link_to() {
        let text = "Read online\nUnsubscribe here";
        let links = [
            BodyLink {
                start: 0,
                end: 11,
                url: "https://news.example/read".into(),
            },
            BodyLink {
                start: 12,
                end: 28,
                url: "https://news.example/leave".into(),
            },
        ];
        assert_eq!(
            from_body(text, &links),
            Some(Method::Page("https://news.example/leave".into()))
        );
        let method = from_body(
            "Read: https://news.example/read\nUnsubscribe: <https://news.example/out>.",
            &[],
        )
        .unwrap();
        assert_eq!(method, Method::Page("https://news.example/out".into()));
        assert_eq!(method.destination(), "news.example");
        assert_eq!(from_body("See you Friday. https://cafe.example", &[]), None);
    }
}
