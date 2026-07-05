//! Flexible datetime parsing for user- and expression-supplied values.
//!
//! Workflow expressions feed datetime fields whatever shape the upstream node
//! produced: RFC 3339, Sheets/SQL "2026-07-05 09:00:00", US/EU slash dates,
//! month names, 12-hour clocks, RFC 2822, JS `Date.toString()`, or bare Unix
//! timestamps (seconds or milliseconds, string or JSON number). This module
//! reconciles all of them into one of three shapes so API adapters can format
//! exactly what their backend expects.

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, SecondsFormat};
use serde_json::Value;

/// A parsed datetime, preserving how much the input actually specified.
/// Keeping the three shapes distinct matters downstream: a date-only value
/// means an all-day event, and a naive value means operator-local wall clock,
/// not UTC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlexiDateTime {
    /// A calendar date with no time — all-day semantics.
    DateOnly(NaiveDate),
    /// A wall-clock datetime with no offset; interpret in the caller's zone.
    Naive(NaiveDateTime),
    /// An absolute instant with an explicit offset.
    Zoned(DateTime<FixedOffset>),
}

impl FlexiDateTime {
    /// RFC 3339 string for API params that need an absolute instant. Naive
    /// values get `default_offset` (e.g. "+08:00") appended; date-only becomes
    /// that zone's midnight.
    pub fn to_rfc3339(&self, default_offset: &str) -> String {
        match self {
            Self::DateOnly(d) => format!("{}T00:00:00{default_offset}", d.format("%Y-%m-%d")),
            Self::Naive(dt) => format!("{}{default_offset}", dt.format("%Y-%m-%dT%H:%M:%S")),
            Self::Zoned(dt) => dt.to_rfc3339_opts(SecondsFormat::Secs, true),
        }
    }
}

/// Date shapes tried after the fast paths, most to least common. Slash dates
/// are ambiguous ("07/05/2026"): month-first wins when both orders fit, which
/// matches US and Philippine convention; day-first still parses whenever the
/// first number can't be a month. Dotted D.M.Y is the European form.
const DATE_PATTERNS: &[&str] = &[
    "%Y-%m-%d",
    "%Y/%m/%d",
    "%Y.%m.%d",
    "%m/%d/%Y",
    "%d/%m/%Y",
    "%m-%d-%Y",
    "%d-%m-%Y",
    "%d.%m.%Y",
    "%B %d, %Y",
    "%B %d %Y",
    "%b %d, %Y",
    "%b %d %Y",
    "%d %B %Y",
    "%d %b %Y",
    "%d %B, %Y",
    "%d %b, %Y",
    "%Y %B %d",
    "%Y%m%d",
];

/// Time shapes combined with each date pattern. `%.f` also matches an empty
/// fraction, so the first entry covers plain `HH:MM:SS`. There are no
/// bare-hour `%I%p` entries because chrono needs a minute to resolve a time;
/// normalization rewrites "3pm" to "3:00PM" instead.
const TIME_PATTERNS: &[&str] = &[
    "%H:%M:%S%.f",
    "%H:%M",
    "%I:%M:%S %p",
    "%I:%M %p",
    "%I:%M%p",
];

/// Offset-carrying shapes that RFC 3339 parsing rejects: colonless offsets
/// ("+0800"), a space before the offset, and JS `Date.toString()` ("GMT+0800",
/// weekday and parenthetical already stripped by normalization).
const ZONED_PATTERNS: &[&str] = &[
    "%Y-%m-%dT%H:%M:%S%.f%z",
    "%Y-%m-%d %H:%M:%S%.f%z",
    "%Y-%m-%dT%H:%M:%S%.f %z",
    "%Y-%m-%d %H:%M:%S%.f %z",
    "%Y-%m-%dT%H:%M%z",
    "%Y-%m-%d %H:%M %z",
    "%b %d %Y %H:%M:%S GMT%z",
    "%b %d, %Y %H:%M:%S GMT%z",
];

const WEEKDAYS: &[&str] = &[
    "mon", "tue", "tues", "wed", "thu", "thur", "thurs", "fri", "sat", "sun", "monday", "tuesday",
    "wednesday", "thursday", "friday", "saturday", "sunday",
];

