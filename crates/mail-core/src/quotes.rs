//! Finds where a reply stops being new and starts repeating the message it
//! answers, so the reader can fold that part away. In a conversation every
//! reply carries the ones before it, and each of those is already on screen.
//!
//! This reads the text Mail stores, not the HTML, so it knows quoting by its
//! marks: an "On … wrote:" line, lines starting with `>`, or the block of
//! From, Sent and Subject lines Outlook puts above the original. It folds
//! only when it is sure. A reply written between the quoted lines stays
//! whole, as does a forwarded message, where the quoted part is the point.

/// How "On 4 Nov 2026, Ana wrote:" ends, in the languages mail clients
/// write it in.
const WROTE: &[&str] = &[
    "wrote:",
    "writes:",
    "a écrit :",
    "a écrit:",
    "schrieb:",
    "escribió:",
    "ha scritto:",
    "escreveu:",
    "schreef:",
];

/// Labels Outlook starts the lines above an original with.
const FROM: &[&str] = &["From:", "Von:", "De:", "De :", "Da:", "Van:"];
const SENT: &[&str] = &[
    "Sent:",
    "Date:",
    "Gesendet:",
    "Envoyé:",
    "Envoyé :",
    "Enviado:",
    "Inviato:",
    "Verzonden:",
];
const ABOUT: &[&str] = &[
    "Subject:", "To:", "Betreff:", "An:", "Objet:", "Objet :", "Asunto:", "Para:", "Oggetto:",
];

/// The byte in `text` where the quoted earlier message starts, if the
/// message ends with one and has words of its own before it. `subject` is
/// the message's own: a forward is never folded.
pub fn quoted_from(subject: &str, text: &str) -> Option<usize> {
    let subject = subject.trim_start().to_ascii_lowercase();
    if subject.starts_with("fw:") || subject.starts_with("fwd:") {
        return None;
    }
    let mut lines = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        lines.push(Line {
            start: offset,
            text: line.trim(),
        });
        offset += line.len();
    }
    if lines.iter().any(|line| is_forward_mark(line.text)) {
        return None;
    }
    let start = (0..lines.len())
        .find_map(|index| starts_quote(&lines, index))
        .or_else(|| trailing_quote(&lines))?;
    let own = &text[..lines[start].start];
    (!own.trim().is_empty()).then_some(lines[start].start)
}

struct Line<'a> {
    /// Where the line starts in the text, in bytes.
    start: usize,
    /// The line without the spaces around it.
    text: &'a str,
}

fn is_quoted(line: &str) -> bool {
    line.starts_with('>')
}

/// "---------- Forwarded message ---------" or "Begin forwarded message:".
fn is_forward_mark(line: &str) -> bool {
    line.to_ascii_lowercase().contains("forwarded message")
}

/// The line the quote starts on, if the line at `index` opens one that runs
/// to the end of the message.
fn starts_quote(lines: &[Line], index: usize) -> Option<usize> {
    let line = lines[index].text;
    if let Some(last) = attribution_end(lines, index) {
        return runs_to_the_end(&lines[last + 1..]).then_some(index);
    }
    if is_separator(line) {
        return Some(index);
    }
    if is_header_block(lines, index) {
        // The rule Outlook draws above the block goes with it.
        let above = lines[..index]
            .iter()
            .rposition(|line| !line.text.is_empty())
            .filter(|&above| is_rule(lines[above].text));
        return Some(above.unwrap_or(index));
    }
    None
}

/// A line of underscores or dashes.
fn is_rule(line: &str) -> bool {
    line.len() >= 5 && line.chars().all(|c| c == '_' || c == '-')
}

/// The last line of an "On … wrote:" that starts at `index`. Mail clients
/// wrap a long one over two or three lines.
fn attribution_end(lines: &[Line], index: usize) -> Option<usize> {
    let mut joined = String::new();
    for (offset, line) in lines[index..].iter().take(3).enumerate() {
        if line.text.is_empty() || is_quoted(line.text) {
            return None;
        }
        if offset > 0 {
            joined.push(' ');
        }
        joined.push_str(line.text);
        if joined.chars().count() > 300 {
            return None;
        }
        if WROTE.iter().any(|wrote| joined.ends_with(wrote)) {
            // A date or an address, so "On reflection, he wrote:" in a
            // letter is not taken for one.
            let dated = joined.chars().any(|c| c.is_ascii_digit()) || joined.contains('@');
            let english = joined.ends_with("wrote:") || joined.ends_with("writes:");
            let opens =
                !english || joined.starts_with("On ") || offset == 0 && joined.contains('@');
            return (dated && opens).then_some(index + offset);
        }
    }
    None
}

/// Whether everything after an attribution is the quote: every line marked
/// with `>`, or none of them (text made from HTML has no marks). Marked and
/// unmarked lines together are a reply written between the quoted lines.
fn runs_to_the_end(rest: &[Line]) -> bool {
    let written: Vec<&Line> = rest.iter().filter(|line| !line.text.is_empty()).collect();
    let quoted = written.iter().filter(|line| is_quoted(line.text)).count();
    quoted == 0 || quoted == written.len()
}

/// "-----Original Message-----", which Outlook writes above a quote.
fn is_separator(line: &str) -> bool {
    line.starts_with("---") && line.to_ascii_lowercase().contains("original message")
}

