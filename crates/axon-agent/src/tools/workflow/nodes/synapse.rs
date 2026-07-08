use crate::state::AppState;
use crate::tools::http::{HttpAuth, HttpRequestParams, HttpRequestTool};
use crate::tools::workflow::try_parse_json_value;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// A response "page" counts as empty (stop paginating) when it has no data.
fn is_empty_body(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// Resolve a dot-path (e.g. "paging.next") against a JSON value.
fn get_by_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Decide whether pagination should stop. When a data-field path is set, the
/// page is "empty" when that field's array is empty/absent — so APIs that wrap
/// results (e.g. `{"items": [], "total": 0}`) stop correctly instead of running
/// to the page cap because the envelope is never literally empty.
fn page_is_empty(body: &Value, data_field: Option<&str>) -> bool {
    match data_field {
        Some(f) => match get_by_path(body, f) {
            Some(Value::Array(a)) => a.is_empty(),
            Some(v) => is_empty_body(v),
            None => true,
        },
        None => is_empty_body(body),
    }
}

/// Extract the items of one page into `out`. With a data-field path, pull that
/// array (or scalar); otherwise flatten top-level arrays and pass objects through.
fn collect_page_items(body: &Value, data_field: Option<&str>, out: &mut Vec<Value>) {
    let target = match data_field {
        Some(f) => get_by_path(body, f),
        None => Some(body),
    };
    match target {
        Some(Value::Array(arr)) => out.extend(arr.iter().cloned()),
        Some(other) => out.push(other.clone()),
        None => {}
    }
}

/// Parse an RFC 5988 `Link` header for the URL with `rel="next"` (GitHub-style).
fn parse_link_next(headers: &Value) -> Option<String> {
    let link = headers
        .get("link")
        .or_else(|| headers.get("Link"))
        .and_then(|v| v.as_str())?;
    for part in link.split(',') {
        let is_next = part.contains("rel=\"next\"") || part.contains("rel=next");
        if is_next {
            let a = part.find('<')?;
            let b = part.find('>')?;
            if b > a + 1 {
                return Some(part[a + 1..b].to_string());
            }
        }
    }
    None
}

/// In-process cache of OAuth2 client-credentials / refresh-token results, keyed
/// by the grant identity. Avoids a token round-trip on every request; entries
/// are treated as expired 60s early so a request never rides a just-expired token.
static OAUTH2_TOKEN_CACHE: Lazy<Mutex<HashMap<String, (String, i64)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Fetch (or reuse a cached) OAuth2 access token for the generic OAuth2
/// credential type. Supports the `client_credentials` and `refresh_token` grants.
async fn fetch_oauth2_token(config: &Value) -> Result<String, String> {
    let grant = config
        .get("oauth2GrantType")
        .and_then(|v| v.as_str())
        .unwrap_or("clientCredentials");
    let token_url = config
        .get("oauth2TokenUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if token_url.is_empty() {
        return Err("OAuth2 Token URL is required".to_string());
    }
    let client_id = config
        .get("oauth2ClientId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let client_secret = config
        .get("oauth2ClientSecret")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let scope = config
        .get("oauth2Scope")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let cache_key = format!("{grant}|{token_url}|{client_id}|{scope}");
    let now = chrono::Utc::now().timestamp_millis();
    if let Ok(cache) = OAUTH2_TOKEN_CACHE.lock() {
        if let Some((tok, exp)) = cache.get(&cache_key) {
            if *exp - 60_000 > now {
                return Ok(tok.clone());
            }
        }
    }

    let client = reqwest::Client::new();
    let (access_token, expires_at) = match grant {
        "refreshToken" => {
            let refresh = config
                .get("oauth2RefreshToken")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if refresh.is_empty() {
                return Err("OAuth2 Refresh Token is required for the refresh_token grant".into());
            }
            let extra: Vec<(&str, &str)> = if scope.is_empty() {
                vec![]
            } else {
                vec![("scope", scope.as_str())]
            };
            let tok = axon_core::oauth::refresh_token(
                &client,
                &token_url,
                &client_id,
                &client_secret,
                refresh,
                &extra,
            )
            .await
            .map_err(|e| e.to_string())?;
            (tok.access_token, tok.expires_at)
        }
        _ => {
            // client_credentials
            let mut form = vec![
                ("grant_type", "client_credentials"),
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
            ];
            if !scope.is_empty() {
                form.push(("scope", scope.as_str()));
            }
            let resp = client
                .post(&token_url)
                .form(&form)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let body: Value = resp.json().await.map_err(|e| e.to_string())?;
            let token = body
                .get("access_token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    let desc = body
                        .get("error_description")
                        .or_else(|| body.get("error"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("no access_token in response");
                    format!("token endpoint returned {status}: {desc}")
                })?
                .to_string();
            let expires_in = body
                .get("expires_in")
                .and_then(|v| v.as_i64())
                .unwrap_or(3600);
            (token, now + expires_in * 1000)
        }
    };

    if let Ok(mut cache) = OAUTH2_TOKEN_CACHE.lock() {
        cache.insert(cache_key, (access_token.clone(), expires_at));
    }
    Ok(access_token)
}

/// Resolve a live access token for a Google / Microsoft / Facebook account the
/// user has already connected in Axon. Tokens are auto-refreshed by the
/// respective auth module. Bridges to the shared `axon_core::AppState` that the
/// in-process MCP backend owns (where OAuth tokens live).
async fn resolve_connected_account_token(
    state: &AppState,
    provider: &str,
) -> Result<String, String> {
    let core = state
        .mcp
        .inprocess_state()
        .await
        .ok_or_else(|| "connected-accounts backend is not available".to_string())?;
    match provider {
        "google" => axon_google::auth::access_token(&core)
            .await
            .map_err(|e| e.to_string()),
        "microsoft" => axon_microsoft::auth::access_token(&core)
            .await
            .map_err(|e| e.to_string()),
        "facebook" => axon_facebook::auth::page_token(&core)
            .await
            .map_err(|e| e.to_string()),
        other => Err(format!("unknown connected-account provider '{other}'")),
    }
}

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

/// When `cssExtract` is on, scrape the response with the same CSS-selector
/// engine as the HTML Extract node — saves wiring a separate HTML Extract node
/// after a scrape-only Synapse call. Reuses `html_extract::execute` directly:
/// Synapse's config carries the same `extractionValues`/`trimValues` field
/// names, and since it has no `html` field of its own, `html_extract`'s
/// html-source probing falls through to the response we pass as its `input`
/// (which is exactly the `{ status, body, headers, ... }` shape it already
/// knows how to read `body` from). `includeInputFields` is forced on so the
/// extracted keys land merged onto the response, not replacing it.
fn apply_css_extraction(config: &Value, response: Value) -> Result<Value, String> {
    if !config
        .get("cssExtract")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Ok(response);
    }
    let mut extract_cfg = config.clone();
    if let Some(o) = extract_cfg.as_object_mut() {
        o.insert("includeInputFields".to_string(), json!(true));
    }
    super::html_extract::execute(&extract_cfg, &response)
}

pub(crate) async fn execute_http_node(config: &Value, state: &AppState) -> Result<Value, String> {
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

    // The UI stores the mode in `authentication` (none / genericCredentialType /
    // connectedAccount) and the concrete scheme in `genericAuthType`
    // (httpBasicAuth / httpHeaderAuth / httpBearerAuth / httpQueryAuth / oAuth2).
    // http.rs matches on "basicAuth" / "headerAuth" / "bearerAuth", so translate
    // here. Query Auth is applied to `query_obj` further down. All secret fields
    // may be typed inline OR supplied by a picked credential — interpolate_config
    // merges the credential's stored fields into this config before we run.
    let mut auth: Option<HttpAuth> = if authentication == "genericCredentialType" {
        let scheme = config
            .get("genericAuthType")
            .and_then(|v| v.as_str())
            .unwrap_or("httpBasicAuth");
        match scheme {
            "httpBasicAuth" => Some(HttpAuth {
                auth_type: "basicAuth".to_string(),
                user: config
                    .get("user")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                password: config
                    .get("password")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                header_name: None,
                header_value: None,
            }),
            "httpHeaderAuth" => Some(HttpAuth {
                auth_type: "headerAuth".to_string(),
                user: None,
                password: None,
                header_name: config
                    .get("authHeaderName")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                header_value: config
                    .get("authHeaderValue")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            }),
            "httpBearerAuth" => config
                .get("authBearerToken")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|token| HttpAuth {
                    auth_type: "bearerAuth".to_string(),
                    user: None,
                    password: None,
                    header_name: None,
                    header_value: Some(token.to_string()),
                }),
            // httpQueryAuth → handled in query_obj; oAuth2 → fetched just below.
            _ => None,
        }
    } else {
        None
    };

    // OAuth2 generic credential: exchange for (or reuse a cached) access token,
    // then send it as a Bearer header. Fail loudly — a missing token is not
    // something to silently retry unauthenticated.
    if authentication == "genericCredentialType"
        && config.get("genericAuthType").and_then(|v| v.as_str()) == Some("oAuth2")
    {
        let token = fetch_oauth2_token(config)
            .await
            .map_err(|e| format!("OAuth2 token request failed: {e}"))?;
        auth = Some(HttpAuth {
            auth_type: "bearerAuth".to_string(),
            user: None,
            password: None,
            header_name: None,
            header_value: Some(token),
        });
    }

    // Connected Account: reuse the (auto-refreshed) access token Axon already
    // holds for a Google / Microsoft / Facebook login, sent as a Bearer header.
    // This unlocks every API on those platforms without a dedicated node.
    if authentication == "connectedAccount" {
        let provider = config
            .get("connectedAccountProvider")
            .and_then(|v| v.as_str())
            .unwrap_or("google");
        let token = resolve_connected_account_token(state, provider)
            .await
            .map_err(|e| format!("Connected account '{provider}' unavailable: {e}"))?;
        auth = Some(HttpAuth {
            auth_type: "bearerAuth".to_string(),
            user: None,
            password: None,
            header_name: None,
            header_value: Some(token),
        });
    }

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

    // Query Auth (Generic Credential Type → Query Auth): append the credential as a
    // query-string parameter. Applied regardless of "Send Query Parameters".
    if authentication == "genericCredentialType"
        && config.get("genericAuthType").and_then(|v| v.as_str()) == Some("httpQueryAuth")
    {
        if let Some(name) = config.get("authQueryName").and_then(|v| v.as_str()) {
            if !name.is_empty() {
                let value = config.get("authQueryValue").cloned().unwrap_or(Value::Null);
                query_obj.insert(name.to_string(), header_query_value(&value));
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
                } else if specify_body == "json" {
                    // A JSON object whose key/values become the urlencoded form fields.
                    config.get("jsonBody").and_then(|v| {
                        if let Some(s) = v.as_str() {
                            serde_json::from_str(s).ok()
                        } else {
                            Some(v.clone())
                        }
                    })
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
            "graphql" => {
                // Compose the standard { "query": ..., "variables": {...} } POST
                // body. Variables may be a JSON string or an already-parsed object.
                let query = config
                    .get("graphqlQuery")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut gql = serde_json::Map::new();
                gql.insert("query".to_string(), json!(query));
                let vars = match config.get("graphqlVariables") {
                    Some(Value::String(s)) if !s.trim().is_empty() => serde_json::from_str(s).ok(),
                    Some(Value::Object(_)) => config.get("graphqlVariables").cloned(),
                    _ => None,
                };
                if let Some(v) = vars {
                    gql.insert("variables".to_string(), v);
                }
                Some(Value::Object(gql))
            }
            _ => None,
        }
    } else {
        None
    };

    let options = config.get("options");
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
        // Response Format lives inside the Options collection in the UI; fall back
        // to a top-level field for older saved configs.
        response_format: options
            .and_then(|o| o.get("responseFormat"))
            .and_then(|v| v.as_str())
            .or_else(|| config.get("responseFormat").and_then(|v| v.as_str()))
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
        keep_links: options
            .and_then(|o| o.get("keepLinks"))
            .and_then(|v| v.as_bool())
            .or_else(|| config.get("keepLinks").and_then(|v| v.as_bool())),
        always_output_binary: options
            .and_then(|o| o.get("alwaysOutputBinary"))
            .and_then(|v| v.as_bool())
            .or_else(|| config.get("alwaysOutputBinary").and_then(|v| v.as_bool())),
        json_body: None,
        specify_body: None,
        header_parameters: None,
        follow_redirects: options
            .and_then(|o| o.get("followRedirects"))
            .and_then(|v| v.as_bool()),
        max_redirects: options
            .and_then(|o| o.get("maxRedirects"))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize),
        retry_on_fail: options
            .and_then(|o| o.get("retryOnFail"))
            .and_then(|v| v.as_bool()),
        max_tries: options
            .and_then(|o| o.get("maxTries"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        retry_interval_ms: options
            .and_then(|o| o.get("retryInterval"))
            .and_then(|v| v.as_u64()),
        fail_on_error_status: options
            .and_then(|o| o.get("failOnErrorStatus"))
            .and_then(|v| v.as_bool())
            .or_else(|| config.get("failOnErrorStatus").and_then(|v| v.as_bool())),
    };

    let tool = HttpRequestTool::new();

    // Single request (the common case).
    if !config
        .get("pagination")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return match tool.request(params).await {
            Ok(resp) => serde_json::to_value(resp)
                .map_err(|e| e.to_string())
                .and_then(|v| apply_css_extraction(config, v)),
            Err(e) => Err(e.to_string()),
        };
    }

    // --- Pagination: fetch pages until a stop condition or the page cap. ---
    let mode = config
        .get("paginationMode")
        .and_then(|v| v.as_str())
        .unwrap_or("updateParameter");
    let max_pages = config
        .get("paginationMaxPages")
        .and_then(|v| v.as_u64())
        .unwrap_or(100)
        .max(1);
    let interval_ms = config
        .get("paginationInterval")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // Optional dot-path to the array of results inside each page. Drives both the
    // stop condition and the flattened `items` output, so APIs that wrap results
    // in an envelope (e.g. `{"items": [...], "total": N}`) paginate correctly.
    let data_field = config
        .get("paginationDataField")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut pages: Vec<Value> = Vec::new();
    let mut page: u64 = 0;

    match mode {
        "nextUrl" | "header" => {
            // nextUrl: read the next-page URL from a body field. header: read it
            // from the RFC 5988 Link header (rel="next"), GitHub-style.
            let field = config
                .get("paginationNextUrlField")
                .and_then(|v| v.as_str())
                .unwrap_or("next")
                .to_string();
            let mut next_params = params.clone();
            loop {
                page += 1;
                if page > max_pages {
                    break;
                }
                let resp = match tool.request(next_params.clone()).await {
                    Ok(r) => r,
                    Err(e) => {
                        if pages.is_empty() {
                            return Err(e.to_string());
                        }
                        break;
                    }
                };
                let next_url = if mode == "header" {
                    parse_link_next(&resp.headers)
                } else {
                    get_by_path(&resp.body, &field)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                };
                pages.push(resp.body);
                match next_url {
                    Some(u) => {
                        next_params.url = u;
                        // The next URL already carries its own query string.
                        next_params.query = None;
                    }
                    None => break,
                }
                if interval_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                }
            }
        }
        "cursor" => {
            // Cursor / token pagination (Notion, Slack, Stripe): read a cursor
            // token from each response and send it back on the next request as a
            // query param or a body field. Stop when the token is absent/empty.
            let cursor_field = config
                .get("paginationCursorField")
                .and_then(|v| v.as_str())
                .unwrap_or("next_cursor")
                .to_string();
            let cursor_param = config
                .get("paginationCursorParameter")
                .and_then(|v| v.as_str())
                .unwrap_or("cursor")
                .to_string();
            let cursor_in = config
                .get("paginationCursorIn")
                .and_then(|v| v.as_str())
                .unwrap_or("query");
            let mut cursor: Option<String> = None;
            loop {
                page += 1;
                if page > max_pages {
                    break;
                }
                let mut p = params.clone();
                if let Some(c) = &cursor {
                    if cursor_in == "body" {
                        let mut b = p
                            .body
                            .as_ref()
                            .and_then(|v| v.as_object())
                            .cloned()
                            .unwrap_or_default();
                        b.insert(cursor_param.clone(), Value::String(c.clone()));
                        p.body = Some(Value::Object(b));
                    } else {
                        let mut q = p
                            .query
                            .as_ref()
                            .and_then(|v| v.as_object())
                            .cloned()
                            .unwrap_or_default();
                        q.insert(cursor_param.clone(), Value::String(c.clone()));
                        p.query = Some(Value::Object(q));
                    }
                }
                let resp = match tool.request(p).await {
                    Ok(r) => r,
                    Err(e) => {
                        if pages.is_empty() {
                            return Err(e.to_string());
                        }
                        break;
                    }
                };
                let next_cursor = get_by_path(&resp.body, &cursor_field)
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .filter(|s| !s.is_empty());
                pages.push(resp.body);
                match next_cursor {
                    Some(c) => cursor = Some(c),
                    None => break,
                }
                if interval_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                }
            }
        }
        _ => {
            // updateParameter: bump a query parameter each page, stop on an empty page.
            let param_name = config
                .get("paginationParameterName")
                .and_then(|v| v.as_str())
                .unwrap_or("page")
                .to_string();
            let start = config
                .get("paginationParameterStart")
                .and_then(|v| v.as_i64())
                .unwrap_or(1);
            let increment = config
                .get("paginationParameterIncrement")
                .and_then(|v| v.as_i64())
                .unwrap_or(1)
                .max(1);
            let mut value = start;
            loop {
                page += 1;
                if page > max_pages {
                    break;
                }
                let mut p = params.clone();
                let mut q = p
                    .query
                    .as_ref()
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                q.insert(param_name.clone(), Value::String(value.to_string()));
                p.query = Some(Value::Object(q));

                let resp = match tool.request(p).await {
                    Ok(r) => r,
                    Err(e) => {
                        if pages.is_empty() {
                            return Err(e.to_string());
                        }
                        break;
                    }
                };
                let empty = page_is_empty(&resp.body, data_field);
                pages.push(resp.body);
                if empty {
                    break;
                }
                value += increment;
                if interval_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                }
            }
        }
    }

    // Flatten each page's items into a single `items` list for easy downstream
    // use (respecting the data-field path), while preserving raw bodies in `pages`.
    let mut items: Vec<Value> = Vec::new();
    for b in &pages {
        collect_page_items(b, data_field, &mut items);
    }

    Ok(json!({
        "items": items,
        "pages": pages,
        "page_count": pages.len(),
    }))
}

