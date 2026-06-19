use crate::state::AppState;
use crate::tools::file_handler::AgentFile;
use crate::tools::schema::{ToolDefinition, ToolSource};
use anyhow::Result;
use serde_json::{json, Value};

pub async fn execute_myelin_node(state: &AppState, config: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("save");

    match operation {
        "save" => {
            // Get binary data from config.
            // In Axon, binary data is passed in a "binary" property or similar.
            // We search for local_path in common locations.
            let binary_data = config
                .get("binary_data")
                .or_else(|| config.get("binary"))
                .ok_or(
                "No binary data found to save. Make sure the previous node outputs binary data.",
            )?;

            if binary_data.is_string() {
                return Err("Expected a binary data object, but got a string. Did you forget to 'Download' the file first using a node like Telegram? Myelin needs the physical file, not just a file_id string.".to_string());
            }

            let local_path = binary_data
                .get("local_path")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    binary_data
                        .get("data")
                        .and_then(|v| v.get("localPath"))
                        .and_then(|v| v.as_str())
                })
                .ok_or("No local_path found in binary data object. Ensure the previous node correctly staged the file.")?;

            let original_name = binary_data
                .get("original_name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    binary_data
                        .get("data")
                        .and_then(|v| v.get("fileName"))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("unnamed_file");

            let bytes = tokio::fs::read(local_path)
                .await
                .map_err(|e| format!("Failed to read file from staging: {e}"))?;

            let file_size = bytes.len();
            let file = AgentFile {
                id: String::new(),
                filename: original_name.to_string(),
                mime_type: mime_guess::from_path(original_name)
                    .first_or_octet_stream()
                    .to_string(),
                size_bytes: file_size,
                bytes,
                platform: Some("myelin".to_string()),
                chat_id: None,
            };

            let id = state
                .files
                .store_incoming(file)
                .await
                .map_err(|e| format!("Failed to save to storage: {e}"))?;

            Ok(json!({
                "id": id,
                "status": "saved",
                "filename": original_name,
                "size": file_size
            }))
        }
        "retrieve" => {
            let file_id = config
                .get("file_id")
                .and_then(|v| v.as_str())
                .ok_or("Missing file_id for retrieve")?;
            let file = state
                .files
                .read(file_id)
                .await
                .map_err(|e| format!("File not found in storage: {e}"))?;

            // Output as binary data for downstream
            // We stage it so downstream nodes can use local_path (e.g. for uploads)
            let staged_path = crate::files::stage_bytes(&file.bytes, &file.filename)
                .map_err(|e| format!("Failed to stage file for downstream: {e}"))?;

            Ok(json!({
                "binary": {
                    "original_name": file.filename,
                    "local_path": staged_path.to_string_lossy(),
                    "mime_type": file.mime_type,
                    "size": file.size_bytes
                }
            }))
        }
        _ => Err(format!("Unknown operation: {}", operation)),
    }
}

pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "myelin".to_string(),
        description: "Save binary files from previous nodes to long-term storage, or retrieve them by ID for downstream nodes. Mirrors the biological myelin sheath by protecting and storing signals.".to_string(),
        parameters: json!({
            "operation": {
                "type": "string",
                "enum": ["save", "retrieve"],
                "description": "Whether to save binary data from previous node or retrieve a file from storage."
            },
            "binary_data": {
                "type": "object",
                "description": "Incoming binary data object (used in 'save' operation). Usually provided by JSON interpolation like {{ $node['Previous Node'].binary }}"
            },
            "file_id": {
                "type": "string",
                "description": "The unique ID of the file to retrieve from storage (used in 'retrieve' operation)."
            }
        }),
        required: vec!["operation".into()],
        source: ToolSource::Internal,
        enabled: true,
        is_mutating: true,
    }
}
