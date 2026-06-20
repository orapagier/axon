use anyhow::Result;
use chrono::DateTime;
use serde_json::{Map, Value};
use std::collections::HashSet;

pub const LEAD_STATUSES: &[&str] = &["Open", "Contacted", "Qualified", "Lost"];
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
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            tags.push(trimmed.to_owned());
        }
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
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(anyhow::anyhow!(
            "param '{field}' must be a valid email address"
        ));
    }
    Ok(())
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

pub fn validate_rfc3339_opt(field: &str, value: Option<&str>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };

    DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|_| anyhow::anyhow!("param '{field}' must be an ISO 8601 / RFC 3339 timestamp"))
}
