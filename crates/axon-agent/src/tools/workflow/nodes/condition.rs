use crate::tools::workflow::evaluate_condition_typed;
use serde_json::{json, Value};

pub(crate) fn execute_if_condition_node(config: &Value) -> Result<Value, String> {
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

    // Node-level default; each condition may override with its own flag.
    let default_cs = config
        .get("caseSensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let mut results = Vec::new();
    for cond in &conditions {
        let data_type = cond
            .get("dataType")
            .and_then(|v| v.as_str())
            .unwrap_or("string");
        let op = cond
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("equals");
        // Values arrive already expression-resolved, so keep their JSON type.
        let v1 = cond.get("value1").cloned().unwrap_or(Value::Null);
        let v2 = cond.get("value2").cloned().unwrap_or(Value::Null);
        let cs = cond
            .get("caseSensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(default_cs);

        results.push(evaluate_condition_typed(data_type, op, &v1, &v2, cs));
    }

    let final_result = if combine == "any" {
        results.iter().any(|&r| r)
    } else if results.is_empty() {
        // No conditions defined: route to the false branch (n8n parity).
        false
    } else {
        results.iter().all(|&r| r)
    };

    Ok(json!({
        "condition": final_result,
        "branch": if final_result { "true" } else { "false" },
        "outputIndex": if final_result { 0 } else { 1 },
        "results": results
    }))
}

pub(crate) fn execute_switch_node(config: &Value) -> Result<Value, String> {
    // Top-level type/case-sensitivity act as defaults; each rule may override
    // its own dataType (n8n v3 routes each rule with its own condition type).
    let top_data_type = config
        .get("dataType")
        .and_then(|v| v.as_str())
        .unwrap_or("string");
    let default_cs = config
        .get("caseSensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    // Tested value keeps its resolved JSON type.
    let top_value = config.get("value1").cloned().unwrap_or(Value::Null);

    // matchMode beyond n8n: "first" (n8n parity — first matching rule wins) or
    // "all" (route the value to EVERY matching output simultaneously — a fan-out
    // switch n8n's standard node can't do without extra nodes).
    let match_mode = config
        .get("matchMode")
        .and_then(|v| v.as_str())
        .unwrap_or("first");

    let rules = config
        .get("rules")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Static UI outputs: Case1..Case5 + Default (index 5)
    let max_cases = 5usize;
    let mut matched: Vec<usize> = Vec::new();

    for (idx, rule) in rules.iter().enumerate().take(max_cases) {
        let data_type = rule
            .get("dataType")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(top_data_type);
        let op = rule
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("equals");
        // Per-rule subject override beyond n8n: a rule may test its OWN value1
        // (e.g. route on a different field per case). Empty/absent → fall back
        // to the node-level value.
        let subject = match rule.get("value1") {
            Some(v) if !(v.is_null() || (v.is_string() && v.as_str() == Some(""))) => v.clone(),
            _ => top_value.clone(),
        };
        let case_value = rule
            .get("value2")
            .or_else(|| rule.get("value"))
            .cloned()
            .unwrap_or(Value::Null);
        let cs = rule
            .get("caseSensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(default_cs);

        if evaluate_condition_typed(data_type, op, &subject, &case_value, cs) {
            matched.push(idx);
            if match_mode != "all" {
                break;
            }
        }
    }

    // No rule matched → the Default output (index = max_cases).
    let indices: Vec<usize> = if matched.is_empty() {
        vec![max_cases]
    } else {
        matched.clone()
    };
    let first = *indices.first().unwrap_or(&max_cases);

    Ok(json!({
        "value": top_value,
        // outputIndex: first active output (back-compat + UI run animation).
        "outputIndex": first,
        // outputIndices: every active output (drives multi-output routing).
        "outputIndices": indices,
        "matchMode": match_mode,
        "matched": !matched.is_empty(),
        "branch": if matched.is_empty() {
            "default".to_string()
        } else {
            format!("case_{}", first + 1)
        }
    }))
}
