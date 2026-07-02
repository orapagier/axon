//! n8n-compatible condition engine, split out of `workflow.rs`. Mirrors
//! n8n's Filter/IF/Switch operator set across every data type with loose
//! coercion. Re-exported by the parent via `pub(crate) use conditions::*;`.

use super::*;

// ── n8n-compatible condition engine ─────────────────────────────────────────
//
// Mirrors n8n's Filter/IF/Switch operator set across every data type
// (string, number, boolean, dateTime, array, object) plus the universal
// existence/emptiness operators. Values arrive already expression-resolved,
// so each side may be any JSON type; we coerce per the chosen data type
// (n8n "loose" type validation) before comparing.

/// Map legacy/aliased operator ids to their canonical n8n id.
pub(crate) fn canonical_op(op: &str) -> &str {
    match op {
        "isEmpty" => "empty",
        "isNotEmpty" => "notEmpty",
        "isTrue" => "true",
        "isFalse" => "false",
        "greater" | "larger" => "gt",
        "less" | "smaller" => "lt",
        "greaterEqual" | "largerEqual" | "greaterThanOrEqual" => "gte",
        "lessEqual" | "smallerEqual" | "lessThanOrEqual" => "lte",
        "matches" => "regex",
        "notMatches" | "doesNotMatch" => "notRegex",
        other => other,
    }
}

pub(crate) fn val_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f.abs() < 1e15 {
                    return (f as i64).to_string();
                }
            }
            n.to_string()
        }
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

pub(crate) fn val_to_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                t.parse::<f64>().ok()
            }
        }
        _ => None,
    }
}

pub(crate) fn val_to_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" | "" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// n8n "empty": null/undefined, "", [], {} are empty.
pub(crate) fn val_is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

pub(crate) fn val_to_datetime(v: &Value) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
    match v {
        Value::Number(n) => {
            let f = n.as_f64()?;
            // Heuristic: large magnitudes are epoch millis, otherwise seconds.
            let (secs, nsecs) = if f.abs() >= 1e11 {
                (
                    (f / 1000.0).trunc() as i64,
                    ((f as i64 % 1000) * 1_000_000) as u32,
                )
            } else {
                (f as i64, 0)
            };
            Utc.timestamp_opt(secs, nsecs)
                .single()
                .map(|dt| dt.fixed_offset())
        }
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return None;
            }
            if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
                return Some(dt);
            }
            if let Ok(dt) = DateTime::parse_from_rfc2822(t) {
                return Some(dt);
            }
            for fmt in [
                "%Y-%m-%dT%H:%M:%S%.f",
                "%Y-%m-%dT%H:%M:%S",
                "%Y-%m-%d %H:%M:%S",
                "%Y-%m-%dT%H:%M",
                "%Y-%m-%d %H:%M",
            ] {
                if let Ok(ndt) = NaiveDateTime::parse_from_str(t, fmt) {
                    return Some(Utc.from_utc_datetime(&ndt).fixed_offset());
                }
            }
            if let Ok(nd) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
                if let Some(ndt) = nd.and_hms_opt(0, 0, 0) {
                    return Some(Utc.from_utc_datetime(&ndt).fixed_offset());
                }
            }
            if let Ok(secs) = t.parse::<i64>() {
                return Utc
                    .timestamp_opt(secs, 0)
                    .single()
                    .map(|dt| dt.fixed_offset());
            }
            None
        }
        _ => None,
    }
}

/// Compile a regex, supporting n8n's `/pattern/flags` form and case-insensitive
/// matching. Returns None on an invalid pattern (treated as no-match).
pub(crate) fn compile_regex(pattern: &str, case_insensitive: bool) -> Option<Regex> {
    let mut pat = pattern.to_string();
    let mut ci = case_insensitive;
    let mut multiline = false;
    let mut dotall = false;
    // /body/flags  →  extract body + flags
    if pat.len() >= 2 && pat.starts_with('/') {
        if let Some(close) = pat.rfind('/') {
            if close > 0 {
                let flags = pat[close + 1..].to_string();
                let body = pat[1..close].to_string();
                if flags
                    .chars()
                    .all(|c| matches!(c, 'i' | 'm' | 's' | 'g' | 'u' | 'y'))
                {
                    ci = ci || flags.contains('i');
                    multiline = flags.contains('m');
                    dotall = flags.contains('s');
                    pat = body;
                }
            }
        }
    }
    regex::RegexBuilder::new(&pat)
        .case_insensitive(ci)
        .multi_line(multiline)
        .dot_matches_new_line(dotall)
        .build()
        .ok()
}

pub(crate) fn values_loosely_equal(a: &Value, b: &Value, case_sensitive: bool) -> bool {
    if a == b {
        return true;
    }
    if let (Some(x), Some(y)) = (val_to_number(a), val_to_number(b)) {
        if (x - y).abs() < f64::EPSILON {
            return true;
        }
    }
    let mut sa = val_to_string(a);
    let mut sb = val_to_string(b);
    if !case_sensitive {
        sa = sa.to_lowercase();
        sb = sb.to_lowercase();
    }
    sa == sb
}

