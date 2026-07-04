use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use std::collections::HashSet;

pub const LEAD_STATUSES: &[&str] = &["Open", "Contacted", "Qualified", "Lost"];

// Field length caps: stop a looping agent from writing megabytes into a record.
pub const MAX_NAME_LEN: usize = 500; // name, title, company, website, ...
pub const MAX_CONTACT_LEN: usize = 200; // email, phone
pub const MAX_TEXT_LEN: usize = 64 * 1024; // notes, body
pub const MAX_TAGS: usize = 50;
pub const MAX_TAG_LEN: usize = 100;

// Cap in minor units (10^13 major units) — keeps cents exactly representable
// in both i64 and the f64 the JSON layer speaks.
const MAX_AMOUNT_MINOR: f64 = 1_000_000_000_000_000.0;
pub const DEAL_STAGES: &[&str] = &[
    "Prospecting",
    "Qualified",
    "Proposal",
    "Negotiation",
    "Won",
    "Lost",
];
pub const ACTIVITY_ENTITY_TYPES: &[&str] = &["lead", "deal", "org"];
pub const ACTIVITY_KINDS: &[&str] = &["note", "call", "email", "meeting", "task", "other"];

pub fn str_field<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

pub fn require_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    str_field(args, key).ok_or_else(|| anyhow::anyhow!("missing required param '{key}'"))
}

pub fn require_non_empty_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    let value = require_str(args, key)?.trim();
    if value.is_empty() {
        return Err(anyhow::anyhow!("param '{key}' cannot be empty"));
    }
    if value.contains('\0') {
        return Err(anyhow::anyhow!(
            "param '{key}' contains an invalid NUL character"
        ));
    }
    Ok(value)
}

pub fn string_opt(args: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_owned()))
            }
        }
        Some(_) => Err(anyhow::anyhow!("param '{key}' must be a string")),
    }
}

pub fn string_patch(args: &Map<String, Value>, key: &str) -> Result<Option<Option<String>>> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(Some(None))
            } else {
                Ok(Some(Some(trimmed.to_owned())))
            }
        }
        Some(_) => Err(anyhow::anyhow!("param '{key}' must be a string or null")),
    }
}

pub fn f64_arg(args: &Map<String, Value>, key: &str) -> Result<Option<f64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("param '{key}' must be a number")),
    }
}

pub fn i64_arg(args: &Map<String, Value>, key: &str) -> Result<Option<i64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("param '{key}' must be an integer")),
    }
}

pub fn bool_arg(args: &Map<String, Value>, key: &str) -> Result<Option<bool>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("param '{key}' must be a boolean")),
    }
}

pub fn page_args(args: &Map<String, Value>) -> (i64, i64) {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(50)
        .clamp(1, 200);
    let offset = args
        .get("offset")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0);
    (limit, offset)
}

pub fn parse_tags(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
}

pub fn tags_json_from_value(value: Option<&Value>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };

    let arr = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("param 'tags' must be an array of strings"))?;

    let mut seen = HashSet::new();
    let mut tags = Vec::new();

    for item in arr {
        let raw = item
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("param 'tags' must contain only strings"))?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let len = trimmed.chars().count();
        if len > MAX_TAG_LEN {
            return Err(anyhow::anyhow!(
                "param 'tags': each tag must be at most {MAX_TAG_LEN} characters (got one with {len}). Use 'notes' for longer text."
            ));
        }
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            tags.push(trimmed.to_owned());
        }
    }

    if tags.len() > MAX_TAGS {
        return Err(anyhow::anyhow!(
            "param 'tags' accepts at most {MAX_TAGS} tags (got {}). Keep tags as a small set of labels.",
            tags.len()
        ));
    }

    Ok(Some(serde_json::to_string(&tags)?))
}

pub fn inject_tags(mut v: Value, tags_json: &str) -> Value {
    if let Value::Object(ref mut map) = v {
        map.insert(
            "tags".to_owned(),
            serde_json::from_str(tags_json).unwrap_or(Value::Array(vec![])),
        );
    }
    v
}

pub fn like(query: &str) -> String {
    format!(
        "%{}%",
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

pub fn validate_choice(value: &str, allowed: &[&str], field: &str) -> Result<()> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "param '{field}' must be one of: {}",
            allowed.join(", ")
        ))
    }
}

