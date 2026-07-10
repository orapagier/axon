//! GraphQL — a first-class endpoint node over the Synapse HTTP engine.
//!
//! Synapse can already POST a GraphQL body (`contentType: "graphql"`), but a
//! dedicated node earns its place with the parts that make GraphQL annoying
//! over a raw HTTP node: a `query` / `variables` / `operationName` config
//! surface, and correct error semantics — GraphQL servers return HTTP 200
//! with an `errors` array, which a plain HTTP node reports as success.
//!
//! Config: `url`, `query` (required), `variables` (object or JSON string),
//! `operationName`, `failOnErrors` (default true — any GraphQL error fails
//! the node so retries/continue-on-fail apply; off routes `{data, errors}`
//! downstream). Every Synapse auth mode (`authentication` /
//! `genericAuthType` / `connectedAccount`), header option, and the `options`
//! collection pass straight through, because the request IS a Synapse request.

use crate::state::AppState;
use serde_json::{json, Value};

/// Shape the HTTP response into the node's `{data, errors, status}` output,
/// or an Err when the server reported GraphQL errors and `fail_on_errors`.
fn shape_response(response: &Value, fail_on_errors: bool) -> Result<Value, String> {
    let body = response.get("body").cloned().unwrap_or(Value::Null);
    // A `text` response format (or a non-JSON error page) leaves the body a
    // string — surface it as-is under data with an explanatory error.
    let (data, errors) = match &body {
        Value::Object(m) => (
            m.get("data").cloned().unwrap_or(Value::Null),
            m.get("errors").cloned().unwrap_or_else(|| json!([])),
        ),
        other => (other.clone(), json!([])),
    };
    let has_errors = errors.as_array().map(|a| !a.is_empty()).unwrap_or(false);
    if has_errors && fail_on_errors {
        let msgs: Vec<String> = errors
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                e.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
                    .to_string()
            })
            .collect();
        return Err(format!("GraphQL errors: {}", msgs.join("; ")));
    }
    Ok(json!({
        "data": data,
        "errors": errors,
        "status": response.get("status").cloned().unwrap_or(Value::Null),
    }))
}

pub(crate) async fn execute(config: &Value, state: &AppState) -> Result<Value, String> {
    let query = config
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "GraphQL needs a 'query'".to_string())?
        .to_string();
    if config
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err("GraphQL needs an endpoint 'url'".to_string());
    }

    // Rebuild as a Synapse config: same object (auth/headers/options ride
    // along), with the GraphQL body fields mapped onto Synapse's keys.
    let mut cfg = config.as_object().cloned().unwrap_or_default();
    cfg.insert("method".to_string(), json!("POST"));
    cfg.insert("sendBody".to_string(), json!(true));
    cfg.insert("contentType".to_string(), json!("graphql"));
    cfg.insert("graphqlQuery".to_string(), json!(query));
    if let Some(vars) = config.get("variables").cloned() {
        cfg.insert("graphqlVariables".to_string(), vars);
    }
    if let Some(op) = config.get("operationName").cloned() {
        cfg.insert("graphqlOperationName".to_string(), op);
    }
    // GraphQL is JSON-in/JSON-out; pagination cursors don't apply.
    cfg.remove("pagination");

    let fail_on_errors = config
        .get("failOnErrors")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let response = super::synapse::execute_http_node(&Value::Object(cfg), state).await?;
    shape_response(&response, fail_on_errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A clean response yields {data, errors: [], status}.
    #[test]
    fn clean_response_shapes_data() {
        let response = json!({
            "status": 200,
            "body": { "data": { "viewer": { "login": "amla" } } },
        });
        let out = shape_response(&response, true).unwrap();
        assert_eq!(out["data"]["viewer"]["login"], json!("amla"));
        assert_eq!(out["errors"], json!([]));
        assert_eq!(out["status"], json!(200));
    }

    // GraphQL errors (HTTP 200!) fail the node by default…
    #[test]
    fn errors_fail_by_default() {
        let response = json!({
            "status": 200,
            "body": {
                "data": null,
                "errors": [ { "message": "Field 'x' doesn't exist" } ],
            },
        });
        let err = shape_response(&response, true).unwrap_err();
        assert!(err.contains("Field 'x' doesn't exist"), "got: {err}");
    }

    // …but route downstream as data when failOnErrors is off (partial data
    // responses are legal GraphQL).
    #[test]
    fn errors_pass_through_when_soft() {
        let response = json!({
            "status": 200,
            "body": {
                "data": { "a": 1 },
                "errors": [ { "message": "partial failure" } ],
            },
        });
        let out = shape_response(&response, false).unwrap();
        assert_eq!(out["data"]["a"], json!(1));
        assert_eq!(out["errors"][0]["message"], json!("partial failure"));
    }

    // A non-JSON body (text response format / proxy error page) still shapes.
    #[test]
    fn text_body_survives() {
        let response = json!({ "status": 502, "body": "Bad Gateway" });
        let out = shape_response(&response, true).unwrap();
        assert_eq!(out["data"], json!("Bad Gateway"));
        assert_eq!(out["errors"], json!([]));
    }
}