pub(crate) fn num_cmp(a: &Value, b: &Value, f: impl Fn(f64, f64) -> bool) -> bool {
    match (val_to_number(a), val_to_number(b)) {
        (Some(x), Some(y)) => f(x, y),
        _ => false,
    }
}

/// Evaluate a single n8n-style condition. `left` is the tested value, `right`
/// the comparison value (ignored by unary operators).
pub(crate) fn evaluate_condition_typed(
    data_type: &str,
    op_raw: &str,
    left: &Value,
    right: &Value,
    case_sensitive: bool,
) -> bool {
    let op = canonical_op(op_raw);

    // Universal operators — valid for every data type.
    match op {
        "exists" => return !left.is_null(),
        "notExists" => return left.is_null(),
        "empty" => return val_is_empty(left),
        "notEmpty" => return !val_is_empty(left),
        _ => {}
    }

    match data_type {
        "number" => match op {
            "equals" => num_cmp(left, right, |a, b| (a - b).abs() < f64::EPSILON),
            "notEquals" => !num_cmp(left, right, |a, b| (a - b).abs() < f64::EPSILON),
            "gt" => num_cmp(left, right, |a, b| a > b),
            "lt" => num_cmp(left, right, |a, b| a < b),
            "gte" => num_cmp(left, right, |a, b| a >= b),
            "lte" => num_cmp(left, right, |a, b| a <= b),
            _ => false,
        },
        "boolean" => {
            let l = val_to_bool(left);
            match op {
                "true" => l == Some(true),
                "false" => l == Some(false),
                "equals" => l.is_some() && l == val_to_bool(right),
                "notEquals" => l != val_to_bool(right),
                _ => false,
            }
        }
        "dateTime" => {
            let l = val_to_datetime(left);
            let r = val_to_datetime(right);
            match (l, r) {
                (Some(a), Some(b)) => match op {
                    "equals" => a == b,
                    "notEquals" => a != b,
                    "after" => a > b,
                    "before" => a < b,
                    "afterOrEquals" => a >= b,
                    "beforeOrEquals" => a <= b,
                    _ => false,
                },
                // Unparseable on either side: only "notEquals" can be true.
                _ => op == "notEquals",
            }
        }
        "array" => {
            let arr = left.as_array();
            match op {
                "contains" => arr
                    .map(|a| {
                        a.iter()
                            .any(|el| values_loosely_equal(el, right, case_sensitive))
                    })
                    .unwrap_or(false),
                "notContains" => !arr
                    .map(|a| {
                        a.iter()
                            .any(|el| values_loosely_equal(el, right, case_sensitive))
                    })
                    .unwrap_or(false),
                "lengthEquals" | "lengthNotEquals" | "lengthGt" | "lengthLt" | "lengthGte"
                | "lengthLte" => {
                    let len = arr.map(|a| a.len() as f64).unwrap_or(0.0);
                    let r = val_to_number(right).unwrap_or(0.0);
                    match op {
                        "lengthEquals" => (len - r).abs() < f64::EPSILON,
                        "lengthNotEquals" => (len - r).abs() >= f64::EPSILON,
                        "lengthGt" => len > r,
                        "lengthLt" => len < r,
                        "lengthGte" => len >= r,
                        "lengthLte" => len <= r,
                        _ => false,
                    }
                }
                _ => false,
            }
        }
        "object" => false, // only existence/emptiness apply (handled above)
        // string (default)
        _ => {
            if op == "regex" {
                return compile_regex(&val_to_string(right), !case_sensitive)
                    .map(|re| re.is_match(&val_to_string(left)))
                    .unwrap_or(false);
            }
            if op == "notRegex" {
                return !compile_regex(&val_to_string(right), !case_sensitive)
                    .map(|re| re.is_match(&val_to_string(left)))
                    .unwrap_or(false);
            }
            // Numeric comparisons are also offered on string fields (n8n loose).
            match op {
                "gt" => return num_cmp(left, right, |a, b| a > b),
                "lt" => return num_cmp(left, right, |a, b| a < b),
                "gte" => return num_cmp(left, right, |a, b| a >= b),
                "lte" => return num_cmp(left, right, |a, b| a <= b),
                _ => {}
            }
            let mut l = val_to_string(left);
            let mut r = val_to_string(right);
            if !case_sensitive {
                l = l.to_lowercase();
                r = r.to_lowercase();
            }
            match op {
                "equals" => l == r,
                "notEquals" => l != r,
                "contains" => l.contains(&r),
                "notContains" => !l.contains(&r),
                "startsWith" => l.starts_with(&r),
                "notStartsWith" => !l.starts_with(&r),
                "endsWith" => l.ends_with(&r),
                "notEndsWith" => !l.ends_with(&r),
                _ => l == r,
            }
        }
    }
}

#[cfg(test)]
mod condition_tests {
    use super::evaluate_condition_typed as ev;
    use serde_json::json;