pub fn validate_email(field: &str, value: Option<&str>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }

    let Some((local, domain)) = value.split_once('@') else {
        return Err(anyhow::anyhow!(
            "param '{field}' must be a valid email address"
        ));
    };
    if local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || value.contains(char::is_whitespace)
    {
        return Err(anyhow::anyhow!(
            "param '{field}' must be a valid email address"
        ));
    }
    Ok(())
}

/// Separator characters stripped when comparing phone numbers. The SQL twin is
/// the `replace(...)` chain in [`phone_match_sql`] — keep the two in sync.
const PHONE_STRIP_CHARS: &[char] = &[' ', '-', '(', ')', '+', '.'];

/// Strip common separators so `0917-555-1234`, `(0917) 555 1234` and
/// `0917.555.1234` all compare equal. Deliberately does NOT equate national
/// and international prefixes (`0917...` vs `63917...`) — that needs a
/// region database and guessing wrong merges different people.
pub fn normalize_phone(phone: &str) -> String {
    phone
        .chars()
        .filter(|c| !PHONE_STRIP_CHARS.contains(c))
        .collect()
}

/// SQL expression normalizing a phone column the same way [`normalize_phone`]
/// normalizes the input side.
pub fn phone_match_sql(column: &str) -> String {
    format!(
        "replace(replace(replace(replace(replace(replace({column}, ' ', ''), '-', ''), '(', ''), ')', ''), '+', ''), '.', '')"
    )
}

pub fn validate_currency(field: &str, value: &str) -> Result<()> {
    if value.len() == 3 && value.chars().all(|c| c.is_ascii_uppercase()) {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "param '{field}' must be a 3-letter uppercase currency code"
        ))
    }
}

/// Fixed-width UTC storage format. Views compare timestamps lexicographically,
/// so everything comparable must be written in exactly this shape (the SQL
/// twin is `strftime('%Y-%m-%dT%H:%M:%fZ', ...)` in migration 0003).
pub fn format_utc(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Parse any RFC 3339 offset — or a bare `YYYY-MM-DD` date, normalized to
/// midnight UTC — and rewrite it as fixed-format UTC. Timestamps were
/// previously stored verbatim, which made lexicographic comparisons wrong
/// for non-UTC offsets (e.g. `+10:00` sorts after the same instant in UTC).
pub fn parse_rfc3339_utc(field: &str, value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };

    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Ok(Some(format_utc(dt.with_timezone(&Utc))));
    }
    // Agents and humans write plain dates constantly; midnight UTC keeps the
    // fixed-format lexicographic ordering intact.
    if let Ok(date) = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        let dt = date
            .and_hms_opt(0, 0, 0)
            .expect("midnight is always valid")
            .and_utc();
        return Ok(Some(format_utc(dt)));
    }
    Err(anyhow::anyhow!(
        "param '{field}' must be an ISO 8601 / RFC 3339 timestamp or a YYYY-MM-DD date"
    ))
}

/// Dollars (or any major unit) → integer minor units, round-half-even.
pub fn amount_to_minor(field: &str, amount: f64) -> Result<i64> {
    if !amount.is_finite() || amount < 0.0 {
        return Err(anyhow::anyhow!("param '{field}' must be >= 0"));
    }
    let minor = (amount * 100.0).round_ties_even();
    if minor > MAX_AMOUNT_MINOR {
        return Err(anyhow::anyhow!("param '{field}' is too large"));
    }
    Ok(minor as i64)
}

/// Optional decimal amount argument, converted to minor units.
pub fn amount_arg_minor(args: &Map<String, Value>, key: &str) -> Result<Option<i64>> {
    match f64_arg(args, key)? {
        Some(value) => Ok(Some(amount_to_minor(key, value)?)),
        None => Ok(None),
    }
}

pub fn minor_to_amount(minor: i64) -> f64 {
    minor as f64 / 100.0
}

pub fn check_len(field: &str, value: Option<&str>, max: usize) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let len = value.chars().count();
    if len > max {
        return Err(anyhow::anyhow!(
            "param '{field}' is too long: {len} characters (max {max}). Shorten it or split the content across activity entries."
        ));
    }
    Ok(())
}
