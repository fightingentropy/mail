//! Plain text from HTML email, for search and reading.
//!
//! This is not a browser: it drops markup, scripts and styles, keeps line
//! breaks where blocks end and decodes entities. That is enough to index and
//! read most mail. Links keep their words in the text and their addresses
//! beside it, as [`BodyLink`]s, so the reader can make "Track your order"
//! clickable without printing a long tracking address.

use crate::model::BodyLink;

/// Where a link's words start and end while the text is built. Private-use
/// characters, removed from the mail's own text, so they can't be forged.
const LINK_START: char = '\u{E000}';
const LINK_END: char = '\u{E001}';

/// Elements whose contents are never text.
const SKIPPED: &[&str] = &["head", "script", "style", "title", "template"];

/// Elements that start and end on a line of their own.
const LINES: &[&str] = &[
    "address", "article", "dd", "div", "dl", "dt", "footer", "header", "li", "section", "tr",
];

/// Elements set apart by an empty line, as browsers give them a margin.
const PARAGRAPHS: &[&str] = &[
    "blockquote",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "ol",
    "p",
    "pre",
    "table",
    "ul",
];

/// The text alone, for search and comparison.
pub fn to_text(html: &str) -> String {
    to_text_with_links(html).0
}

/// The text, and the links among its words.
pub fn to_text_with_links(html: &str) -> (String, Vec<BodyLink>) {
    let mut text = String::with_capacity(html.len() / 2);
    let mut urls = Vec::new();
    let mut in_link = false;
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        push_text(&mut text, &rest[..open]);
        rest = &rest[open..];

        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        // `<` not followed by a tag name, `/` or `!` is text, as in "1 < 2".
        if !rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '/' || c == '!') {
            push_text(&mut text, "<");
            rest = &rest[1..];
            continue;
        }
        let Some(close) = tag_end(rest) else {
            // A lone `<` is text.
            push_text(&mut text, rest);
            rest = "";
            break;
        };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];

        let name = tag_name(tag);
        if SKIPPED.contains(&name.as_str()) && !tag.starts_with('/') && !tag.ends_with('/') {
            let end = format!("</{name}");
            rest = find_ignoring_case(rest, &end)
                .and_then(|start| {
                    rest[start..]
                        .find('>')
                        .map(|close| &rest[start + close + 1..])
                })
                .unwrap_or("");
        } else if name == "a" {
            // Links don't nest; a new one ends any left open.
            if in_link {
                text.push(LINK_END);
                in_link = false;
            }
            if !tag.starts_with('/')
                && let Some(url) = attribute(tag, "href").filter(|url| is_link(url))
            {
                text.push(LINK_START);
                urls.push(url);
                in_link = true;
            }
        } else if name == "br" {
            text.push('\n');
        } else if LINES.contains(&name.as_str()) {
            break_line(&mut text, 1);
        } else if PARAGRAPHS.contains(&name.as_str()) {
            break_line(&mut text, 2);
        } else if (name == "td" || name == "th")
            && !last_visible(&text).is_none_or(|c| c == ' ' || c == '\n')
        {
            text.push(' ');
        }
    }
    push_text(&mut text, rest);
    if in_link {
        text.push(LINK_END);
    }
    let visible: String = text.chars().filter(|&c| !is_invisible(c)).collect();
    let (marked, kept) = attach_markers(&visible);
    let urls: Vec<String> = urls
        .into_iter()
        .zip(kept)
        .filter_map(|(url, kept)| kept.then_some(url))
        .collect();
    extract_links(&tidy(&marked), &urls)
}

/// Where the tag starting at `<` ends: the first `>` outside quotes, since
/// attributes such as `alt="a > b"` may contain one.
fn tag_end(tag: &str) -> Option<usize> {
    let mut quote = None;
    for (index, c) in tag.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), _) if c == open => quote = None,
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

/// The value of `name="..."` (or single-quoted, or bare) inside a tag,
/// with entities decoded.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let start = from + found;
        from = start + name.len();
        // A whole attribute name: "href", not "data-href".
        let before = lower[..start].chars().next_back();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let after = lower[from..].trim_start();
        let Some(value) = after.strip_prefix('=') else {
            continue;
        };
        let value_start = tag.len() - value.trim_start().len();
        let value = &tag[value_start..];
        let raw = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => value[1..].split(quote).next().unwrap_or(""),
            _ => value
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or(""),
        };
        return Some(decode_entities(raw.trim()));
    }
    None
}

