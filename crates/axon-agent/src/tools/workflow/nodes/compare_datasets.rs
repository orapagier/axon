//! Compare Datasets — diff two item lists (n8n gap-closure). Feed it two
//! branches like Merge (input handles `input_main_0` = A, `input_main_1` = B),
//! give it the key field(s) that pair rows, and it buckets every item:
//!
//!   - `same`      — key in both, compared fields equal.
//!   - `different` — key in both, some field differs; each entry carries
//!                   `{key, a, b, changed_fields}` (or just the preferred side
//!                   when `preferWhenDifferent` is "a"/"b").
//!   - `a_only` / `b_only` — key present on one side only.
//!
//! One composable output object (`summary` holds the counts for quick IF
//! routing; each bucket splits out via Split Out / `$json.different` etc.) —
//! where n8n forces four output branches, this keeps the graph linear until
//! YOU choose to fan out.
//!
//! Config: `matchBy` (comma-separated key fields, required), `skipFields`
//! (ignored during comparison — timestamps, etags), `preferWhenDifferent`
//! ("both" default | "a" | "b"). Items whose key fields are all missing land
//! in `a_only`/`b_only` (they can't pair). Duplicate keys within a side keep
//! the first occurrence (a warning rides along in `summary`).

use crate::tools::workflow::val_to_string;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Flatten one input handle's outputs to an item list (array convention).
fn side_items(values: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for v in values {
        match v {
            Value::Array(a) => out.extend(a.iter().cloned()),
            Value::Null => {}
            other => out.push(other.clone()),
        }
    }
    out
}

/// Composite pairing key from the `matchBy` fields (string-canonical, so
/// `"7"` and `7` pair). None when every key field is missing.
fn pair_key(item: &Value, fields: &[String]) -> Option<String> {
    let mut parts = Vec::with_capacity(fields.len());
    let mut any = false;
    for f in fields {
        let v = item.get(f.as_str()).cloned().unwrap_or(Value::Null);
        if !v.is_null() {
            any = true;
        }
        parts.push(val_to_string(&v));
    }
    if any {
        Some(parts.join("\u{1}"))
    } else {
        None
    }
}

/// The item minus key fields and skip fields — what actually gets compared.
fn comparable(item: &Value, exclude: &[String]) -> Value {
    match item {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| !exclude.contains(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Map<String, Value>>(),
        ),
        other => other.clone(),
    }
}