/// Parse any reasonable datetime representation. Returns `None` only when the
/// input has no recognizable date in it.
pub fn parse_flexible(input: &str) -> Option<FlexiDateTime> {
    let s = input.trim().trim_matches('"').trim();
    if s.is_empty() {
        return None;
    }

    // RFC 3339, then the Sheets/SQL hybrid that only differs by a space
    // where the 'T' goes ("2026-07-05 09:00:00Z").
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(FlexiDateTime::Zoned(dt));
    }
    if !s.contains('T') && s.contains(' ') {
        if let Ok(dt) = DateTime::parse_from_rfc3339(&s.replacen(' ', "T", 1)) {
            return Some(FlexiDateTime::Zoned(dt));
        }
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(s) {
        return Some(FlexiDateTime::Zoned(dt));
    }

    // Bare Unix timestamp. Non-numeric strings fall through ("2026-07-05"
    // fails the f64 parse; "20260705" parses but sits below the epoch floor
    // and is caught by %Y%m%d later).
    if s.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '+' | '-')) {
        if let Some(f) = s.parse::<f64>().ok().and_then(epoch_to_flexi) {
            return Some(f);
        }
    }

    // Naive ISO with 'T' separator — the workflow UI's native shape.
    for p in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, p) {
            return Some(FlexiDateTime::Naive(dt));
        }
    }

    let norm = pre_normalize(s);
    for p in ZONED_PATTERNS {
        if let Ok(dt) = DateTime::parse_from_str(&norm, p) {
            return Some(FlexiDateTime::Zoned(dt));
        }
    }
    for d in DATE_PATTERNS {
        for t in TIME_PATTERNS {
            for sep in [" ", ", "] {
                if let Ok(dt) = NaiveDateTime::parse_from_str(&norm, &format!("{d}{sep}{t}")) {
                    return Some(FlexiDateTime::Naive(dt));
                }
            }
        }
    }
    for d in DATE_PATTERNS {
        if let Ok(nd) = NaiveDate::parse_from_str(&norm, d) {
            return Some(FlexiDateTime::DateOnly(nd));
        }
    }
    None
}

/// Like [`parse_flexible`] but for raw JSON values: expressions that are a
/// single bare reference preserve the source's JSON type, so a Unix timestamp
/// arrives as a number, not a string.
pub fn parse_flexible_value(v: &Value) -> Option<FlexiDateTime> {
    match v {
        Value::String(s) => parse_flexible(s),
        Value::Number(n) => epoch_to_flexi(n.as_f64()?),
        _ => None,
    }
}

/// Interpret a number as a Unix timestamp. Magnitude decides the unit:
/// >= 1e11 is milliseconds (1973 onward), otherwise seconds. Values below
/// 1e8 seconds (~1973) are rejected as too ambiguous to be a timestamp.
fn epoch_to_flexi(f: f64) -> Option<FlexiDateTime> {
    let secs = if f >= 1e11 { f / 1000.0 } else { f };
    if !(1e8..=4e11).contains(&secs) {
        return None;
    }
    let dt = DateTime::from_timestamp(secs.trunc() as i64, (secs.fract() * 1e9) as u32)?;
    Some(FlexiDateTime::Zoned(dt.fixed_offset()))
}

/// Reduce free-form datetime text to something the strftime patterns can hit:
/// drop parenthesized zone names, a leading weekday, and filler "at"; collapse
/// whitespace; uppercase am/pm so `%p` matches regardless of input case.
fn pre_normalize(s: &str) -> String {
    let mut depth = 0u32;
    let unparenthesized: String = s
        .chars()
        .filter(|&c| {
            match c {
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                _ => return depth == 0,
            }
            false
        })
        .collect();

    let mut tokens: Vec<String> = Vec::new();
    for (i, raw) in unparenthesized.split_whitespace().enumerate() {
        let bare = raw.trim_end_matches(',').to_ascii_lowercase();
        if i == 0 && WEEKDAYS.contains(&bare.as_str()) {
            continue;
        }
        if bare == "at" || bare == "@" {
            continue;
        }
        tokens.push(normalize_ampm(raw));
    }
    // A detached marker after a bare hour ("3 PM") needs the same ":00"
    // treatment normalize_ampm gives the attached form.
    for i in 1..tokens.len() {
        if (tokens[i] == "AM" || tokens[i] == "PM")
            && !tokens[i - 1].is_empty()
            && tokens[i - 1].chars().all(|c| c.is_ascii_digit())
        {
            tokens[i - 1].push_str(":00");
        }
    }
    tokens.join(" ")
}