/// Web and email links only: a `javascript:` or `file:` link stays words.
fn is_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme) && lower.len() > scheme.len())
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Ends the line, or the paragraph with `newlines` 2, unless the text
/// already does: ten nested `<div>`s make one line break, not ten.
fn break_line(text: &mut String, newlines: usize) {
    if last_visible(text).is_none() {
        return;
    }
    let have = text
        .chars()
        .rev()
        .take_while(|&c| c.is_whitespace() || c == LINK_START || c == LINK_END)
        .filter(|&c| c == '\n')
        .count();
    for _ in have..newlines {
        text.push('\n');
    }
}

/// The last character that will show, skipping link markers.
fn last_visible(text: &str) -> Option<char> {
    text.chars()
        .rev()
        .find(|&c| c != LINK_START && c != LINK_END)
}

/// Moves each link's start onto its first word and its end onto its last,
/// so no marker sits alone on a line or holds a space. A link with no words,
/// such as an image, is dropped. Returns the text and, for each link in
/// order, whether it was kept.
fn attach_markers(text: &str) -> (String, Vec<bool>) {
    let mut out = String::with_capacity(text.len());
    let mut kept = Vec::new();
    // Just after the last character that shows.
    let mut after_visible = 0;
    let mut start_waiting = false;
    for c in text.chars() {
        match c {
            LINK_START => {
                start_waiting = true;
                kept.push(false);
            }
            // No words came between the start and the end.
            LINK_END if start_waiting => start_waiting = false,
            LINK_END => {
                out.insert(after_visible, LINK_END);
                after_visible += LINK_END.len_utf8();
            }
            c if c.is_whitespace() => out.push(c),
            c => {
                if std::mem::take(&mut start_waiting) {
                    out.push(LINK_START);
                    if let Some(last) = kept.last_mut() {
                        *last = true;
                    }
                }
                out.push(c);
                after_visible = out.len();
            }
        }
    }
    (out, kept)
}

/// Takes the markers out, turning each pair into a link over the words
/// between them. The n-th start marker belongs to the n-th address.
fn extract_links(marked: &str, urls: &[String]) -> (String, Vec<BodyLink>) {
    let mut text = String::with_capacity(marked.len());
    let mut links = Vec::new();
    let mut started = 0;
    let mut open: Option<(usize, usize)> = None;
    for c in marked.chars() {
        match c {
            LINK_START => {
                open = Some((text.len(), started));
                started += 1;
            }
            LINK_END => {
                if let Some((start, index)) = open.take()
                    && start < text.len()
                    && let Some(url) = urls.get(index)
                {
                    links.push(BodyLink {
                        start,
                        end: text.len(),
                        url: url.clone(),
                    });
                }
            }
            c => text.push(c),
        }
    }
    (text, links)
}

/// Lowercase element name of the text inside `<...>`, without a leading `/`.
fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn find_ignoring_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack.to_ascii_lowercase().find(needle)
}

/// Source whitespace collapses to single spaces, as in a browser.
fn push_text(out: &mut String, raw: &str) {
    if raw.is_empty() {
        return;
    }
    let decoded: String = decode_entities(raw)
        .chars()
        .filter(|&c| c != LINK_START && c != LINK_END)
        .collect();
    let needs_space = |out: &String| !last_visible(out).is_none_or(|c| c == ' ' || c == '\n');
    for (index, word) in decoded.split_whitespace().enumerate() {
        let starts_with_space = index > 0 || decoded.starts_with(char::is_whitespace);
        if starts_with_space && needs_space(out) {
            out.push(' ');
        }
        out.push_str(word);
    }
    if decoded.ends_with(char::is_whitespace) && needs_space(out) {
        out.push(' ');
    }
}