#[cfg(test)]
mod css_extraction_tests {
    use super::apply_css_extraction;
    use serde_json::json;

    // Off by default: the response passes through untouched.
    #[test]
    fn css_extract_off_leaves_response_unchanged() {
        let config = json!({});
        let response = json!({ "status": 200, "body": "<h1>Hi</h1>" });
        let out = apply_css_extraction(&config, response.clone()).unwrap();
        assert_eq!(out, response);
    }

    // On: scrapes the response body and merges the extracted keys onto the
    // response object, keeping status/headers/body alongside them.
    #[test]
    fn css_extract_on_merges_onto_response() {
        let config = json!({
            "cssExtract": true,
            "extractionValues": { "parameters": [
                { "key": "title", "cssSelector": "h1" }
            ]},
        });
        let response = json!({ "status": 200, "body": "<h1>Hello</h1>" });
        let out = apply_css_extraction(&config, response).unwrap();
        assert_eq!(out["status"], json!(200));
        assert_eq!(out["title"], json!("Hello"));
    }

    // A rule error (e.g. no extraction rows configured) surfaces as a node error.
    #[test]
    fn css_extract_on_without_rules_errors() {
        let config = json!({ "cssExtract": true });
        let response = json!({ "status": 200, "body": "<h1>Hi</h1>" });
        assert!(apply_css_extraction(&config, response).is_err());
    }
}
