//! Open-tracking pixels: images a sender puts in a message only to learn
//! that, when and where you opened it. Mail takes them out of a message's
//! HTML before showing it, so remote images can stay on without telling the
//! sender each time you read their mail.
//!
//! Two kinds are removed: remote images that can't be seen (one pixel or
//! smaller, or hidden by their own style), and images served from the
//! open-tracking addresses of common mailing and "read receipt" services.
//! Pictures sent inside the message (`cid:`, `data:`) are never touched: they
//! come from your mailbox, not the sender's server.

use std::borrow::Cow;

use super::html::{attribute, find_ignoring_case, tag_end};

/// Parts of the addresses common services serve their open-tracking image
/// from, lowercase. An image whose address holds one is removed whatever
/// its size.
const TRACKERS: &[&str] = &[
    // Mailing services.
    "list-manage.com/track/open",
    "mandrillapp.com/track/open",
    "sendgrid.net/wf/open",
    "/wf/open?upn=",
    "awstrack.me/i0/",
    "pstmrk.it/open",
    "mjt.lu/oo/",
    "exct.net/open.aspx",
    "rs6.net/on.jsp",
    "hubspotemail.net/e2t/o",
    "track.customer.io/e/o/",
    "via.intercom.io/o/",
    "open.convertkit-mail",
    "linkedin.com/emimp/",
    "google-analytics.com/collect",
    "emltrk.com",
    // Read receipts people add to their own mail.
    "mailtrack.io/trace",
    "r.superhuman.com",
    "t.yesware.com/t/",
    "mailfoogae.appspot.com/t",
    "track.mixmax.com/api/track",
    "mailstat.us/tr",
    "sidekickopen",
    "tracking.cirrusinsight.com",
    "contactmonkey.com/api/v1/tracker",
    "salesloft.com/email_trackers",
    "close.io/email_opened",
    "bl-1.com",
];

/// `html` without its tracking pixels, and how many were taken out.
pub fn strip(html: &str) -> (Cow<'_, str>, usize) {
    let mut out = String::new();
    let mut copied = 0;
    let mut removed = 0;
    let mut from = 0;
    while let Some(found) = find_ignoring_case(&html[from..], "<img") {
        let at = from + found;
        let after = at + "<img".len();
        // "<img", not "<imgx": the name ends here.
        let is_tag = html[after..]
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || c == '/' || c == '>');
        if !is_tag {
            from = after;
            continue;
        }
        let Some(end) = tag_end(&html[at..]) else {
            break;
        };
        from = at + end + 1;
        if is_tracker(&html[at + 1..at + end]) {
            out.push_str(&html[copied..at]);
            copied = from;
            removed += 1;
        }
    }
    if removed == 0 {
        return (Cow::Borrowed(html), 0);
    }
    out.push_str(&html[copied..]);
    (Cow::Owned(out), removed)
}

/// Whether the text inside an `<img …>` describes a tracking pixel.
fn is_tracker(tag: &str) -> bool {
    let Some(source) = attribute(tag, "src") else {
        return false;
    };
    let source = source.trim().to_ascii_lowercase();
    let remote = ["https://", "http://", "//"]
        .iter()
        .any(|scheme| source.starts_with(scheme));
    if !remote {
        return false;
    }
    if TRACKERS.iter().any(|tracker| source.contains(tracker)) {
        return true;
    }
    let style = attribute(tag, "style").unwrap_or_default();
    let declared = |name: &str| declaration(&style, name);
    if declared("display").as_deref() == Some("none")
        || declared("visibility").as_deref() == Some("hidden")
    {
        return true;
    }
    // The style wins over the attribute, as in a browser.
    let side = |name: &str| {
        declared(name)
            .or_else(|| declared(&format!("max-{name}")))
            .or_else(|| attribute(tag, name))
            .and_then(|value| pixels(&value))
    };
    matches!((side("width"), side("height")), (Some(width), Some(height)) if width <= 1.0 && height <= 1.0)
}

/// The value of `name` in an inline style, lowercase, without `!important`.
fn declaration(style: &str, name: &str) -> Option<String> {
    style.split(';').find_map(|pair| {
        let (key, value) = pair.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| {
            value
                .to_ascii_lowercase()
                .replace("!important", "")
                .trim()
                .to_owned()
        })
    })
}

/// A length in pixels: `1`, `1px` or `0`. A percentage or `auto` is not one.
fn pixels(value: &str) -> Option<f32> {
    let value = value.trim().trim_end_matches('/').trim();
    let number = value.strip_suffix("px").unwrap_or(value).trim();
    number
        .parse()
        .ok()
        .filter(|pixels: &f32| pixels.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invisible_remote_images_are_removed() {
        let html = concat!(
            r#"<p>Hi</p><img src="https://shop.example/open?u=7" width="1" height="1">"#,
            r#"<IMG height=0 width=0 SRC='http://shop.example/o.gif'/>"#,
            r#"<img src="https://shop.example/p" style="width:1px; height: 1PX !important">"#,
            r#"<img src="//shop.example/h" style="display:none" width="600" height="80">"#,
            r#"<img src="https://shop.example/v" style="visibility: hidden"><p>Bye</p>"#,
        );
        let (clean, removed) = strip(html);
        assert_eq!(clean, "<p>Hi</p><p>Bye</p>");
        assert_eq!(removed, 5);
    }

    #[test]
    fn known_trackers_are_removed_whatever_their_size() {
        let html = concat!(
            r#"<img src="https://acme.us1.list-manage.com/track/open.php?u=1&id=2" width="600">"#,
            r#"<img alt="" src="https://u123.ct.sendgrid.net/wf/open?upn=abc">"#,
            r#"<img src="https://R.Superhuman.com/abc.gif">"#,
        );
        assert_eq!(strip(html), (Cow::Borrowed(""), 3));
    }

    #[test]
    fn pictures_you_can_see_stay() {
        let html = concat!(
            r#"<img src="https://shop.example/logo.png" width="120" height="40">"#,
            r#"<img src="https://shop.example/banner.png">"#,
            r#"<img src="https://shop.example/rule.png" width="600" height="1">"#,
            r#"<img src="https://shop.example/wide.png" width="100%" height="1">"#,
            r#"<imgx src="https://shop.example/x" width="1" height="1">"#,
        );
        let (clean, removed) = strip(html);
        assert_eq!(removed, 0);
        assert!(matches!(clean, Cow::Borrowed(_)), "nothing is copied");
        assert_eq!(clean, html);
    }

    #[test]
    fn pictures_sent_inside_the_message_stay_even_when_tiny() {
        let html = concat!(
            r#"<img src="cid:dot@shop" width="1" height="1">"#,
            r#"<img src="data:image/gif;base64,R0lGOD" width="1" height="1">"#,
            r#"<img width="1" height="1">"#,
        );
        assert_eq!(strip(html).1, 0);
    }

    #[test]
    fn a_bracket_inside_an_attribute_does_not_end_the_tag() {
        let html = r#"a<img alt="1 > 0" src="https://shop.example/o" width="1" height="1">b"#;
        assert_eq!(strip(html), (Cow::Borrowed("ab"), 1));
        // A tag that never ends is left as it is.
        let open = r#"a<img src="https://shop.example/o" width="1" height="1""#;
        assert_eq!(strip(open).1, 0);
    }

    #[test]
    fn text_around_removed_pixels_keeps_its_characters() {
        let html = "Café — <img src=\"https://shop.example/o\" width=1 height=1> menü";
        assert_eq!(strip(html).0, "Café —  menü");
    }
}