/// Trims each line, drops invisible characters and allows at most one blank
/// line in a row.
pub(crate) fn tidy(text: &str) -> String {
    let visible: String = text.chars().filter(|&c| !is_invisible(c)).collect();
    let mut out = String::with_capacity(visible.len());
    let mut blank_lines = 0;
    for line in visible.lines().map(str::trim) {
        if line.is_empty() {
            blank_lines += 1;
            if blank_lines > 1 {
                continue;
            }
        } else {
            blank_lines = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_owned()
}

/// Decodes `&amp;`, `&#39;`, `&#x2019;` and the named entities common in mail.
/// Unknown entities stay as written.
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let decoded = rest[1..]
            .find(';')
            .filter(|&end| end <= 10)
            .and_then(|end| entity(&rest[1..end + 1]).map(|text| (text, end + 2)));
        match decoded {
            Some((text, used)) => {
                out.push_str(&text);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<String> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        let character = char::from_u32(code)?;
        return Some(if is_invisible(character) {
            String::new()
        } else {
            character.to_string()
        });
    }
    let text = match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "ndash" => "–",
        "mdash" => "—",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "hellip" => "…",
        "bull" => "•",
        "middot" => "·",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "euro" => "€",
        "pound" => "£",
        "zwnj" | "zwj" | "shy" => "",
        _ => return None,
    };
    Some(text.to_owned())
}

/// Zero-width characters that newsletters repeat to pad their preview text.
pub(crate) fn is_invisible(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}' | '\u{034F}' | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_text_and_block_breaks() {
        let html = "<html><head><title>x</title><style>p { color: red }</style></head>
            <body><p>Hello&nbsp;<b>Ana</b>,</p><p>Your order   ships today.</p>
            <script>alert(1)</script><!-- tracking --><div>Thanks&#33;</div></body></html>";
        assert_eq!(
            to_text(html),
            "Hello Ana,\n\nYour order ships today.\n\nThanks!"
        );
    }

    #[test]
    fn table_cells_are_separated() {
        assert_eq!(
            to_text("<table><tr><td>Total</td><td>&pound;12</td></tr></table>"),
            "Total £12"
        );
    }

    #[test]
    fn entities() {
        assert_eq!(
            decode_entities("Tom &amp; Jerry&#39;s &#x2019; &bogus; a&b"),
            "Tom & Jerry's ’ &bogus; a&b"
        );
    }

    #[test]
    fn stray_angle_brackets_are_text() {
        assert_eq!(to_text("1 < 2 and 3 > 2"), "1 < 2 and 3 > 2");
    }

    #[test]
    fn link_words_keep_their_address_beside_the_text() {
        let html = r#"<p>Hi Ana, <a href="https://click.example/t?a=1&amp;b=2">Track your order</a>.</p>
            <p><a href="javascript:alert(1)">Not a link</a> and <a href='mailto:help@shop.example'>write to us</a></p>"#;
        let (text, links) = to_text_with_links(html);
        assert_eq!(
            text,
            "Hi Ana, Track your order.\n\nNot a link and write to us"
        );
        let found: Vec<(&str, &str)> = links
            .iter()
            .map(|link| (&text[link.start..link.end], link.url.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("Track your order", "https://click.example/t?a=1&b=2"),
                ("write to us", "mailto:help@shop.example")
            ]
        );
    }

    #[test]
    fn image_links_and_cards_leave_no_stray_lines() {
        let html = r#"<a href="https://shop.example/"><img src="logo.png"></a>
            <a href="https://shop.example/job/1"><div>Senior Developer</div><div>London</div></a>
            <p>Bye</p>"#;
        let (text, links) = to_text_with_links(html);
        assert_eq!(text, "Senior Developer\nLondon\n\nBye");
        assert_eq!(links.len(), 1);
        assert_eq!(
            &text[links[0].start..links[0].end],
            "Senior Developer\nLondon"
        );
        assert_eq!(links[0].url, "https://shop.example/job/1");
    }

    #[test]
    fn a_closing_bracket_inside_quotes_stays_in_the_tag() {
        assert_eq!(
            to_text(r#"<p>Add us <a href="mailto:hi@shop.example" title="a > b">here</a></p>"#),
            "Add us here"
        );
    }

    #[test]
    fn drops_preheader_padding() {
        assert_eq!(
            to_text("Sale&zwnj;&nbsp;&#847;&zwnj; <p>now</p>"),
            "Sale\n\nnow"
        );
    }
}
