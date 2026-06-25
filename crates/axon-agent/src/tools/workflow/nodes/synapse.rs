use crate::tools::http::{HttpAuth, HttpRequestParams, HttpRequestTool};
use crate::tools::workflow::try_parse_json_value;
use serde_json::{json, Value};

/// Header and query-parameter values are always strings on the wire, so they must
/// NOT be coerced to JSON numbers/bools the way body fields are. Coercing them
/// corrupts real-world values: ZIP codes ("01234" → 1234), long IDs (i64/f64
/// precision loss), version strings ("1.0" → 1.0) and tokens.
fn header_query_value(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(s.clone()),
        Value::Null => Value::String(String::new()),
        other => Value::String(other.to_string()),
    }
}

pub(crate) async fn execute_http_node(config: &Value) -> Result<Value, String> {
    let method = config
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_string();
    let url = config
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if url.is_empty() {
        return Err("URL is empty".to_string());
    }

    let authentication = config
        .get("authentication")
        .and_then(|v| v.as_str())
        .unwrap_or("none");

    let auth = if authentication != "none" {
        Some(HttpAuth {
            auth_type: authentication.to_string(),
            user: config
                .get("user")
                .and_then(|v| v.as_str())
                .map(String::from),
            password: config
                .get("password")
                .and_then(|v| v.as_str())
                .map(String::from),
            header_name: config
                .get("authHeaderName")
                .and_then(|v| v.as_str())
                .map(String::from),
            header_value: config
                .get("authHeaderValue")
                .and_then(|v| v.as_str())
                .map(String::from),
        })
    } else {
        None
    };

    let mut headers_obj = serde_json::Map::new();
    if config
        .get("sendHeaders")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let specify = config
            .get("specifyHeaders")
            .and_then(|v| v.as_str())
            .unwrap_or("keypair");
        if specify == "json" {
            if let Some(json_s) = config.get("jsonHeaders").and_then(|v| v.as_str()) {
                if let Ok(Value::Object(map)) = serde_json::from_str(json_s) {
                    for (k, v) in map {
                        headers_obj.insert(k, v);
                    }
                }
            }
        } else {
            if let Some(params) = config
                .get("headerParameters")
                .and_then(|v| v.get("parameters"))
                .and_then(|v| v.as_array())
            {
                for p in params {
                    if let (Some(name), Some(value)) =
                        (p.get("name").and_then(|v| v.as_str()), p.get("value"))
                    {
                        if !name.is_empty() {
                            headers_obj.insert(name.to_string(), header_query_value(value));
                        }
                    }
                }
            }
        }
    }

    let mut query_obj = serde_json::Map::new();
    if config
        .get("sendQuery")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let specify = config
            .get("specifyQuery")
            .and_then(|v| v.as_str())
            .unwrap_or("keypair");
        if specify == "json" {
            if let Some(json_s) = config.get("jsonQuery").and_then(|v| v.as_str()) {
                if let Ok(Value::Object(map)) = serde_json::from_str(json_s) {
                    for (k, v) in map {
                        query_obj.insert(k, v);
                    }
                }
            }
        } else {
            if let Some(params) = config
                .get("queryParameters")
                .and_then(|v| v.get("parameters"))
                .and_then(|v| v.as_array())
            {
                for p in params {
                    if let (Some(name), Some(value)) =
                        (p.get("name").and_then(|v| v.as_str()), p.get("value"))
                    {
                        if !name.is_empty() {
                            query_obj.insert(name.to_string(), header_query_value(value));
                        }
                    }
                }
            }
        }
    }

    let raw_content_type = config
        .get("contentType")
        .or_else(|| config.get("bodyContentType"))
        .and_then(|v| v.as_str())
        .unwrap_or("json");

    let content_type = raw_content_type;

    let specify_body = config
        .get("specifyBody")
        .and_then(|v| v.as_str())
        .unwrap_or("keypair");

    // Only POST/PUT/PATCH/DELETE carry a body — this matches the UI's `sendBody`
    // display options. HEAD/OPTIONS must never send one.
    let has_body_method =
        ["POST", "PUT", "PATCH", "DELETE"].contains(&method.to_uppercase().as_str());

    let body = if has_body_method
        && config
            .get("sendBody")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        match content_type {
            "json" => {
                if specify_body == "json" {
                    // jsonBody can be either a String (raw JSON) or already-parsed Object
                    config.get("jsonBody").and_then(|v| {
                        if let Some(s) = v.as_str() {
                            // Raw JSON string - parse it
                            serde_json::from_str(s).ok()
                        } else {
                            // Already parsed JSON value
                            Some(v.clone())
                        }
                    })
                } else if specify_body == "string" {
                    config
                        .get("body")
                        .and_then(|v| v.as_str())
                        .map(|b| json!(b))
                } else {
                    let mut base_obj = serde_json::Map::new();
                    if let Some(params) = config
                        .get("bodyParameters")
                        .and_then(|v| v.get("parameters"))
                        .and_then(|v| v.as_array())
                    {
                        for p in params {
                            if let (Some(name), Some(value)) =
                                (p.get("name").and_then(|v| v.as_str()), p.get("value"))
                            {
                                if !name.is_empty() {
                                    base_obj.insert(
                                        name.to_string(),
                                        try_parse_json_value(value.clone()),
                                    );
                                }
                            }
                        }
                    }
                    if base_obj.is_empty() {
                        None
                    } else {
                        Some(Value::Object(base_obj))
                    }
                }
            }
            "form-urlencoded" | "multipart-form-data" => {
                if specify_body == "string" {
                    config
                        .get("body")
                        .and_then(|v| v.as_str())
                        .map(|b| json!(b))
                } else {
                    let mut base_obj = serde_json::Map::new();
                    if let Some(params) = config
                        .get("bodyParameters")
                        .and_then(|v| v.get("parameters"))
                        .and_then(|v| v.as_array())
                    {
                        for p in params {
                            if let (Some(name), Some(value)) =
                                (p.get("name").and_then(|v| v.as_str()), p.get("value"))
                            {
                                if !name.is_empty() {
                                    if content_type == "multipart-form-data" {
                                        let p_type = p
                                            .get("parameterType")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("formData");
                                        if p_type == "formBinaryData" {
                                            base_obj.insert(
                                                name.to_string(),
                                                json!({ "_axon_file_path": value.clone() }),
                                            );
                                        } else {
                                            base_obj.insert(
                                                name.to_string(),
                                                try_parse_json_value(value.clone()),
                                            );
                                        }
                                    } else {
                                        base_obj.insert(
                                            name.to_string(),
                                            try_parse_json_value(value.clone()),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Some(Value::Object(base_obj))
                }
            }
            "raw" => config
                .get("body")
                .and_then(|v| v.as_str())
                .map(|b| json!(b)),
            _ => None,
        }
    } else {
        None
    };

    let options = config.get("options");
    tracing::info!("Synapse options raw: {:?}", options);
    tracing::info!("Synapse proxy: {:?}", options.and_then(|o| o.get("proxy")));
    let timeout_seconds = options
        .and_then(|o| o.get("timeout"))
        .and_then(|v| v.as_u64())
        .map(|v| (v / 1000).max(1)) // ms to sec; sub-second values must not truncate to 0
        .or_else(|| config.get("timeout").and_then(|v| v.as_u64()))
        .or(Some(30));

    let allow_unauthorized_certs = options
        .and_then(|o| o.get("allowUnauthorizedCerts"))
        .and_then(|v| v.as_bool())
        .or_else(|| {
            config
                .get("allowUnauthorizedCerts")
                .and_then(|v| v.as_bool())
        });

    let full_response = options
        .and_then(|o| o.get("fullResponse"))
        .or_else(|| config.get("fullResponse"))
        .and_then(|v| v.as_bool());

    // Debug: Log the body and content type
    tracing::info!(
        "Synapse body: {:?}, content_type: {:?}, specify_body: {:?}",
        body,
        content_type,
        specify_body
    );

    // Debug: Log jsonBody field specifically
    tracing::info!("Synapse jsonBody raw: {:?}", config.get("jsonBody"));

    let params = HttpRequestParams {
        method,
        url,
        headers: if headers_obj.is_empty() {
            None
        } else {
            Some(Value::Object(headers_obj))
        },
        query: if query_obj.is_empty() {
            None
        } else {
            Some(Value::Object(query_obj))
        },
        body,
        auth,
        timeout_seconds,
        response_format: config
            .get("responseFormat")
            .and_then(|v| v.as_str())
            .map(String::from),
        limit: None,
        // Proxy can be at top level or in options
        proxy: config
            .get("proxy")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| {
                options
                    .and_then(|o| o.get("proxy"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            }),
        send_binary_data: Some(content_type == "binaryData"),
        binary_property: config
            .get("inputDataFieldName")
            .and_then(|v| v.as_str())
            .map(String::from),
        body_content_type: Some(content_type.to_string()),
        stealth_headers: config.get("stealthHeaders").and_then(|v| v.as_bool()),
        raw_content_type: config
            .get("rawContentType")
            .and_then(|v| v.as_str())
            .map(String::from),
        allow_unauthorized_certs,
        full_response,
        data_cleaner: options
            .and_then(|o| o.get("dataCleaner"))
            .and_then(|v| v.as_bool())
            .or_else(|| config.get("dataCleaner").and_then(|v| v.as_bool())),
        always_output_binary: options
            .and_then(|o| o.get("alwaysOutputBinary"))
            .and_then(|v| v.as_bool())
            .or_else(|| config.get("alwaysOutputBinary").and_then(|v| v.as_bool())),
        json_body: None,
        specify_body: None,
        header_parameters: None,
    };

    let tool = HttpRequestTool::new();
    match tool.request(params).await {
        Ok(resp) => serde_json::to_value(resp).map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}