/// Uppercase an am/pm marker, standalone ("pm", "p.m.") or attached to the
/// time ("3pm", "9:30am"), leaving every other token untouched. Bare hours
/// gain an explicit ":00" minute — chrono can't resolve a time from hour and
/// am/pm alone.
fn normalize_ampm(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    match lower.as_str() {
        "am" | "a.m." | "a.m" => return "AM".into(),
        "pm" | "p.m." | "p.m" => return "PM".into(),
        _ => {}
    }
    if let Some(prefix) = lower.strip_suffix("am").or_else(|| lower.strip_suffix("pm")) {
        if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit() || c == ':') {
            let split = token.len() - 2;
            let marker = token[split..].to_ascii_uppercase();
            let time = &token[..split];
            if time.contains(':') {
                return format!("{time}{marker}");
            }
            return format!("{time}:00{marker}");
        }
    }
    token.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn naive(s: &str) -> FlexiDateTime {
        FlexiDateTime::Naive(NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap())
    }

    fn date(s: &str) -> FlexiDateTime {
        FlexiDateTime::DateOnly(NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap())
    }

    #[test]
    fn rfc3339_and_offset_variants() {
        for s in [
            "2026-07-05T09:00:00Z",
            "2026-07-05T09:00:00+08:00",
            "2026-07-05T09:00:00.123+08:00",
            "2026-07-05 09:00:00Z",
            "2026-07-05 09:00:00+08:00",
            "2026-07-05T09:00:00+0800",
            "2026-07-05 09:00:00 +08:00",
        ] {
            assert!(
                matches!(parse_flexible(s), Some(FlexiDateTime::Zoned(_))),
                "failed: {s}"
            );
        }
    }

    #[test]
    fn rfc2822_and_js_date_tostring() {
        assert!(matches!(
            parse_flexible("Sun, 05 Jul 2026 09:00:00 +0800"),
            Some(FlexiDateTime::Zoned(_))
        ));
        let js = parse_flexible("Sun Jul 05 2026 09:00:00 GMT+0800 (Philippine Standard Time)");
        assert!(matches!(js, Some(FlexiDateTime::Zoned(_))), "got {js:?}");
    }

    #[test]
    fn unix_timestamps_seconds_and_millis() {
        // 2026-07-05T09:00:00+08:00 == 1783213200 UTC seconds
        let secs = parse_flexible("1783213200").unwrap();
        assert_eq!(secs.to_rfc3339("+08:00"), "2026-07-05T01:00:00Z");
        let millis = parse_flexible("1783213200000").unwrap();
        assert_eq!(millis, secs);
        let num = parse_flexible_value(&serde_json::json!(1783213200)).unwrap();
        assert_eq!(num, secs);
    }

    #[test]
    fn naive_iso_and_sheets_style() {
        assert_eq!(parse_flexible("2026-07-05T09:00:00"), Some(naive("2026-07-05T09:00:00")));
        assert_eq!(parse_flexible("2026-07-05T09:00"), Some(naive("2026-07-05T09:00:00")));
        assert_eq!(parse_flexible("2026-07-05 09:00:00"), Some(naive("2026-07-05T09:00:00")));
        assert_eq!(parse_flexible("2026-07-05 09:00"), Some(naive("2026-07-05T09:00:00")));
    }

    #[test]
    fn slash_dates_prefer_month_first_but_accept_day_first() {
        assert_eq!(parse_flexible("07/05/2026"), Some(date("2026-07-05")));
        assert_eq!(parse_flexible("25/12/2026"), Some(date("2026-12-25")));
        assert_eq!(parse_flexible("07/05/2026 3:00 PM"), Some(naive("2026-07-05T15:00:00")));
        assert_eq!(parse_flexible("07/05/2026 15:00"), Some(naive("2026-07-05T15:00:00")));
    }

    #[test]
    fn month_names_and_twelve_hour_clock() {
        assert_eq!(parse_flexible("July 5, 2026"), Some(date("2026-07-05")));
        assert_eq!(parse_flexible("5 July 2026"), Some(date("2026-07-05")));
        assert_eq!(parse_flexible("Jul 5 2026"), Some(date("2026-07-05")));
        assert_eq!(
            parse_flexible("July 5, 2026 3:00 PM"),
            Some(naive("2026-07-05T15:00:00"))
        );
        assert_eq!(
            parse_flexible("July 5, 2026 at 3pm"),
            Some(naive("2026-07-05T15:00:00"))
        );
        assert_eq!(
            parse_flexible("Sunday, July 5, 2026, 9:30am"),
            Some(naive("2026-07-05T09:30:00"))
        );
    }

    #[test]
    fn compact_and_alternative_date_shapes() {
        assert_eq!(parse_flexible("20260705"), Some(date("2026-07-05")));
        assert_eq!(parse_flexible("2026/07/05"), Some(date("2026-07-05")));
        assert_eq!(parse_flexible("05.07.2026"), Some(date("2026-07-05")));
    }

    #[test]
    fn fractional_epoch_keeps_subseconds() {
        let dt = parse_flexible("1783299600.5").unwrap();
        match dt {
            FlexiDateTime::Zoned(z) => assert_eq!(z.nanosecond(), 500_000_000),
            other => panic!("expected zoned, got {other:?}"),
        }
    }

    #[test]
    fn garbage_and_ambiguous_numbers_return_none() {
        assert_eq!(parse_flexible("not a date"), None);
        assert_eq!(parse_flexible(""), None);
        assert_eq!(parse_flexible("42"), None);
        assert_eq!(parse_flexible_value(&serde_json::json!(null)), None);
        assert_eq!(parse_flexible_value(&serde_json::json!([1, 2])), None);
    }

    #[test]
    fn to_rfc3339_applies_default_offset_only_when_naive() {
        assert_eq!(date("2026-07-05").to_rfc3339("+08:00"), "2026-07-05T00:00:00+08:00");
        assert_eq!(
            naive("2026-07-05T09:00:00").to_rfc3339("+08:00"),
            "2026-07-05T09:00:00+08:00"
        );
        let zoned = parse_flexible("2026-07-05T09:00:00-05:00").unwrap();
        assert_eq!(zoned.to_rfc3339("+08:00"), "2026-07-05T09:00:00-05:00");
    }
}
