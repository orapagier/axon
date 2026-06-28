use crate::state::AppState;
use crate::tools::file_handler::AgentFile;
use crate::tools::schema::{ToolDefinition, ToolSource};
use crate::tools::telegram::{binary_descriptor, extract_file_descriptor};
use anyhow::Result;
use base64::Engine;
use serde_json::{json, Value};

const NO_INPUT_ERR: &str = "Myelin save: no file to save. Provide a binary object from a \
     previous node (e.g. {{ $node[\"Telegram\"].binary }}), a server file path \
     (e.g. /data/files/report.pdf), or base64 'file_bytes' + 'file_name'.";

/// Myelin — long-term local file storage for workflows.
///
/// Save ANY file produced upstream (a Telegram/HTTP download, a literal server
/// path, or raw base64) into Axon's content-addressed store, then retrieve it
/// later by ID as a standardized binary object that any downstream node
/// (Telegram, Synapse/HTTP multipart, …) can consume. Also lists and deletes
/// stored files. Mirrors the biological myelin sheath: it protects and stores
/// the signal so it can be relayed again later.
pub async fn execute_myelin_node(state: &AppState, config: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("save");

    match operation {
        "save" => save_file(state, config).await,
        "retrieve" => retrieve_file(state, config).await,
        "list" => list_files(state).await,
        "delete" => delete_file(state, config).await,
        other => Err(format!(
            "Myelin: unknown operation '{}' (expected save | retrieve | list | delete)",
            other
        )),
    }
}

