//! Synaptic Gate (Filter) — Task 1.2. Keeps or drops each item of an array by a
//! set of conditions evaluated PER ITEM. Dropped items simply disappear from the
//! stream; the output is always a single array `Value` (Axon's array-input
//! convention), so it composes with Merge / Loop / the rest of the list toolkit.
//!
//! Per-item field access is the one thing that separates Filter from IF/Switch.
//! IF resolves a single `value1` expression ONCE against the predecessor and
//! routes the whole item. Filter must test a *different* value for every item, and
//! the engine can't pre-resolve "the current item's field" (config is interpolated
//! once, before execution). So each condition names a `field` — a dot/bracket path
//! RELATIVE to the current item (e.g. `user.age`, `tags[0]`); an empty `field`
//! tests the item itself, which is what you want for a scalar array. The comparison
//! value `value2` is still interpolated normally, because it's constant across
//! items. Operators and combine (AND/OR) logic are shared with IF via
//! `evaluate_condition_typed`, so the two nodes never drift.

use crate::tools::workflow::{evaluate_condition_typed, parse_path_pointer};
use serde_json::Value;

/// Turn the primary input into an item list. An array is the items; a `Null`
/// contributes none; anything else (a bare object or scalar) is a single item —
/// this mirrors Merge's `flatten_items` rather than Loop's aggressive "find any
/// array field" scan, so a nested field is never silently filtered by surprise.
/// `array_path` is the explicit escape hatch for a wrapper like `{ results: [...] }`.
fn to_items(input: &Value, array_path: Option<&str>) -> Vec<Value> {
    if let Some(path) = array_path.map(str::trim).filter(|p| !p.is_empty()) {
        // A path was given: honor it exactly (absent path → nothing to filter).
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

/// Read `field` (a dot/bracket path) out of `item`. An empty path returns the whole
/// item, so scalar arrays (numbers/strings) can be filtered directly. A missing
/// path resolves to `Null`, which the unary operators (`exists`/`empty`) handle.
fn field_value(item: &Value, field: &str) -> Value {
    let f = field.trim();
    if f.is_empty() {
        return item.clone();
    }
    item.pointer(&parse_path_pointer(f))
        .cloned()
        .unwrap_or(Value::Null)
}

/// True if `item` satisfies the configured conditions, combined with AND (`all`,
/// default) or OR (`any`). No conditions → keep everything (an empty filter is a
/// pass-through, matching IF's "no conditions" leniency in spirit).
fn item_matches(item: &Value, conditions: &[Value], combine: &str, default_cs: bool) -> bool {
    if conditions.is_empty() {
        return true;
    }
    let eval = |cond: &Value| -> bool {
        let data_type = cond
            .get("dataType")
            .and_then(|v| v.as_str())
            .unwrap_or("string");
        let op = cond
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("equals");
        let field = cond.get("field").and_then(|v| v.as_str()).unwrap_or("");
        let left = field_value(item, field);
        // value2 arrives already expression-resolved; keep its JSON type.
        let right = cond.get("value2").cloned().unwrap_or(Value::Null);
        let cs = cond
            .get("caseSensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(default_cs);
        evaluate_condition_typed(data_type, op, &left, &right, cs)
    };
    if combine == "any" {
        conditions.iter().any(eval)
    } else {
        conditions.iter().all(eval)
    }
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let array_path = config.get("arrayPath").and_then(|v| v.as_str());
    let items = to_items(input, array_path);

    let conditions = config
        .get("conditions")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let combine = config
        .get("combineOperation")
        .and_then(|v| v.as_str())
        .unwrap_or("all");
    let default_cs = config
        .get("caseSensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    // "matching" (default) keeps items that satisfy the conditions; "notMatching"
    // inverts the gate — a one-flag way to grab the dropped side without rewriting
    // every condition's operator.
    let keep_matching = config.get("keep").and_then(|v| v.as_str()) != Some("notMatching");

    let kept: Vec<Value> = items
        .into_iter()
        .filter(|item| item_matches(item, &conditions, combine, default_cs) == keep_matching)
        .collect();

    Ok(Value::Array(kept))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // One condition: `field` op value2, with an optional dataType (default string).
    fn cond(field: &str, data_type: &str, op: &str, value2: Value) -> Value {
        json!({ "field": field, "dataType": data_type, "operation": op, "value2": value2 })
    }

    fn cfg(conditions: Vec<Value>, extra: Value) -> Value {
        let mut c = json!({ "conditions": { "parameters": conditions } });
        if let (Some(obj), Some(ex)) = (c.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        c
    }

    // Keep items whose numeric `age` is greater than 18. value2 arrives as a string
    // from the UI — evaluate_condition_typed coerces it.
    #[test]
    fn keeps_items_matching_a_numeric_condition() {
        let input = json!([{ "age": 15 }, { "age": 30 }, { "age": 18 }, { "age": 40 }]);
        let out = execute(
            &cfg(vec![cond("age", "number", "gt", json!("18"))], json!({})),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "age": 30 }, { "age": 40 }]));
    }

    // Two conditions combined with ANY (OR): keep if status==active OR vip==true.
    #[test]
    fn combines_conditions_with_any_or() {
        let input = json!([
            { "status": "active", "vip": false },
            { "status": "closed", "vip": true },
            { "status": "closed", "vip": false },
        ]);
        let out = execute(
            &cfg(
                vec![
                    cond("status", "string", "equals", json!("active")),
                    cond("vip", "boolean", "true", Value::Null),
                ],
                json!({ "combineOperation": "any" }),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(
            out,
            json!([{ "status": "active", "vip": false }, { "status": "closed", "vip": true }])
        );
    }

    // Default AND: both conditions must hold.
    #[test]
    fn combines_conditions_with_all_and_by_default() {
        let input = json!([
            { "age": 30, "country": "PH" },
            { "age": 30, "country": "US" },
            { "age": 10, "country": "PH" },
        ]);
        let out = execute(
            &cfg(
                vec![
                    cond("age", "number", "gte", json!("18")),
                    cond("country", "string", "equals", json!("PH")),
                ],
                json!({}),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "age": 30, "country": "PH" }]));
    }

    // keep=notMatching inverts the gate: return exactly the items the filter drops.
    #[test]
    fn not_matching_inverts_the_gate() {
        let input = json!([{ "age": 15 }, { "age": 30 }]);
        let out = execute(
            &cfg(
                vec![cond("age", "number", "gt", json!("18"))],
                json!({ "keep": "notMatching" }),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "age": 15 }]));
    }

    // No conditions → pass everything through unchanged.
    #[test]
    fn no_conditions_passes_all_through() {
        let input = json!([{ "a": 1 }, { "a": 2 }]);
        let out = execute(&cfg(vec![], json!({})), &input).unwrap();
        assert_eq!(out, input);
    }

    // Nested dot-path field access into each item.
    #[test]
    fn reads_nested_field_path() {
        let input = json!([
            { "user": { "role": "admin" } },
            { "user": { "role": "guest" } },
        ]);
        let out = execute(
            &cfg(
                vec![cond("user.role", "string", "equals", json!("admin"))],
                json!({}),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "user": { "role": "admin" } }]));
    }

    // Empty field tests the item itself — filtering a scalar array.
    #[test]
    fn empty_field_tests_scalar_item() {
        let input = json!(["apple", "banana", "avocado"]);
        let out = execute(
            &cfg(
                vec![cond("", "string", "startsWith", json!("a"))],
                json!({}),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!(["apple", "avocado"]));
    }

    // A unary operator (exists) drops items whose field is missing.
    #[test]
    fn exists_operator_drops_items_missing_the_field() {
        let input = json!([{ "email": "a@x.com" }, { "name": "no email" }]);
        let out = execute(
            &cfg(
                vec![cond("email", "string", "exists", Value::Null)],
                json!({}),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "email": "a@x.com" }]));
    }

    // A bare object input is treated as a single item (kept or dropped whole).
    #[test]
    fn bare_object_is_a_single_item() {
        let keep = execute(
            &cfg(vec![cond("ok", "boolean", "true", Value::Null)], json!({})),
            &json!({ "ok": true }),
        )
        .unwrap();
        assert_eq!(keep, json!([{ "ok": true }]));
        let drop = execute(
            &cfg(vec![cond("ok", "boolean", "true", Value::Null)], json!({})),
            &json!({ "ok": false }),
        )
        .unwrap();
        assert_eq!(drop, json!([]));
    }

    // Null input → empty array, never an error.
    #[test]
    fn null_input_yields_empty_array() {
        let out = execute(
            &cfg(vec![cond("a", "string", "exists", Value::Null)], json!({})),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(out, json!([]));
    }

    // arrayPath unwraps a wrapper object like `{ "results": [...] }` before filtering.
    #[test]
    fn array_path_unwraps_a_wrapper_object() {
        let input = json!({ "results": [{ "n": 1 }, { "n": 5 }, { "n": 9 }] });
        let out = execute(
            &cfg(
                vec![cond("n", "number", "gte", json!("5"))],
                json!({ "arrayPath": "results" }),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "n": 5 }, { "n": 9 }]));
    }

    // Case-insensitivity: node-level default off, and a per-condition override.
    #[test]
    fn case_insensitivity_applies_at_node_and_condition_level() {
        let input = json!([{ "s": "HELLO" }, { "s": "world" }]);
        // Node-level caseSensitive=false makes "hello" match "HELLO".
        let node_level = execute(
            &cfg(
                vec![cond("s", "string", "equals", json!("hello"))],
                json!({ "caseSensitive": false }),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(node_level, json!([{ "s": "HELLO" }]));

        // Per-condition override wins over the node default (default here is true).
        let mut c = cond("s", "string", "equals", json!("world"));
        c.as_object_mut()
            .unwrap()
            .insert("caseSensitive".to_string(), json!(false));
        let per_cond = execute(&cfg(vec![c], json!({})), &json!([{ "s": "WORLD" }])).unwrap();
        assert_eq!(per_cond, json!([{ "s": "WORLD" }]));
    }
}
