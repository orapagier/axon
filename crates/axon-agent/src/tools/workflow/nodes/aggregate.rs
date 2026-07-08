//! Summation (Aggregate) — Task 1.3. Rolls an array of items into ONE summary
//! item by reducing one or more fields. It's the complement of Loop/Split Out:
//! where Filter keeps the list shape and Merge joins two lists, Aggregate collapses
//! a list down to a single object like `{ total: 42, emails: [...] }`.
//!
//! Output is a bare object (one item), because a reducer's result is naturally a
//! single value — `{{ $node["Aggregate"].total }}` reads it directly, and the list
//! nodes still treat a bare object as a 1-item list, so it composes downstream.
//!
//! Each aggregation names a source `field` (dot/bracket path in each item; blank =
//! the item itself, for scalar arrays) and an `operation`; the result lands under
//! `outputField` (defaulting to the field's last segment, or the operation name
//! when there's no field). Numeric ops (sum/avg/min/max) coerce via the shared
//! `val_to_number` and skip values that aren't numbers; concat/collectField skip
//! missing/null values; count counts items (all of them, or only those where the
//! field is present when a field is given).

use crate::tools::workflow::{parse_path_pointer, val_to_number, val_to_string};
use serde_json::{json, Map, Value};

/// Turn the primary input into an item list — identical convention to Filter: an
/// array is the items, a `Null` contributes none, anything else is a single item,
/// and an explicit `array_path` unwraps a `{ results: [...] }` wrapper.
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

