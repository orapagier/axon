//! Sort / Limit / Remove Duplicates — Task 1.5. The last Phase-1 list node, bundled
//! into one because the stages compose so often ("take the top 5 unique, most
//! recent"). It's a small pipeline applied in a fixed, sensible order:
//!
//!   1. Remove Duplicates (`dedupe`) — drop repeat items, keeping the first, by a
//!      key field (or the whole item when no key is given).
//!   2. Sort (`sort`) — order by one or more field rules, each asc/desc and typed
//!      (auto / number / string / date). A blank field sorts the item itself.
//!   3. Limit (`limit`) — keep the first or last N items.
//!
//! Each stage is off by default, so an unconfigured node is a pass-through. Running
//! dedupe → sort → limit yields "top N unique". Input/output follow the array
//! convention (Filter/Aggregate's `to_items` + `arrayPath`); output is the reshaped
//! array.

use crate::tools::workflow::{
    cfg_usize, parse_path_pointer, val_to_datetime, val_to_number, val_to_string,
};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashSet;

/// Turn the primary input into an item list — same convention as the other list
/// nodes (array = items, Null = none, else a single item; `array_path` unwraps a
/// `{ results: [...] }` wrapper).
fn to_items(input: &Value, array_path: Option<&str>) -> Vec<Value> {
    if let Some(path) = array_path.map(str::trim).filter(|p| !p.is_empty()) {
        return match input.pointer(&parse_path_pointer(path)) {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Null) | None => Vec::new(),
            Some(other) => vec![other.clone()],
        };
    }
    match input {
        Value::Array(a) => a.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

/// Read `field` from `item`; a blank path returns the whole item (for scalar arrays).
fn field_or_self(item: &Value, field: &str) -> Value {
    let f = field.trim();
    if f.is_empty() {
        return item.clone();
    }
    item.pointer(&parse_path_pointer(f))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Compare two field values under a type, ascending. Missing (`Null`) values sort
/// last; `desc` is applied by the caller reversing the result.
fn cmp_values(a: &Value, b: &Value, ty: &str) -> Ordering {
    match (a.is_null(), b.is_null()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater, // nulls last (ascending)
        (false, true) => return Ordering::Less,
        _ => {}
    }
    let by_number = |a: &Value, b: &Value| match (val_to_number(a), val_to_number(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less, // parseable numbers before non-numbers
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let by_date = |a: &Value, b: &Value| match (val_to_datetime(a), val_to_datetime(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    match ty {
        "number" => by_number(a, b),
        "date" => by_date(a, b),
        "string" => val_to_string(a).cmp(&val_to_string(b)),
        // auto: numbers if both numeric, else dates if both parse, else strings.
        _ => {
            if val_to_number(a).is_some() && val_to_number(b).is_some() {
                by_number(a, b)
            } else if val_to_datetime(a).is_some() && val_to_datetime(b).is_some() {
                by_date(a, b)
            } else {
                val_to_string(a).cmp(&val_to_string(b))
            }
        }
    }
}

/// Order two items by the ordered sort rules (first non-equal rule decides).
fn compare_items(a: &Value, b: &Value, rules: &[Value]) -> Ordering {
    for rule in rules {
        let field = rule.get("field").and_then(|v| v.as_str()).unwrap_or("");
        let ty = rule.get("type").and_then(|v| v.as_str()).unwrap_or("auto");
        let desc = rule.get("order").and_then(|v| v.as_str()) == Some("desc");
        let av = field_or_self(a, field);
        let bv = field_or_self(b, field);
        let mut ord = cmp_values(&av, &bv, ty);
        if desc {
            ord = ord.reverse();
        }
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// Composite dedupe key: the joined string forms of the key fields, or the whole
/// item's JSON when no fields are given. `\u{1}` can't appear in normal text, so it
/// safely separates multi-field keys.
fn dedupe_key(item: &Value, fields: &[String]) -> String {
    if fields.is_empty() {
        serde_json::to_string(item).unwrap_or_default()
    } else {
        fields
            .iter()
            .map(|f| val_to_string(&field_or_self(item, f)))
            .collect::<Vec<_>>()
            .join("\u{1}")
    }
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let array_path = config.get("arrayPath").and_then(|v| v.as_str());
    let mut items = to_items(input, array_path);

    let enabled = |key: &str| config.get(key).and_then(|v| v.as_bool()).unwrap_or(false);

    // 1. Remove duplicates (keep first occurrence).
    if enabled("dedupe") {
        let fields: Vec<String> = config
            .get("dedupeBy")
            .and_then(|v| v.as_str())
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let mut seen: HashSet<String> = HashSet::new();
        items.retain(|it| seen.insert(dedupe_key(it, &fields)));
    }

    // 2. Sort by the rule list (stable, so equal items keep their order).
    if enabled("sort") {
        let rules = config
            .get("sortRules")
            .and_then(|v| v.get("parameters"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if !rules.is_empty() {
            items.sort_by(|a, b| compare_items(a, b, &rules));
        }
    }

    // 3. Limit to the first or last N (0/unset = no limit).
    if enabled("limit") {
        if let Some(n) = cfg_usize(config, "maxItems").filter(|n| *n > 0) {
            if items.len() > n {
                let keep_last = config.get("keep").and_then(|v| v.as_str()) == Some("last");
                if keep_last {
                    items.drain(0..items.len() - n);
                } else {
                    items.truncate(n);
                }
            }
        }
    }

    Ok(Value::Array(items))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sort_rule(field: &str, order: &str, ty: &str) -> Value {
        json!({ "field": field, "order": order, "type": ty })
    }

    fn cfg(extra: Value) -> Value {
        extra
    }

    // Sort ascending by a numeric field (auto type infers number).
    #[test]
    fn sorts_ascending_by_number() {
        let input = json!([{ "n": 3 }, { "n": 1 }, { "n": 2 }]);
        let out = execute(
            &cfg(json!({ "sort": true, "sortRules": { "parameters": [sort_rule("n", "asc", "auto")] } })),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "n": 1 }, { "n": 2 }, { "n": 3 }]));
    }

    // Sort descending.
    #[test]
    fn sorts_descending() {
        let input = json!([{ "n": 1 }, { "n": 3 }, { "n": 2 }]);
        let out = execute(
            &cfg(json!({ "sort": true, "sortRules": { "parameters": [sort_rule("n", "desc", "number")] } })),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "n": 3 }, { "n": 2 }, { "n": 1 }]));
    }

    // Multi-key sort: primary field ties broken by the second rule.
    #[test]
    fn multi_key_sort_breaks_ties() {
        let input = json!([
            { "team": "b", "score": 5 },
            { "team": "a", "score": 2 },
            { "team": "a", "score": 9 },
        ]);
        let out = execute(
            &cfg(json!({
                "sort": true,
                "sortRules": { "parameters": [
                    sort_rule("team", "asc", "string"),
                    sort_rule("score", "desc", "number"),
                ] }
            })),
            &input,
        )
        .unwrap();
        assert_eq!(
            out,
            json!([
                { "team": "a", "score": 9 },
                { "team": "a", "score": 2 },
                { "team": "b", "score": 5 },
            ])
        );
    }

    // Date-typed sort orders ISO date strings chronologically.
    #[test]
    fn sorts_by_date_type() {
        let input = json!([{ "d": "2024-03-01" }, { "d": "2024-01-15" }, { "d": "2024-02-10" }]);
        let out = execute(
            &cfg(json!({ "sort": true, "sortRules": { "parameters": [sort_rule("d", "asc", "date")] } })),
            &input,
        )
        .unwrap();
        assert_eq!(
            out,
            json!([{ "d": "2024-01-15" }, { "d": "2024-02-10" }, { "d": "2024-03-01" }])
        );
    }

    // Missing sort values fall to the end (ascending).
    #[test]
    fn missing_values_sort_last() {
        let input = json!([{ "n": 2 }, { "x": true }, { "n": 1 }]);
        let out = execute(
            &cfg(json!({ "sort": true, "sortRules": { "parameters": [sort_rule("n", "asc", "number")] } })),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "n": 1 }, { "n": 2 }, { "x": true }]));
    }

    // Limit keeps the first N.
    #[test]
    fn limit_keeps_first_n() {
        let input = json!([1, 2, 3, 4, 5]);
        let out = execute(&cfg(json!({ "limit": true, "maxItems": 2 })), &input).unwrap();
        assert_eq!(out, json!([1, 2]));
    }

    // Limit can keep the last N instead.
    #[test]
    fn limit_keeps_last_n() {
        let input = json!([1, 2, 3, 4, 5]);
        let out = execute(
            &cfg(json!({ "limit": true, "maxItems": 2, "keep": "last" })),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([4, 5]));
    }

    // maxItems unset/0 imposes no limit.
    #[test]
    fn limit_zero_is_no_limit() {
        let input = json!([1, 2, 3]);
        let out = execute(&cfg(json!({ "limit": true, "maxItems": 0 })), &input).unwrap();
        assert_eq!(out, json!([1, 2, 3]));
    }

    // Dedupe by a key field keeps the first occurrence of each key.
    #[test]
    fn dedupe_by_field_keeps_first() {
        let input = json!([
            { "id": 1, "v": "a" },
            { "id": 2, "v": "b" },
            { "id": 1, "v": "c" },
        ]);
        let out = execute(&cfg(json!({ "dedupe": true, "dedupeBy": "id" })), &input).unwrap();
        assert_eq!(out, json!([{ "id": 1, "v": "a" }, { "id": 2, "v": "b" }]));
    }

    // Dedupe with no key field compares the whole item.
    #[test]
    fn dedupe_whole_item() {
        let input = json!([{ "a": 1 }, { "a": 1 }, { "a": 2 }]);
        let out = execute(&cfg(json!({ "dedupe": true })), &input).unwrap();
        assert_eq!(out, json!([{ "a": 1 }, { "a": 2 }]));
    }

    // The pipeline composes: dedupe → sort → limit yields the top N unique.
    #[test]
    fn pipeline_dedupe_sort_limit_top_n_unique() {
        let input = json!([
            { "id": 1, "score": 10 },
            { "id": 2, "score": 50 },
            { "id": 1, "score": 10 }, // duplicate id → dropped
            { "id": 3, "score": 30 },
            { "id": 4, "score": 20 },
        ]);
        let out = execute(
            &cfg(json!({
                "dedupe": true, "dedupeBy": "id",
                "sort": true, "sortRules": { "parameters": [sort_rule("score", "desc", "number")] },
                "limit": true, "maxItems": 2,
            })),
            &input,
        )
        .unwrap();
        assert_eq!(
            out,
            json!([{ "id": 2, "score": 50 }, { "id": 3, "score": 30 }])
        );
    }

    // Nothing enabled → pass the list through untouched.
    #[test]
    fn passthrough_when_nothing_enabled() {
        let input = json!([{ "n": 3 }, { "n": 1 }]);
        let out = execute(&cfg(json!({})), &input).unwrap();
        assert_eq!(out, input);
    }

    // arrayPath unwraps a wrapper object before reshaping.
    #[test]
    fn array_path_unwraps_wrapper() {
        let input = json!({ "rows": [{ "n": 2 }, { "n": 1 }] });
        let out = execute(
            &cfg(json!({
                "arrayPath": "rows",
                "sort": true, "sortRules": { "parameters": [sort_rule("n", "asc", "number")] }
            })),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "n": 1 }, { "n": 2 }]));
    }
}