fn csv_list(config: &Value, key: &str) -> Vec<String> {
    config
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn execute(
    config: &Value,
    inputs_by_handle: &BTreeMap<String, Vec<Value>>,
) -> Result<Value, String> {
    let match_by = csv_list(config, "matchBy");
    if match_by.is_empty() {
        return Err(
            "Compare Datasets needs 'matchBy' — the key field(s) that pair items across the two inputs"
                .to_string(),
        );
    }
    let skip_fields = csv_list(config, "skipFields");
    let prefer = config
        .get("preferWhenDifferent")
        .and_then(|v| v.as_str())
        .unwrap_or("both");

    // Sides in stable handle order, exactly like Merge: first handle = A,
    // second = B. A missing side is just an empty list (everything lands in
    // the other side's `_only` bucket).
    let mut sides = inputs_by_handle.values();
    let a_items = sides.next().map(|v| side_items(v)).unwrap_or_default();
    let b_items = sides.next().map(|v| side_items(v)).unwrap_or_default();

    // Fields excluded from the equality check: the pairing keys (equal by
    // construction) plus the configured skips.
    let mut exclude = match_by.clone();
    exclude.extend(skip_fields.iter().cloned());

    let mut a_unkeyed: Vec<Value> = Vec::new();
    let mut a_by_key: BTreeMap<String, Value> = BTreeMap::new();
    let mut duplicate_keys = 0usize;
    for it in a_items {
        match pair_key(&it, &match_by) {
            Some(k) => {
                if a_by_key.contains_key(&k) {
                    duplicate_keys += 1;
                } else {
                    a_by_key.insert(k, it);
                }
            }
            None => a_unkeyed.push(it),
        }
    }

    let mut same = Vec::new();
    let mut different = Vec::new();
    let mut b_only: Vec<Value> = Vec::new();
    let mut seen_in_b: std::collections::HashSet<String> = std::collections::HashSet::new();

    for it in b_items {
        let key = match pair_key(&it, &match_by) {
            Some(k) => k,
            None => {
                b_only.push(it);
                continue;
            }
        };
        if !seen_in_b.insert(key.clone()) {
            duplicate_keys += 1;
            continue;
        }
        match a_by_key.remove(&key) {
            None => b_only.push(it),
            Some(a_it) => {
                let ca = comparable(&a_it, &exclude);
                let cb = comparable(&it, &exclude);
                if ca == cb {
                    same.push(a_it);
                } else {
                    // The fields whose values differ (union of both objects'
                    // keys, minus excluded).
                    let mut changed: Vec<String> = Vec::new();
                    if let (Value::Object(ma), Value::Object(mb)) = (&ca, &cb) {
                        let mut keys: Vec<&String> = ma.keys().chain(mb.keys()).collect();
                        keys.sort();
                        keys.dedup();
                        for k in keys {
                            if ma.get(k) != mb.get(k) {
                                changed.push(k.clone());
                            }
                        }
                    }
                    different.push(match prefer {
                        "a" => a_it,
                        "b" => it,
                        _ => json!({
                            "key": key.replace('\u{1}', " / "),
                            "a": a_it,
                            "b": it,
                            "changed_fields": changed,
                        }),
                    });
                }
            }
        }
    }

    // Whatever A items were never claimed by a B match (plus unkeyable ones).
    let mut a_only: Vec<Value> = a_by_key.into_values().collect();
    a_only.extend(a_unkeyed);

    let summary = json!({
        "same": same.len(),
        "different": different.len(),
        "a_only": a_only.len(),
        "b_only": b_only.len(),
        "duplicate_keys_skipped": duplicate_keys,
    });
    Ok(json!({
        "same": same,
        "different": different,
        "a_only": a_only,
        "b_only": b_only,
        "summary": summary,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handles(a: Value, b: Value) -> BTreeMap<String, Vec<Value>> {
        let mut m = BTreeMap::new();
        m.insert("input_main_0".to_string(), vec![a]);
        m.insert("input_main_1".to_string(), vec![b]);
        m
    }

    fn cfg(match_by: &str) -> Value {
        json!({ "matchBy": match_by })
    }

    // The four buckets: unchanged, changed, only-in-A, only-in-B.
    #[test]
    fn buckets_items_correctly() {
        let a = json!([
            { "id": 1, "name": "ana" },
            { "id": 2, "name": "bo" },
            { "id": 3, "name": "cy" },
        ]);
        let b = json!([
            { "id": 1, "name": "ana" },
            { "id": 2, "name": "bob" },
            { "id": 4, "name": "di" },
        ]);
        let out = execute(&cfg("id"), &handles(a, b)).unwrap();
        assert_eq!(out["summary"]["same"], json!(1));
        assert_eq!(out["summary"]["different"], json!(1));
        assert_eq!(out["summary"]["a_only"], json!(1));
        assert_eq!(out["summary"]["b_only"], json!(1));
        assert_eq!(out["same"][0]["id"], json!(1));
        assert_eq!(out["different"][0]["changed_fields"], json!(["name"]));
        assert_eq!(out["a_only"][0]["id"], json!(3));
        assert_eq!(out["b_only"][0]["id"], json!(4));
    }

    // skipFields excludes volatile fields from the equality check.
    #[test]
    fn skip_fields_ignored_in_comparison() {
        let a = json!([{ "id": 1, "name": "ana", "updated_at": "2026-01-01" }]);
        let b = json!([{ "id": 1, "name": "ana", "updated_at": "2026-07-10" }]);
        let out = execute(
            &json!({ "matchBy": "id", "skipFields": "updated_at" }),
            &handles(a, b),
        )
        .unwrap();
        assert_eq!(out["summary"]["same"], json!(1));
        assert_eq!(out["summary"]["different"], json!(0));
    }

    // preferWhenDifferent picks one side's item instead of the {a,b} pair.
    #[test]
    fn prefer_side_replaces_pair() {
        let a = json!([{ "id": 1, "v": "old" }]);
        let b = json!([{ "id": 1, "v": "new" }]);
        let out = execute(
            &json!({ "matchBy": "id", "preferWhenDifferent": "b" }),
            &handles(a, b),
        )
        .unwrap();
        assert_eq!(out["different"][0], json!({ "id": 1, "v": "new" }));
    }

    // Numeric and string keys pair ("7" == 7 via canonical string form).
    #[test]
    fn keys_pair_across_types() {
        let a = json!([{ "id": "7", "x": 1 }]);
        let b = json!([{ "id": 7, "x": 1 }]);
        let out = execute(&cfg("id"), &handles(a, b)).unwrap();
        assert_eq!(out["summary"]["same"], json!(1));
    }

    // Composite keys: both fields must match to pair.
    #[test]
    fn composite_match_keys() {
        let a = json!([{ "region": "ph", "sku": "A", "stock": 5 }]);
        let b = json!([{ "region": "us", "sku": "A", "stock": 5 }]);
        let out = execute(&cfg("region, sku"), &handles(a, b)).unwrap();
        assert_eq!(out["summary"]["a_only"], json!(1));
        assert_eq!(out["summary"]["b_only"], json!(1));
    }

    // Items with no key fields can't pair — they land in their side's bucket.
    #[test]
    fn unkeyed_items_fall_to_only_buckets() {
        let a = json!([{ "name": "keyless" }]);
        let b = json!([{ "id": 1, "name": "keyed" }]);
        let out = execute(&cfg("id"), &handles(a, b)).unwrap();
        assert_eq!(out["summary"]["a_only"], json!(1));
        assert_eq!(out["summary"]["b_only"], json!(1));
    }

    // A missing matchBy is a config error.
    #[test]
    fn missing_match_by_errors() {
        let err = execute(&json!({}), &handles(json!([]), json!([]))).unwrap_err();
        assert!(err.contains("matchBy"), "got: {err}");
    }

    // One dead side (merge-after-IF style): everything is a_only.
    #[test]
    fn single_live_side() {
        let mut m = BTreeMap::new();
        m.insert(
            "input_main_0".to_string(),
            vec![json!([{ "id": 1 }, { "id": 2 }])],
        );
        let out = execute(&cfg("id"), &m).unwrap();
        assert_eq!(out["summary"]["a_only"], json!(2));
        assert_eq!(out["summary"]["b_only"], json!(0));
    }
}