/// Outlook's block above an original: a From line, then Sent or Date and
/// Subject or To within the next few lines.
fn is_header_block(lines: &[Line], index: usize) -> bool {
    let starts = |line: &str, labels: &[&str]| labels.iter().any(|label| line.starts_with(label));
    if !starts(lines[index].text, FROM) {
        return false;
    }
    let next: Vec<&str> = lines[index + 1..]
        .iter()
        .take(5)
        .map(|line| line.text)
        .collect();
    next.iter().any(|line| starts(line, SENT)) && next.iter().any(|line| starts(line, ABOUT))
}

/// Quoted lines at the very end with no "wrote:" above them, two or more.
fn trailing_quote(lines: &[Line]) -> Option<usize> {
    let mut first = None;
    let mut quoted = 0;
    for (index, line) in lines.iter().enumerate().rev() {
        if is_quoted(line.text) {
            first = Some(index);
            quoted += 1;
        } else if !line.text.is_empty() {
            break;
        }
    }
    first.filter(|_| quoted >= 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The part of `text` that stays in view.
    fn own(text: &str) -> &str {
        match quoted_from("Re: Plans", text) {
            Some(at) => text[..at].trim_end(),
            None => text,
        }
    }

    #[test]
    fn a_reply_keeps_its_words_and_folds_the_marked_quote() {
        let text = "Works for me. Ship it.\n\nOn Tue, 4 Nov 2026 at 10:02, Sam Okafor <sam@build.example> wrote:\n\n> Should refused changes surface somewhere?\n>\n> Sam";
        assert_eq!(own(text), "Works for me. Ship it.");
    }

    #[test]
    fn mail_made_from_html_has_no_marks_under_the_wrote_line() {
        let text = "Sounds good, see you then.\n\nOn 14 Nov 2026, at 09:12, Maya Chen wrote:\n\nThere's a new Georgian place near Borough Market.\n\n7:30?";
        assert_eq!(own(text), "Sounds good, see you then.");
    }

    #[test]
    fn a_wrote_line_wrapped_over_two_lines_still_counts() {
        let text = "Thanks!\n\nOn Tue, 4 Nov 2026 at 10:02, Sam Okafor\n<sam@build.example> wrote:\n> One question.";
        assert_eq!(own(text), "Thanks!");
        let other = "Merci !\n\nLe mar. 4 nov. 2026 à 10:02, Ana Lima <ana@example.org> a écrit :\n> Une question.";
        assert_eq!(own(other), "Merci !");
    }

    #[test]
    fn a_reply_written_between_the_quoted_lines_stays_whole() {
        let text = "On Tue, 4 Nov 2026 at 10:02, Sam <sam@build.example> wrote:\n> Is Tuesday fine?\nYes.\n> And the venue?\nStill looking.";
        assert_eq!(quoted_from("Re: Plans", text), None);
        // Top-posted with answers below too.
        let mixed = format!("See below.\n\n{text}");
        assert_eq!(quoted_from("Re: Plans", &mixed), None);
    }

    #[test]
    fn outlooks_from_and_sent_lines_start_the_quote() {
        let text = "Approved.\n\n________________________________\nFrom: Sam Okafor <sam@build.example>\nSent: 04 November 2026 10:02\nTo: Alex Rivera\nSubject: Code review\n\nLooked through the queue.";
        assert_eq!(own(text), "Approved.");
        let marked = "Approved.\n\n-----Original Message-----\nFrom: Sam\nSent: Tuesday\nSubject: Code review\n\nLooked through the queue.";
        assert_eq!(own(marked), "Approved.");
    }

    #[test]
    fn a_forward_is_never_folded() {
        let gmail = "FYI\n\n---------- Forwarded message ---------\nFrom: Sam <sam@build.example>\nDate: Tue, 4 Nov 2026 at 10:02\nSubject: Code review\nTo: Alex\n\nLooked through the queue.";
        assert_eq!(quoted_from("Fwd: Code review", gmail), None);
        assert_eq!(
            quoted_from("Code review", gmail),
            None,
            "the mark is enough"
        );
        let outlook =
            "FYI\n\nFrom: Sam\nSent: Tuesday\nSubject: Code review\n\nLooked through the queue.";
        assert_eq!(quoted_from("FW: Code review", outlook), None);
        assert!(quoted_from("RE: Code review", outlook).is_some());
    }

    #[test]
    fn a_message_that_is_only_a_quote_shows_it() {
        let text =
            "On Tue, 4 Nov 2026 at 10:02, Sam <sam@build.example> wrote:\n> Is Tuesday fine?";
        assert_eq!(quoted_from("Re: Plans", text), None);
        assert_eq!(quoted_from("Re: Plans", ""), None);
    }

    #[test]
    fn quoted_lines_at_the_end_fold_without_a_wrote_line() {
        let text = "Yes to both.\n\n> Is Tuesday fine?\n> And the venue?\n";
        assert_eq!(own(text), "Yes to both.");
        // One quoted line at the end is part of the reply.
        let one = "Yes.\n> Is Tuesday fine?";
        assert_eq!(quoted_from("Re: Plans", one), None);
    }

    #[test]
    fn a_letter_that_only_sounds_like_a_quote_stays_whole() {
        let text = "On reflection, he wrote:\nI was wrong about the venue.";
        assert_eq!(quoted_from("Venue", text), None);
        let from = "From: the top of the hill you can see the sea.\nIt was lovely.";
        assert_eq!(quoted_from("Holiday", from), None);
    }

    #[test]
    fn the_fold_lands_on_a_character_boundary() {
        let text = "Café at 3 — né?\n\nOn 4 Nov 2026, Zoë wrote:\n> Où ?";
        let at = quoted_from("Re: Café", text).unwrap();
        assert!(text.is_char_boundary(at));
        assert!(text[at..].starts_with("On 4 Nov"));
    }
}
