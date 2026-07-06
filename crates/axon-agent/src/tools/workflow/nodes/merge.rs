//! Plexus (Merge) — Task 1.1. Rejoins two forked branches, or reshapes two lists
//! into one. This is the node that makes every IF/Switch/Approval fork stop being
//! a dead-end.
//!
//! Input model: the engine hands us the direct-predecessor outputs already grouped
//! by input handle (`direct_predecessor_outputs`, Task 1.0). That grouping has
//! already (a) excluded the prior-run cache seed, (b) dropped not-taken
//! (`status: "skipped"`) branches, and (c) keyed each side by its `target_handle`
//! (`input_main_0`, `input_main_1`, …). So a not-taken branch is simply ABSENT
//! here — which is exactly how the skipped-branch pass-through works: with only one
//! live side, we return it unchanged (never error, never null the dead side).
//!
//! Output is always a single array `Value`, per Axon's array-input convention, so
//! the merged stream composes with the other list nodes and with `loop`.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Normalize an input handle's collected predecessor outputs into a flat item
/// list. Per the array-input convention a list node's input is an array `Value`;
/// a predecessor that emitted a bare object counts as a one-item list, and a
/// `Null` (e.g. a node that produced nothing) contributes no items.
fn flatten_items(values: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for v in values {
        match v {
            Value::Array(arr) => out.extend(arr.iter().cloned()),
            Value::Null => {}
            other => out.push(other.clone()),
        }
    }
    out
}

/// Field-merge two items into one. When both are JSON objects, `b`'s fields win on
/// key conflicts (the second input *enriches* the first). When only one side is an
/// object, that object is kept (a merge must never null out a live item); with two
/// non-objects the second is taken. `None` means "no item on this side".
fn merge_items(a: Option<&Value>, b: Option<&Value>) -> Value {
    match (a, b) {
        (Some(Value::Object(oa)), Some(Value::Object(ob))) => {
            let mut m: Map<String, Value> = oa.clone();
            for (k, v) in ob {
                m.insert(k.clone(), v.clone());
            }
            Value::Object(m)
        }
        (Some(av), None) => av.clone(),
        (None, Some(bv)) => bv.clone(),
        // At least one side is a non-object: prefer an object side, else take `b`.
        (Some(av), Some(bv)) => {
            if av.is_object() {
                av.clone()
            } else {
                bv.clone()
            }
        }
        (None, None) => Value::Null,
    }
}

/// Read the two match-field names for `mergeByKey`. A single `field` applies to
/// both inputs; `field1`/`field2` override per side when the key names differ.
fn match_fields(config: &Value) -> (String, String) {
    let common = config.get("field").and_then(|v| v.as_str()).unwrap_or("");
    let f1 = config
        .get("field1")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(common);
    let f2 = config
        .get("field2")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(if f1.is_empty() { common } else { f1 });
    (f1.to_string(), f2.to_string())
}

/// Combine two live sides `a` and `b` per `mode`. Folded left-to-right so >2 wired
/// inputs still merge sensibly, though the node renders exactly two handles.
fn combine_two(mode: &str, a: &[Value], b: &[Value], config: &Value) -> Result<Vec<Value>, String> {
    match mode {
        // Concatenate: input 0's items, then input 1's. The default and #1 unlock.
        "append" => {
            let mut out = a.to_vec();
            out.extend(b.iter().cloned());
            Ok(out)
        }
        // Zip by index: item[i] of A field-merged with item[i] of B. Unpaired tail
        // items (when the sides differ in length) pass through unchanged.
        "mergeByPosition" => {
            let n = a.len().max(b.len());
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                out.push(merge_items(a.get(i), b.get(i)));
            }
            Ok(out)
        }
        // Cartesian product: every A item field-merged with every B item.
        "combine" => {
            let mut out = Vec::with_capacity(a.len().saturating_mul(b.len()));
            for ai in a {
                for bi in b {
                    out.push(merge_items(Some(ai), Some(bi)));
                }
            }
            Ok(out)
        }
        // SQL-style left join on a matching field: each A item is enriched with the
        // first B item whose key equals A's key. Unmatched A items pass through.
        "mergeByKey" => {
            let (key1, key2) = match_fields(config);
            if key1.is_empty() || key2.is_empty() {
                return Err(
                    "Merge (Merge By Key) needs a 'Field to Match' to join the two inputs on"
                        .to_string(),
                );
            }
            let mut out = Vec::with_capacity(a.len());
            for ai in a {
                let av = ai.get(&key1);
                let matched = match av {
                    Some(av) => b.iter().find(|bi| bi.get(&key2) == Some(av)),
                    None => None,
                };
                out.push(merge_items(Some(ai), matched));
            }
            Ok(out)
        }
        other => Err(format!(
            "Merge: unknown mode '{other}' (expected append, mergeByKey, mergeByPosition, or combine)"
        )),
    }
}