/// Resolve raw bytes + filename + optional mime from whatever shape upstream gave us.
async fn resolve_input_bytes(config: &Value) -> Result<(Vec<u8>, String, Option<String>), String> {
    // 1. Inline base64 bytes (nodes that emit file_bytes + file_name).
    if let Some(b64) = config
        .get("file_bytes")
        .or_else(|| config.get("base64"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| format!("Myelin save: invalid base64 in 'file_bytes': {e}"))?;
        let name = config
            .get("file_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("unnamed_file")
            .to_string();
        return Ok((bytes, name, None));
    }

    // 2. A binary object or path string from a previous node. Pick the first
    //    non-empty known field, else fall back to the whole config (it may itself
    //    be the descriptor). `extract_file_descriptor` tolerates string paths,
    //    camelCase/snake_case keys, and nested binary/data/file wrappers.
    let file_val = ["binary_data", "binary", "file", "data"]
        .iter()
        .filter_map(|k| config.get(*k))
        .find(|v| !(v.is_null() || (v.is_string() && v.as_str() == Some(""))))
        .unwrap_or(config);

    let (local_path, name, mime) =
        extract_file_descriptor(file_val).ok_or_else(|| NO_INPUT_ERR.to_string())?;

    let bytes = tokio::fs::read(&local_path)
        .await
        .map_err(|e| format!("Myelin save: failed to read '{local_path}': {e}"))?;

    let file_name = name
        .or_else(|| {
            std::path::Path::new(&local_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "unnamed_file".to_string());

    Ok((bytes, file_name, mime))
}

async fn save_file(state: &AppState, config: &Value) -> Result<Value, String> {
    let (bytes, file_name, mime_type) = resolve_input_bytes(config).await?;

    if bytes.is_empty() {
        return Err("Myelin save: resolved file is empty (0 bytes).".to_string());
    }

    let size = bytes.len();
    let mime = mime_type.unwrap_or_else(|| {
        mime_guess::from_path(&file_name)
            .first_or_octet_stream()
            .to_string()
    });

    let file = AgentFile {
        id: String::new(),
        filename: file_name.clone(),
        mime_type: mime.clone(),
        size_bytes: size,
        bytes,
        platform: Some("myelin".to_string()),
        chat_id: None,
    };

    // store_incoming saves under the file's name: a same-name/same-size file is
    // overwritten (assumed identical), a same-name file of a different size is
    // kept under a numbered variant. The id is the SHA-256 of the bytes, so
    // saving identical bytes returns the same id. Returns (id, path).
    let (id, stored_path) = state
        .files
        .store_incoming(file)
        .await
        .map_err(|e| format!("Myelin save: failed to write to storage: {e}"))?;

    Ok(json!({
        "id": id.clone(),
        "file_id": id,
        "status": "saved",
        "filename": file_name,
        "mime_type": mime,
        "size": size,
        // A standardized descriptor pointing at the stored copy, so this output
        // can be fed straight into a downstream consumer without a retrieve.
        "binary": binary_descriptor(&stored_path, &file_name, &mime, size),
    }))
}

async fn retrieve_file(state: &AppState, config: &Value) -> Result<Value, String> {
    let file_id = config
        .get("file_id")
        .or_else(|| config.get("id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("Myelin retrieve: 'file_id' is required")?;

    let file = state
        .files
        .read(file_id)
        .await
        .map_err(|e| format!("Myelin retrieve: file '{file_id}' not found: {e}"))?;

    // Stage a fresh physical copy so downstream nodes can read it via local_path.
    let staged = crate::files::stage_bytes(&file.bytes, &file.filename)
        .map_err(|e| format!("Myelin retrieve: failed to stage file: {e}"))?;
    let staged_path = staged.to_string_lossy().to_string();
    let size = file.size_bytes;

    Ok(json!({
        "id": file.id.clone(),
        "file_id": file.id.clone(),
        "filename": file.filename.clone(),
        "mime_type": file.mime_type.clone(),
        "size": size,
        "binary": binary_descriptor(&staged_path, &file.filename, &file.mime_type, size),
    }))
}

async fn list_files(state: &AppState) -> Result<Value, String> {
    let files = state
        .files
        .list("incoming")
        .map_err(|e| format!("Myelin list: {e}"))?;

    let items: Vec<Value> = files
        .iter()
        .map(|f| {
            json!({
                "id": f.id,
                "file_id": f.id,
                "filename": f.filename,
                "mime_type": f.mime_type,
                "size": f.size_bytes,
                "created_at": f.created_at,
            })
        })
        .collect();
    let count = items.len();

    Ok(json!({ "files": items, "count": count }))
}

async fn delete_file(state: &AppState, config: &Value) -> Result<Value, String> {
    let file_id = config
        .get("file_id")
        .or_else(|| config.get("id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("Myelin delete: 'file_id' is required")?;

    state
        .files
        .delete(file_id)
        .await
        .map_err(|e| format!("Myelin delete: {e}"))?;

    Ok(json!({ "status": "deleted", "id": file_id, "file_id": file_id }))
}

pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "myelin".to_string(),
        description: "Save any file from a previous node (binary object, server path, or base64) to long-term local storage, or retrieve / list / delete stored files by ID. Retrieved files are emitted as a standardized binary object usable by any downstream node. Mirrors the biological myelin sheath by protecting and storing signals.".to_string(),
        parameters: json!({
            "operation": {
                "type": "string",
                "enum": ["save", "retrieve", "list", "delete"],
                "description": "save: store an upstream file. retrieve: fetch a stored file by id for downstream use. list: enumerate stored files. delete: remove a stored file by id."
            },
            "binary_data": {
                "type": "object",
                "description": "Incoming binary object for 'save'. Usually {{ $node['Previous Node'].binary }}. A server path string or base64 is also accepted."
            },
            "file_bytes": {
                "type": "string",
                "description": "Optional base64-encoded file content for 'save' (used with file_name when there is no staged file on disk)."
            },
            "file_name": {
                "type": "string",
                "description": "Filename to use when saving from base64 'file_bytes'."
            },
            "file_id": {
                "type": "string",
                "description": "The unique ID of the file to retrieve or delete."
            }
        }),
        required: vec!["operation".into()],
        source: ToolSource::Internal,
        enabled: true,
        is_mutating: true,
    }
}
