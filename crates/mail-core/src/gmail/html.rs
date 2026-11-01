//! Plain text from HTML email, for search and for messages with no text part.
//!
//! This is not a browser: it drops markup, scripts and styles, keeps line
//! breaks where blocks end and decodes entities. That is enough to index and
//! read most mail.

/// Elements whose contents are never text.
const SKIPPED: &[&str] = &["head", "script", "style", "title", "template"];

/// Elements that start or end on their own line.
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "blockquote",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tr",
    "ul",
];

pub fn to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len() / 2);
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
        let Some(close) = rest.find('>') else {
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
        } else if BLOCKS.contains(&name.as_str()) {
            text.push('\n');
        } else if (name == "td" || name == "th") && !text.ends_with([' ', '\n']) {
            text.push(' ');
        }
    }
    push_text(&mut text, rest);
    tidy(&text)
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
    let decoded = decode_entities(raw);
    for (index, word) in decoded.split_whitespace().enumerate() {
        let starts_with_space = index > 0 || decoded.starts_with(char::is_whitespace);
        if starts_with_space && !out.ends_with([' ', '\n']) && !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if decoded.ends_with(char::is_whitespace) && !out.ends_with([' ', '\n']) {
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
    fn drops_preheader_padding() {
        assert_eq!(
            to_text("Sale&zwnj;&nbsp;&#847;&zwnj; <p>now</p>"),
            "Sale\nnow"
        );
    }
}