/// Entry point. `inputs_by_handle` is the engine-supplied, handle-grouped map of
/// this merge node's live direct-predecessor outputs (see module docs).
pub(crate) fn execute(
    config: &Value,
    inputs_by_handle: &BTreeMap<String, Vec<Value>>,
) -> Result<Value, String> {
    let mode = config
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("append");

    // Sides in stable input-handle order (BTreeMap keys sort input_main_0 <
    // input_main_1 < …). Each side is flattened to an item list.
    let mut sides: Vec<Vec<Value>> = inputs_by_handle
        .values()
        .map(|vs| flatten_items(vs))
        .collect();

    // Nothing ran into the merge (both branches dead / node unconnected).
    if sides.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    // Exactly one live side → pass it straight through, for EVERY mode. This is the
    // merge-after-IF case: the not-taken branch is absent, so there is nothing to
    // join/zip against and we must not error or emit nulls for the dead side.
    if sides.len() == 1 {
        return Ok(Value::Array(sides.pop().unwrap()));
    }

    // Two (or more) live sides → merge per the mode, folding any extras in.
    let mut acc = sides[0].clone();
    for right in sides.iter().skip(1) {
        acc = combine_two(mode, &acc, right, config)?;
    }
    Ok(Value::Array(acc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn handles(pairs: Vec<(&str, Value)>) -> BTreeMap<String, Vec<Value>> {
        let mut m: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for (h, v) in pairs {
            m.entry(h.to_string()).or_default().push(v);
        }
        m
    }

    // Append (default): two branches rejoin into one array, input 0 then input 1.
    #[test]
    fn append_concatenates_both_sides_in_handle_order() {
        let inputs = handles(vec![
            ("input_main_1", json!([{ "n": 3 }, { "n": 4 }])),
            ("input_main_0", json!([{ "n": 1 }, { "n": 2 }])),
        ]);
        let out = execute(&json!({ "mode": "append" }), &inputs).unwrap();
        assert_eq!(out, json!([{ "n": 1 }, { "n": 2 }, { "n": 3 }, { "n": 4 }]));
    }

    // A bare (non-array) object on a side is treated as a one-item list.
    #[test]
    fn append_treats_bare_object_as_single_item() {
        let inputs = handles(vec![
            ("input_main_0", json!({ "a": 1 })),
            ("input_main_1", json!({ "b": 2 })),
        ]);
        let out = execute(&json!({}), &inputs).unwrap(); // default mode == append
        assert_eq!(out, json!([{ "a": 1 }, { "b": 2 }]));
    }

    // THE skipped-branch semantics: only one side is present (the other branch was
    // routed away by an IF/Switch, so Task 1.0 dropped it). Pass the live side
    // through unchanged — no error, no nulls — regardless of the configured mode.
    #[test]
    fn one_live_side_passes_through_for_every_mode() {
        for mode in ["append", "mergeByKey", "mergeByPosition", "combine"] {
            let inputs = handles(vec![("input_main_0", json!([{ "kept": true }]))]);
            let out = execute(&json!({ "mode": mode, "field": "id" }), &inputs).unwrap();
            assert_eq!(out, json!([{ "kept": true }]), "mode={mode}");
        }
    }

    // No live sides at all → empty array, never an error.
    #[test]
    fn no_sides_yields_empty_array() {
        let out = execute(&json!({ "mode": "append" }), &BTreeMap::new()).unwrap();
        assert_eq!(out, json!([]));
    }

    // Merge By Key: SQL-style left join enriches each left item with the matching
    // right item's fields (right wins on conflict); unmatched left passes through.
    #[test]
    fn merge_by_key_left_joins_on_field() {
        let inputs = handles(vec![
            (
                "input_main_0",
                json!([{ "id": 1, "name": "a" }, { "id": 2, "name": "b" }]),
            ),
            (
                "input_main_1",
                json!([{ "id": 1, "email": "a@x.com" }, { "id": 9, "email": "z@x.com" }]),
            ),
        ]);
        let out = execute(&json!({ "mode": "mergeByKey", "field": "id" }), &inputs).unwrap();
        assert_eq!(
            out,
            json!([
                { "id": 1, "name": "a", "email": "a@x.com" }, // matched → enriched
                { "id": 2, "name": "b" }                       // unmatched → passthrough
            ])
        );
    }

    // Merge By Key with distinct key names per side (field1/field2).
    #[test]
    fn merge_by_key_supports_distinct_field_names() {
        let inputs = handles(vec![
            ("input_main_0", json!([{ "uid": 7, "role": "admin" }])),
            ("input_main_1", json!([{ "userId": 7, "active": true }])),
        ]);
        let out = execute(
            &json!({ "mode": "mergeByKey", "field1": "uid", "field2": "userId" }),
            &inputs,
        )
        .unwrap();
        // Field-merge is a full union of both items' fields, so the joined row
        // keeps input 1's `uid` AND input 2's `userId` (plus its `active`).
        assert_eq!(
            out,
            json!([{ "uid": 7, "userId": 7, "role": "admin", "active": true }])
        );
    }

    // Merge By Key without a field is a config error (both sides present).
    #[test]
    fn merge_by_key_without_field_errors_when_both_present() {
        let inputs = handles(vec![
            ("input_main_0", json!([{ "id": 1 }])),
            ("input_main_1", json!([{ "id": 1 }])),
        ]);
        let err = execute(&json!({ "mode": "mergeByKey" }), &inputs).unwrap_err();
        assert!(err.contains("Field to Match"), "got: {err}");
    }

    // Merge By Position: zip by index, field-merging paired items; the longer
    // side's unpaired tail passes through.
    #[test]
    fn merge_by_position_zips_and_keeps_unpaired_tail() {
        let inputs = handles(vec![
            ("input_main_0", json!([{ "a": 1 }, { "a": 2 }, { "a": 3 }])),
            ("input_main_1", json!([{ "b": 10 }, { "b": 20 }])),
        ]);
        let out = execute(&json!({ "mode": "mergeByPosition" }), &inputs).unwrap();
        assert_eq!(
            out,
            json!([{ "a": 1, "b": 10 }, { "a": 2, "b": 20 }, { "a": 3 }])
        );
    }

    // Combine (cartesian): every left item merged with every right item.
    #[test]
    fn combine_produces_cartesian_product() {
        let inputs = handles(vec![
            ("input_main_0", json!([{ "a": 1 }, { "a": 2 }])),
            ("input_main_1", json!([{ "b": 10 }, { "b": 20 }])),
        ]);
        let out = execute(&json!({ "mode": "combine" }), &inputs).unwrap();
        assert_eq!(
            out,
            json!([
                { "a": 1, "b": 10 }, { "a": 1, "b": 20 },
                { "a": 2, "b": 10 }, { "a": 2, "b": 20 }
            ])
        );
    }

    // An unknown mode is a clear error rather than a silent wrong result.
    #[test]
    fn unknown_mode_errors() {
        let inputs = handles(vec![
            ("input_main_0", json!([{ "a": 1 }])),
            ("input_main_1", json!([{ "b": 2 }])),
        ]);
        let err = execute(&json!({ "mode": "bogus" }), &inputs).unwrap_err();
        assert!(err.contains("unknown mode"), "got: {err}");
    }
}
