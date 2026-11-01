//! Unix milliseconds to and from what people and agents read, in local time.

use anyhow::Context;
use jiff::{Timestamp, Zoned, civil::Date, tz::TimeZone};

fn local(millis: i64) -> Zoned {
    Timestamp::from_millisecond(millis)
        .unwrap_or_default()
        .to_zoned(TimeZone::system())
}

/// `Fri 31 Oct 2026 18:02`
pub fn human(millis: i64) -> String {
    local(millis).strftime("%a %-d %b %Y %H:%M").to_string()
}

/// `14:02` today, `31 Oct` earlier this year, `31 Oct 2025` before that.
pub fn short(millis: i64) -> String {
    let then = local(millis);
    let now = Zoned::now();
    let format = if then.date() == now.date() {
        "%H:%M"
    } else if then.year() == now.year() {
        "%-d %b"
    } else {
        "%-d %b %Y"
    };
    then.strftime(format).to_string()
}

/// `31 Oct 2026`, for date ranges.
pub fn day(millis: i64) -> String {
    local(millis).strftime("%-d %b %Y").to_string()
}

/// `2026-10-31T18:02:00+00:00`, for agents.
pub fn iso(millis: i64) -> String {
    local(millis).strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

/// Midnight at the start of a `YYYY-MM-DD` day, local time, in Unix ms.
pub fn day_start(text: &str) -> anyhow::Result<i64> {
    let date: Date = text
        .trim()
        .parse()
        .with_context(|| format!("Expected a date like 2026-10-31, got {text:?}"))?;
    Ok(date
        .to_zoned(TimeZone::system())?
        .timestamp()
        .as_millisecond())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_start_round_trips() {
        let start = day_start("2026-10-31").unwrap();
        assert!(iso(start).starts_with("2026-10-31T00:00:00"));
        assert!(day_start("31/10/2026").is_err());
    }
}
