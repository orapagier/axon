//! Soma — Edit Fields.
//!
//! Builds an output object from user-defined fields. With "Include Other Input
//! Fields" on, the set fields are merged over the incoming item; otherwise the
//! output contains only the fields you set. Field values arrive already
//! expression-resolved by `interpolate_config`, so they may be a real JSON value
//! or a string depending on the expression — `cast_value` coerces to the chosen
//! type.

use serde_json::{json, Map, Value};

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let include_others = config
        .get("includeOtherFields")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut out: Map<String, Value> = if include_others {
        match input {
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        }
    } else {
        Map::new()
    };

    let fields = config
        .get("fields")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for f in &fields {
        let name = f
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            continue;
        }
        let ty = f.get("type").and_then(|v| v.as_str()).unwrap_or("string");
        let raw = f.get("value").cloned().unwrap_or(Value::Null);
        out.insert(name, cast_value(ty, raw));
    }

    Ok(Value::Object(out))
}

fn cast_value(ty: &str, raw: Value) -> Value {
    match ty {
        "number" => match &raw {
            Value::Number(_) => raw,
            Value::String(s) => {
                let t = s.trim();
                if let Ok(i) = t.parse::<i64>() {
                    json!(i)
                } else if let Ok(fl) = t.parse::<f64>() {
                    json!(fl)
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        },
        "boolean" => match &raw {
            Value::Bool(_) => raw,
            Value::String(s) => {
                let t = s.trim().to_ascii_lowercase();
                json!(matches!(t.as_str(), "true" | "1" | "yes" | "on"))
            }
            Value::Number(n) => json!(n.as_f64().map(|f| f != 0.0).unwrap_or(false)),
            _ => Value::Bool(false),
        },
        "json" => match &raw {
            Value::String(s) => {
                serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
            }
            other => other.clone(),
        },
        // string (default)
        _ => match raw {
            Value::String(_) => raw,
            Value::Null => Value::String(String::new()),
            other => Value::String(other.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sets_only_defined_fields_by_default() {
        let config = json!({
            "fields": { "parameters": [
                { "name": "status", "type": "string", "value": "done" },
                { "name": "count", "type": "number", "value": "3" },
            ]}
        });
        let input = json!({ "keep": "me" });
        let out = execute(&config, &input).unwrap();
        assert_eq!(out, json!({ "status": "done", "count": 3 }));
    }

    #[test]
    fn merges_over_input_when_including_others() {
        let config = json!({
            "includeOtherFields": true,
            "fields": { "parameters": [
                { "name": "status", "type": "string", "value": "done" },
            ]}
        });
        let input = json!({ "keep": "me", "status": "old" });
        let out = execute(&config, &input).unwrap();
        assert_eq!(out, json!({ "keep": "me", "status": "done" }));
    }

    #[test]
    fn casts_types() {
        let config = json!({
            "fields": { "parameters": [
                { "name": "n", "type": "number", "value": "4.5" },
                { "name": "b", "type": "boolean", "value": "yes" },
                { "name": "j", "type": "json", "value": "{\"a\":1}" },
            ]}
        });
        let out = execute(&config, &Value::Null).unwrap();
        assert_eq!(out["n"], json!(4.5));
        assert_eq!(out["b"], json!(true));
        assert_eq!(out["j"], json!({ "a": 1 }));
    }

    #[test]
    fn skips_unnamed_fields() {
        let config = json!({
            "fields": { "parameters": [ { "name": "  ", "type": "string", "value": "x" } ]}
        });
        let out = execute(&config, &Value::Null).unwrap();
        assert_eq!(out, json!({}));
    }
}
