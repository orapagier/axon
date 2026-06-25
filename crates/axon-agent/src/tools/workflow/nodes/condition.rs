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

    // What to do with a value that matches no rule (n8n's "Fallback Output"):
    //   "extra" — route it to a dedicated Default output (one past the last rule)
    //   "none"  — drop it; follow no edge at all.
    let fallback = config
        .get("fallbackOutput")
        .and_then(|v| v.as_str())
        .unwrap_or("extra");

    let rules = config
        .get("rules")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Dynamic outputs (n8n-style): one output per rule, in order. The Default
    // output, when present, sits immediately past the last rule.
    let default_index = rules.len();
    let mut matched: Vec<usize> = Vec::new();

    for (idx, rule) in rules.iter().enumerate() {
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

    // Resolve the active output handle(s) and a human-readable branch label.
    let (indices, branch): (Vec<i64>, String) = if !matched.is_empty() {
        (
            matched.iter().map(|&i| i as i64).collect(),
            format!("case_{}", matched[0] + 1),
        )
    } else if fallback == "none" {
        // Drop: no output is active. The empty list (and the -1 outputIndex
        // below) guarantee no `output_main_N` edge handle ever matches.
        (Vec::new(), "none".to_string())
    } else {
        (vec![default_index as i64], "default".to_string())
    };
    let first = indices.first().copied().unwrap_or(-1);

    Ok(json!({
        "value": top_value,
        // outputIndex: first active output (back-compat + UI run animation).
        // -1 means "nothing matched and fallback is none" → no edge animates.
        "outputIndex": first,
        // outputIndices: every active output (drives multi-output routing).
        "outputIndices": indices,
        "matchMode": match_mode,
        "matched": !matched.is_empty(),
        "branch": branch
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a string-equals switch: top value `subject`, one rule per `cases`
    // entry comparing the subject against that case value.
    fn switch_config(subject: &str, cases: &[&str], extra: Value) -> Value {
        let rules: Vec<Value> = cases
            .iter()
            .map(|c| json!({ "operation": "equals", "value2": c }))
            .collect();
        let mut cfg = json!({
            "dataType": "string",
            "value1": subject,
            "rules": { "parameters": rules },
        });
        if let (Some(obj), Some(ex)) = (cfg.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        cfg
    }

    fn indices(out: &Value) -> Vec<i64> {
        out.get("outputIndices")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn routes_to_matching_rule_index() {
        let out = execute_switch_node(&switch_config("b", &["a", "b", "c"], json!({}))).unwrap();
        assert_eq!(out["outputIndex"], 1);
        assert_eq!(indices(&out), vec![1]);
        assert_eq!(out["branch"], "case_2");
    }

    #[test]
    fn default_output_index_follows_rule_count() {
        // Three rules, nothing matches → Default sits just past the last rule.
        let out = execute_switch_node(&switch_config("z", &["a", "b", "c"], json!({}))).unwrap();
        assert_eq!(out["outputIndex"], 3);
        assert_eq!(indices(&out), vec![3]);
        assert_eq!(out["branch"], "default");
        assert_eq!(out["matched"], false);
    }

    #[test]
    fn fallback_none_drops_unmatched() {
        let cfg = switch_config("z", &["a", "b"], json!({ "fallbackOutput": "none" }));
        let out = execute_switch_node(&cfg).unwrap();
        assert_eq!(out["outputIndex"], -1);
        assert!(indices(&out).is_empty());
        assert_eq!(out["branch"], "none");
    }

    #[test]
    fn match_mode_all_fans_out_to_every_match() {
        let cfg = switch_config("x", &["x", "x", "y"], json!({ "matchMode": "all" }));
        let out = execute_switch_node(&cfg).unwrap();
        assert_eq!(indices(&out), vec![0, 1]);
        assert_eq!(out["outputIndex"], 0);
    }

    #[test]
    fn supports_more_than_five_rules() {
        // The old node capped at five cases; the dynamic node must route the 7th.
        let cases = ["0", "1", "2", "3", "4", "5", "6"];
        let out = execute_switch_node(&switch_config("5", &cases, json!({}))).unwrap();
        assert_eq!(out["outputIndex"], 5);
        assert_eq!(indices(&out), vec![5]);
        // And Default for this 7-rule switch would be index 7.
        let miss = execute_switch_node(&switch_config("nope", &cases, json!({}))).unwrap();
        assert_eq!(miss["outputIndex"], 7);
    }
}
