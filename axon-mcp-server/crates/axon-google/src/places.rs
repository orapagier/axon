use crate::auth::access_token;
use anyhow::{anyhow, bail, Result};
use axon_core::AppState;
use rmcp::model::Tool;
use serde_json::{json, Map, Value};
use std::sync::Arc;

const BASE: &str = "https://places.googleapis.com";

struct PlaceAction {
    tool: &'static str,
    description: &'static str,
    method: &'static str,
    path: &'static str,
    has_body: bool,
    requires_field_mask: bool,
    path_params: &'static [&'static str],
    returns_binary: bool,
}

const ACTIONS: &[PlaceAction] = &[
    PlaceAction {
        tool: "gplaces_autocomplete",
        description: "Places API places.autocomplete: get place predictions for user input.",
        method: "POST",
        path: "/v1/places:autocomplete",
        has_body: true,
        requires_field_mask: false,
        path_params: &[],
        returns_binary: false,
    },
    PlaceAction {
        tool: "gplaces_get",
        description:
            "Places API places.get: fetch details for a place resource name (e.g. places/PLACE_ID).",
        method: "GET",
        path: "/v1/{name}",
        has_body: false,
        requires_field_mask: true,
        path_params: &["name"],
        returns_binary: false,
    },
    PlaceAction {
        tool: "gplaces_search_nearby",
        description: "Places API places.searchNearby: nearby place search.",
        method: "POST",
        path: "/v1/places:searchNearby",
        has_body: true,
        requires_field_mask: true,
        path_params: &[],
        returns_binary: false,
    },
    PlaceAction {
        tool: "gplaces_search_text",
        description: "Places API places.searchText: text query place search.",
        method: "POST",
        path: "/v1/places:searchText",
        has_body: true,
        requires_field_mask: true,
        path_params: &[],
        returns_binary: false,
    },
    PlaceAction {
        tool: "gplaces_photos_get_media",
        description: "Places API places.photos.getMedia: get photo media metadata or image bytes.",
        method: "GET",
        path: "/v1/{name}/media",
        has_body: false,
        requires_field_mask: false,
        path_params: &["name"],
        returns_binary: true,
    },
];

pub fn tool_list() -> Vec<Tool> {
    ACTIONS.iter().map(tool_from_spec).collect()
}