    #[test]
    fn string_ops() {
        assert!(ev("string", "equals", &json!("hi"), &json!("hi"), true));
        assert!(!ev("string", "equals", &json!("Hi"), &json!("hi"), true));
        assert!(ev("string", "equals", &json!("Hi"), &json!("hi"), false)); // case-insensitive
        assert!(ev(
            "string",
            "contains",
            &json!("hello world"),
            &json!("lo wo"),
            true
        ));
        assert!(ev(
            "string",
            "notContains",
            &json!("abc"),
            &json!("z"),
            true
        ));
        assert!(ev(
            "string",
            "startsWith",
            &json!("abcdef"),
            &json!("abc"),
            true
        ));
        assert!(ev(
            "string",
            "notStartsWith",
            &json!("abcdef"),
            &json!("xyz"),
            true
        ));
        assert!(ev(
            "string",
            "endsWith",
            &json!("abcdef"),
            &json!("def"),
            true
        ));
        assert!(ev(
            "string",
            "notEndsWith",
            &json!("abcdef"),
            &json!("abc"),
            true
        ));
        assert!(ev(
            "string",
            "regex",
            &json!("user@x.com"),
            &json!(r"^\S+@\S+\.\S+$"),
            true
        ));
        assert!(ev(
            "string",
            "regex",
            &json!("HELLO"),
            &json!("hello"),
            false
        )); // ci regex
        assert!(ev(
            "string",
            "notRegex",
            &json!("abc"),
            &json!(r"^\d+$"),
            true
        ));
        // legacy aliases still work
        assert!(ev("string", "isEmpty", &json!(""), &json!(null), true));
        assert!(ev("string", "isNotEmpty", &json!("x"), &json!(null), true));
    }

    #[test]
    fn number_ops() {
        assert!(ev("number", "equals", &json!(5), &json!("5"), true)); // loose coercion
        assert!(ev("number", "notEquals", &json!(5), &json!(6), true));
        assert!(ev("number", "gt", &json!(10), &json!(3), true));
        assert!(ev("number", "lt", &json!(2), &json!(3), true));
        assert!(ev("number", "gte", &json!(3), &json!(3), true));
        assert!(ev("number", "lte", &json!(3), &json!(4), true));
        // legacy aliases
        assert!(ev("number", "greater", &json!(10), &json!(3), true));
        assert!(ev("number", "lessEqual", &json!(3), &json!(3), true));
    }

    #[test]
    fn boolean_ops() {
        assert!(ev("boolean", "true", &json!(true), &json!(null), true));
        assert!(ev("boolean", "false", &json!(false), &json!(null), true));
        assert!(ev("boolean", "true", &json!("yes"), &json!(null), true)); // coerce
        assert!(ev("boolean", "equals", &json!(true), &json!("true"), true));
        assert!(ev(
            "boolean",
            "notEquals",
            &json!(true),
            &json!(false),
            true
        ));
        // legacy
        assert!(ev("boolean", "isTrue", &json!(1), &json!(null), true));
        assert!(ev("boolean", "isFalse", &json!(0), &json!(null), true));
    }

    #[test]
    fn datetime_ops() {
        let a = json!("2024-01-01T00:00:00Z");
        let b = json!("2024-06-01T00:00:00Z");
        assert!(ev("dateTime", "before", &a, &b, true));
        assert!(ev("dateTime", "after", &b, &a, true));
        assert!(ev(
            "dateTime",
            "equals",
            &json!("2024-01-01"),
            &json!("2024-01-01T00:00:00Z"),
            true
        ));
        assert!(ev("dateTime", "afterOrEquals", &a, &a, true));
        assert!(ev("dateTime", "beforeOrEquals", &a, &b, true));
        // cross-offset equality compares the instant
        assert!(ev(
            "dateTime",
            "equals",
            &json!("2024-01-01T00:00:00+00:00"),
            &json!("2024-01-01T01:00:00+01:00"),
            true
        ));
    }

    #[test]
    fn array_ops() {
        let arr = json!([1, 2, 3]);
        assert!(ev("array", "contains", &arr, &json!(2), true));
        assert!(ev("array", "contains", &arr, &json!("2"), true)); // loose element match
        assert!(ev("array", "notContains", &arr, &json!(9), true));
        assert!(ev("array", "lengthEquals", &arr, &json!(3), true));
        assert!(ev("array", "lengthGt", &arr, &json!(2), true));
        assert!(ev("array", "lengthLte", &arr, &json!(3), true));
        assert!(ev("array", "lengthNotEquals", &arr, &json!(5), true));
    }

    #[test]
    fn universal_ops() {
        assert!(ev("string", "exists", &json!("x"), &json!(null), true));
        assert!(ev("string", "notExists", &json!(null), &json!(null), true));
        assert!(ev("array", "empty", &json!([]), &json!(null), true));
        assert!(ev("object", "empty", &json!({}), &json!(null), true));
        assert!(ev(
            "object",
            "notEmpty",
            &json!({"a": 1}),
            &json!(null),
            true
        ));
        assert!(ev("number", "empty", &json!(null), &json!(null), true));
    }
}