/// Read `field` (a dot/bracket path) out of `item`; an empty path returns the whole
/// item so scalar arrays reduce directly.
fn field_value(item: &Value, field: &str) -> Value {
    let f = field.trim();
    if f.is_empty() {
        return item.clone();
    }
    item.pointer(&parse_path_pointer(f))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Emit a float as an integer when it's whole (so `sum` of `[1,2,3]` is `6`, not
/// `6.0`); non-finite results (overflow / NaN) become `Null` rather than panicking.
fn number_value(f: f64) -> Value {
    if !f.is_finite() {
        return Value::Null;
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

/// The output key when the user leaves `outputField` blank: the field's last path
/// segment (`user.age` → `age`), or the operation name when there's no field.
fn default_output_field(op: &str, field: &str) -> String {
    let f = field.trim();
    if f.is_empty() {
        return op.to_string();
    }
    f.rsplit(|c| c == '.' || c == '[' || c == ']')
        .find(|s| !s.is_empty())
        .unwrap_or(f)
        .to_string()
}

/// Reduce all `items` under one aggregation spec into a single `Value`.
fn aggregate_one(op: &str, field: &str, items: &[Value], agg: &Value) -> Result<Value, String> {
    // Present (non-null) field values across every item; the numeric ops further
    // filter these through val_to_number.
    let values: Vec<Value> = items
        .iter()
        .map(|it| field_value(it, field))
        .filter(|v| !v.is_null())
        .collect();

    match op {
        // No field → count every item; with a field → count items that have it.
        "count" => {
            let n = if field.trim().is_empty() {
                items.len()
            } else {
                values.len()
            };
            Ok(json!(n))
        }
        "sum" => Ok(number_value(values.iter().filter_map(val_to_number).sum())),
        "avg" => {
            let nums: Vec<f64> = values.iter().filter_map(val_to_number).collect();
            if nums.is_empty() {
                Ok(Value::Null)
            } else {
                Ok(number_value(nums.iter().sum::<f64>() / nums.len() as f64))
            }
        }
        "min" => Ok(values
            .iter()
            .filter_map(val_to_number)
            .fold(None, |acc: Option<f64>, x| Some(acc.map_or(x, |a| a.min(x))))
            .map(number_value)
            .unwrap_or(Value::Null)),
        "max" => Ok(values
            .iter()
            .filter_map(val_to_number)
            .fold(None, |acc: Option<f64>, x| Some(acc.map_or(x, |a| a.max(x))))
            .map(number_value)
            .unwrap_or(Value::Null)),
        "concat" => {
            let sep = agg.get("separator").and_then(|v| v.as_str()).unwrap_or(", ");
            let joined = values
                .iter()
                .map(val_to_string)
                .collect::<Vec<_>>()
                .join(sep);
            Ok(Value::String(joined))
        }
        // Gather the present field values into an array (n8n "toList").
        "collectField" => Ok(Value::Array(values)),
        other => Err(format!(
            "Aggregate: unknown operation '{other}' (expected sum, avg, min, max, count, concat, or collectField)"
        )),
    }
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let array_path = config.get("arrayPath").and_then(|v| v.as_str());
    let items = to_items(input, array_path);

    let aggs = config
        .get("aggregations")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut out: Map<String, Value> = Map::new();

    // A bare Aggregate with nothing configured still does something useful: report
    // the item count. (Also the natural result of dragging the node onto the canvas.)
    if aggs.is_empty() {
        out.insert("count".to_string(), json!(items.len()));
        return Ok(Value::Object(out));
    }

    for agg in &aggs {
        let op = agg
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("count");
        let field = agg.get("field").and_then(|v| v.as_str()).unwrap_or("");
        let out_field = agg
            .get("outputField")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| default_output_field(op, field));

        let value = aggregate_one(op, field, &items, agg)?;
        out.insert(out_field, value);
    }

    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agg(op: &str, field: &str, out_field: &str) -> Value {
        json!({ "operation": op, "field": field, "outputField": out_field })
    }

    fn cfg(aggs: Vec<Value>, extra: Value) -> Value {
        let mut c = json!({ "aggregations": { "parameters": aggs } });
        if let (Some(obj), Some(ex)) = (c.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        c
    }

    // Sum of a numeric field; whole result stays an integer. String amounts coerce.
    #[test]
    fn sums_a_field_as_integer() {
        let input = json!([{ "amount": 10 }, { "amount": "20" }, { "amount": 30 }]);
        let out = execute(&cfg(vec![agg("sum", "amount", "total")], json!({})), &input).unwrap();
        assert_eq!(out, json!({ "total": 60 }));
    }

    // Average keeps a fractional result as a float.
    #[test]
    fn averages_a_field() {
        let input = json!([{ "n": 1 }, { "n": 2 }]);
        let out = execute(&cfg(vec![agg("avg", "n", "mean")], json!({})), &input).unwrap();
        assert_eq!(out, json!({ "mean": 1.5 }));
    }

    // Min and max over numeric values, skipping non-numeric.
    #[test]
    fn min_and_max_over_numeric_values() {
        let input = json!([{ "p": 5 }, { "p": "x" }, { "p": 2 }, { "p": 9 }]);
        let out = execute(
            &cfg(
                vec![agg("min", "p", "lo"), agg("max", "p", "hi")],
                json!({}),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!({ "lo": 2, "hi": 9 }));
    }

    // count with no field counts every item; with a field counts only items that
    // have it present.
    #[test]
    fn count_counts_items_or_present_field() {
        let input = json!([{ "email": "a@x" }, { "email": "b@x" }, { "name": "no email" }]);
        let out = execute(
            &cfg(
                vec![
                    agg("count", "", "items"),
                    agg("count", "email", "with_email"),
                ],
                json!({}),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!({ "items": 3, "with_email": 2 }));
    }

    // concat joins present field values as strings with a separator.
    #[test]
    fn concat_joins_with_separator() {
        let input = json!([{ "tag": "a" }, { "tag": "b" }, { "other": 1 }, { "tag": "c" }]);
        let mut a = agg("concat", "tag", "tags");
        a.as_object_mut()
            .unwrap()
            .insert("separator".to_string(), json!(" | "));
        let out = execute(&cfg(vec![a], json!({})), &input).unwrap();
        assert_eq!(out, json!({ "tags": "a | b | c" }));
    }

    // collectField gathers the present field values into an array.
    #[test]
    fn collect_field_gathers_values() {
        let input = json!([{ "id": 1 }, { "id": 2 }, { "nope": true }, { "id": 3 }]);
        let out = execute(
            &cfg(vec![agg("collectField", "id", "ids")], json!({})),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!({ "ids": [1, 2, 3] }));
    }

    // Multiple aggregations combine into one summary object.
    #[test]
    fn multiple_aggregations_combine() {
        let input = json!([{ "amt": 100 }, { "amt": 200 }, { "amt": 300 }]);
        let out = execute(
            &cfg(
                vec![
                    agg("sum", "amt", "total"),
                    agg("avg", "amt", "average"),
                    agg("count", "", "n"),
                ],
                json!({}),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!({ "total": 600, "average": 200, "n": 3 }));
    }

    // Empty field reduces the item itself — summing a scalar array.
    #[test]
    fn empty_field_reduces_scalar_array() {
        let input = json!([2, 4, 6]);
        let out = execute(&cfg(vec![agg("sum", "", "total")], json!({})), &input).unwrap();
        assert_eq!(out, json!({ "total": 12 }));
    }

    // No aggregations configured → just the item count.
    #[test]
    fn no_aggregations_returns_count() {
        let input = json!([{ "a": 1 }, { "a": 2 }]);
        let out = execute(&cfg(vec![], json!({})), &input).unwrap();
        assert_eq!(out, json!({ "count": 2 }));
    }

    // Empty numeric set: avg is null, sum is 0.
    #[test]
    fn empty_numeric_set_yields_null_avg_and_zero_sum() {
        let input = json!([{ "x": "not a number" }]);
        let out = execute(
            &cfg(
                vec![agg("avg", "x", "avg"), agg("sum", "x", "sum")],
                json!({}),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!({ "avg": Value::Null, "sum": 0 }));
    }

    // arrayPath unwraps a wrapper object before reducing.
    #[test]
    fn array_path_unwraps_wrapper() {
        let input = json!({ "rows": [{ "v": 3 }, { "v": 7 }] });
        let out = execute(
            &cfg(
                vec![agg("sum", "v", "total")],
                json!({ "arrayPath": "rows" }),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!({ "total": 10 }));
    }

    // Blank outputField derives the key from the field's last segment / op name.
    #[test]
    fn default_output_field_derivation() {
        let input = json!([{ "user": { "age": 30 } }, { "user": { "age": 10 } }]);
        let out = execute(
            &cfg(
                vec![
                    json!({ "operation": "sum", "field": "user.age" }),
                    json!({ "operation": "count", "field": "" }),
                ],
                json!({}),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!({ "age": 40, "count": 2 }));
    }

    // An unknown operation is a clear error, not a silent wrong result.
    #[test]
    fn unknown_operation_errors() {
        let input = json!([{ "a": 1 }]);
        let err = execute(&cfg(vec![agg("median", "a", "m")], json!({})), &input).unwrap_err();
        assert!(err.contains("unknown operation"), "got: {err}");
    }
}