pub async fn try_call(
    state: &AppState,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Option<Value>> {
    let Some(spec) = ACTIONS.iter().find(|spec| spec.tool == name) else {
        return Ok(None);
    };
    Ok(Some(call_action(state, spec, args).await?))
}

fn tool_from_spec(spec: &PlaceAction) -> Tool {
    let mut properties = Map::new();

    properties.insert(
        "params".to_string(),
        json!({
            "type": "object",
            "description": "Additional query parameters as JSON object."
        }),
    );

    if spec.has_body {
        properties.insert(
            "body".to_string(),
            json!({
                "type": "object",
                "description": "Request body JSON object."
            }),
        );
    }

    if spec.requires_field_mask {
        properties.insert(
            "field_mask".to_string(),
            json!({
                "type": "string",
                "default": "*",
                "description": "Places field mask sent as X-Goog-FieldMask header."
            }),
        );
    }

    properties.insert(
        "api_key".to_string(),
        json!({
            "type": "string",
            "description": "Optional Places API key. If omitted, server uses google.places_api_key or OAuth token."
        }),
    );

    if spec.returns_binary {
        properties.insert(
            "download_filename".to_string(),
            json!({
                "type": "string",
                "description": "Optional filename when response is image bytes."
            }),
        );
    }

    for key in spec.path_params {
        properties.insert(
            (*key).to_string(),
            json!({
                "type": "string",
                "description": format!("Path parameter '{key}' required by this action.")
            }),
        );
    }

    let required: Vec<String> = spec.path_params.iter().map(|s| s.to_string()).collect();

    let schema = json!({
        "type": "object",
        "properties": properties,
        "required": required,
    });
    let input_schema = schema.as_object().cloned().unwrap_or_default();

    Tool {
        name: spec.tool.into(),
        description: spec.description.into(),
        input_schema: Arc::new(input_schema),
    }
}

async fn call_action(
    state: &AppState,
    spec: &PlaceAction,
    args: &Map<String, Value>,
) -> Result<Value> {
    let path = build_path(spec, args)?;
    let url = format!("{BASE}{path}");

    let mut params = parse_json_object_arg(args, "params")?;
    if spec.tool == "gplaces_photos_get_media" && !params.contains_key("skipHttpRedirect") {
        params.insert("skipHttpRedirect".to_string(), Value::Bool(true));
    }

    let query: Vec<(String, String)> = params
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.clone(), value_to_query(v)))
        .collect();

    let body = if spec.has_body {
        parse_json_value_arg(args, "body")?
    } else {
        None
    };

    let api_key_from_args = opt_string_arg(args, "api_key").filter(|s| !s.trim().is_empty());
    let api_key_from_creds = {
        let storage = state.storage.read().await;
        storage
            .credentials
            .google
            .as_ref()
            .map(|g| g.places_api_key.clone())
            .filter(|k| !k.trim().is_empty())
    };
    let api_key = api_key_from_args.or(api_key_from_creds);

    let mut req = match spec.method {
        "GET" => state.client.get(&url),
        "POST" => state.client.post(&url),
        "PUT" => state.client.put(&url),
        "DELETE" => state.client.delete(&url),
        other => bail!("unsupported HTTP method '{other}' for {}", spec.tool),
    }
    .query(&query);

    if let Some(key) = api_key {
        req = req.header("X-Goog-Api-Key", key);
    } else {
        let token = access_token(state).await?;
        req = req.bearer_auth(token);
    }

    if spec.requires_field_mask {
        let field_mask = opt_string_arg(args, "field_mask")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "*".to_string());
        req = req.header("X-Goog-FieldMask", field_mask);
    }

    if let Some(body_json) = body {
        req = req.json(&body_json);
    }

    let response = req.send().await?.error_for_status()?;

    if !spec.returns_binary {
        if response.status().as_u16() == 204 {
            return Ok(json!({ "success": true }));
        }
        return Ok(response.json().await?);
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if content_type.contains("application/json") {
        return Ok(response.json().await?);
    }

    let filename = opt_string_arg(args, "download_filename")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "places_photo.bin".to_string());
    let download_dir = std::path::PathBuf::from("/data/files");
    std::fs::create_dir_all(&download_dir)?;
    let path = download_dir.join(&filename);
    let bytes = response.bytes().await?;
    std::fs::write(&path, &bytes)?;

    Ok(json!({
        "success": true,
        "name": filename,
        "bytes": bytes.len(),
        "file_path": path.to_string_lossy(),
        "message": "Photo media downloaded. Use file_path to access the image."
    }))
}

fn build_path(spec: &PlaceAction, args: &Map<String, Value>) -> Result<String> {
    let mut path = spec.path.to_string();
    for key in spec.path_params {
        let raw = required_string_arg(args, key)?;
        let escaped = if *key == "name" {
            // Name params for Places often contain slashes (e.g. places/PLACE_ID).
            raw.trim().to_string()
        } else {
            url_escape(raw)
        };
        path = path.replace(&format!("{{{key}}}"), &escaped);
    }
    Ok(path)
}

fn parse_json_object_arg(args: &Map<String, Value>, key: &str) -> Result<Map<String, Value>> {
    let Some(value) = args.get(key) else {
        return Ok(Map::new());
    };

    match value {
        Value::Object(map) => Ok(map.clone()),
        Value::String(s) => {
            if s.trim().is_empty() {
                return Ok(Map::new());
            }
            let parsed: Value = serde_json::from_str(s)
                .map_err(|e| anyhow!("{key} must be valid JSON object string: {e}"))?;
            let Some(obj) = parsed.as_object() else {
                bail!("{key} must be a JSON object");
            };
            Ok(obj.clone())
        }
        _ => bail!("{key} must be a JSON object or JSON object string"),
    }
}

fn parse_json_value_arg(args: &Map<String, Value>, key: &str) -> Result<Option<Value>> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };

    match value {
        Value::Null => Ok(None),
        Value::String(s) => {
            if s.trim().is_empty() {
                return Ok(None);
            }
            match serde_json::from_str::<Value>(s) {
                Ok(v) => Ok(Some(v)),
                Err(_) => Ok(Some(Value::String(s.clone()))),
            }
        }
        other => Ok(Some(other.clone())),
    }
}

fn required_string_arg<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("missing required argument '{key}'"))
}

fn opt_string_arg(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

fn value_to_query(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(arr) => arr.iter().map(value_to_query).collect::<Vec<_>>().join(","),
        Value::Object(_) => serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
        Value::Null => "".to_string(),
    }
}

fn url_escape(raw: &str) -> String {
    url::form_urlencoded::byte_serialize(raw.as_bytes()).collect()
}
