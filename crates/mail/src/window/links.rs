//! Finds web and email links in plain-text bodies so the reader can make
//! them clickable. Only `http`, `https` and `mailto` count: mail is untrusted
//! input, and a `file:` or `javascript:` link in a body stays plain text.

use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Where the link is in the text, in bytes.
    pub range: Range<usize>,
    pub url: String,
}

const SCHEMES: &[&str] = &["https://", "http://", "mailto:"];

/// Every link in `text`, in order.
pub fn find(text: &str) -> Vec<Link> {
    // ASCII lowercasing keeps every byte offset, so matches map straight back.
    let lower = text.to_ascii_lowercase();
    let mut links = Vec::new();
    let mut previous: Option<char> = None;
    let mut resume = 0;
    for (start, c) in text.char_indices() {
        let at_word_start = !previous.is_some_and(char::is_alphanumeric);
        previous = Some(c);
        if start < resume || !at_word_start {
            continue;
        }
        let Some(scheme) = SCHEMES
            .iter()
            .find(|scheme| lower[start..].starts_with(*scheme))
        else {
            continue;
        };
        let end = link_end(text, start);
        let url = &text[start..end];
        // Trimming can shorten a bare "mailto:" below its own scheme.
        let rest = url.get(scheme.len()..);
        if rest.is_some_and(|rest| has_destination(rest, scheme)) {
            links.push(Link {
                range: start..end,
                url: url.to_owned(),
            });
            resume = end;
        }
    }
    links
}

/// True for a link Mail is willing to hand to macOS: a web or email address
/// with no spaces or control characters.
pub fn is_openable(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    SCHEMES
        .iter()
        .any(|scheme| lower.starts_with(scheme) && has_destination(&url[scheme.len()..], scheme))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A link runs to the next space or delimiter, minus punctuation that ends
/// the sentence around it, as in "see https://example.com/a." or
/// "(https://example.com/b)".
fn link_end(text: &str, start: usize) -> usize {
    let rest = &text[start..];
    let mut end = rest
        .find(|c: char| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | '`'))
        .unwrap_or(rest.len());
    loop {
        let url = &rest[..end];
        let Some(last) = url.chars().next_back() else {
            break;
        };
        let unbalanced = |open: char| url.matches(open).count() < url.matches(last).count();
        let trim = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' => true,
            ')' => unbalanced('('),
            ']' => unbalanced('['),
            '}' => unbalanced('{'),
            _ => false,
        };
        if !trim {
            break;
        }
        end -= last.len_utf8();
    }
    start + end
}

/// A web link needs a host and an email link an address, so a bare
/// "https://" or "mailto:" is left alone.
fn has_destination(rest: &str, scheme: &str) -> bool {
    if scheme == "mailto:" {
        let address = rest.split('?').next().unwrap_or_default();
        return address
            .split_once('@')
            .is_some_and(|(user, domain)| !user.is_empty() && !domain.is_empty());
    }
    rest.chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '[')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<String> {
        find(text).into_iter().map(|link| link.url).collect()
    }

    #[test]
    fn finds_web_and_email_links() {
        assert_eq!(
            urls("Book at https://rail.example/b?id=7 or write to mailto:help@rail.example today"),
            ["https://rail.example/b?id=7", "mailto:help@rail.example"]
        );
        assert_eq!(urls("http://old.example"), ["http://old.example"]);
        assert_eq!(urls("HTTPS://LOUD.EXAMPLE/X"), ["HTTPS://LOUD.EXAMPLE/X"]);
    }

    #[test]
    fn sentence_punctuation_is_not_part_of_the_link() {
        assert_eq!(
            urls("See https://a.example/page. Then https://b.example, and https://c.example!"),
            [
                "https://a.example/page",
                "https://b.example",
                "https://c.example"
            ]
        );
        assert_eq!(
            urls("(details: https://a.example/x)"),
            ["https://a.example/x"]
        );
        assert_eq!(urls("<https://a.example/y>"), ["https://a.example/y"]);
        assert_eq!(urls("\"https://a.example/z\""), ["https://a.example/z"]);
    }

    #[test]
    fn balanced_brackets_stay_in_the_link() {
        assert_eq!(
            urls("https://wiki.example/Rust_(language) is the page"),
            ["https://wiki.example/Rust_(language)"]
        );
        assert_eq!(
            urls("[docs](https://docs.example/a)"),
            ["https://docs.example/a"]
        );
    }

    #[test]
    fn ignores_what_is_not_a_link() {
        assert!(urls("xhttps://inside.example").is_empty());
        assert!(urls("just https:// and mailto: alone").is_empty());
        assert!(urls("file:///etc/passwd javascript:alert(1)").is_empty());
        assert!(urls("ftp://files.example").is_empty());
    }

    #[test]
    fn ranges_are_byte_offsets_into_the_text() {
        let text = "Café menü — https://café.example/menü, ok";
        let links = find(text);
        assert_eq!(links.len(), 1);
        assert_eq!(&text[links[0].range.clone()], "https://café.example/menü");
    }

    #[test]
    fn links_on_separate_lines_are_separate() {
        assert_eq!(
            urls("https://a.example\nhttps://b.example"),
            ["https://a.example", "https://b.example"]
        );
    }

    #[test]
    fn only_web_and_email_links_may_open() {
        assert!(is_openable("https://rail.example/b"));
        assert!(is_openable("mailto:help@rail.example"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("file:///Applications/Calculator.app"));
        assert!(!is_openable("https://a.example/x y"));
        assert!(!is_openable("https://"));
    }
}
