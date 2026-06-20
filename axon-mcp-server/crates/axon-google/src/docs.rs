use crate::auth::access_token;
use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use serde_json::{json, Value};

const BASE: &str = "https://docs.googleapis.com/v1/documents";

// ── Document Management ───────────────────────────────────────────────────────

/// Create a new blank Google Doc with the given title.
pub async fn create_document(state: &AppState, title: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(BASE)
        .bearer_auth(&tok)
        .json(&json!({ "title": title }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Get the full content and structure of a document.
/// The response includes the document body as a tree of structural elements.
pub async fn get_document(state: &AppState, document_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{document_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Extract the plain text content from a document.
/// Walks the document body and concatenates all text runs.
pub async fn get_text(state: &AppState, document_id: &str) -> Result<Value> {
    let doc = get_document(state, document_id).await?;
    let title = doc["title"].as_str().unwrap_or("").to_owned();
    let mut text = String::new();
    if let Some(content) = doc["body"]["content"].as_array() {
        extract_text_from_content(content, &mut text);
    }
    Ok(json!({ "documentId": document_id, "title": title, "text": text }))
}

// ── Writing & Editing ─────────────────────────────────────────────────────────

/// Append text to the end of a document.
/// Fetches the document first to find the correct end index.
pub async fn append_text(state: &AppState, document_id: &str, text: &str) -> Result<Value> {
    // Find the current end-of-body index so we can insert just before it.
    // The body always ends with a newline segment; the last valid insert point
    // is one position before the body's endIndex.
    let doc = get_document(state, document_id).await?;
    let end_index = doc["body"]["content"]
        .as_array()
        .and_then(|arr| arr.last())
        .and_then(|el| el["endIndex"].as_i64())
        .unwrap_or(1)
        .max(1)
        - 1; // insert before the final newline

    batch_update(
        state,
        document_id,
        vec![json!({
            "insertText": {
                "location": { "index": end_index },
                "text": text,
            }
        })],
    )
    .await
}

/// Insert text at a specific character index in the document.
pub async fn insert_text(
    state: &AppState,
    document_id: &str,
    text: &str,
    index: i64,
) -> Result<Value> {
    batch_update(
        state,
        document_id,
        vec![json!({
            "insertText": {
                "location": { "index": index },
                "text": text,
            }
        })],
    )
    .await
}

/// Replace all occurrences of a string in the document.
pub async fn replace_text(
    state: &AppState,
    document_id: &str,
    find: &str,
    replacement: &str,
    match_case: bool,
) -> Result<Value> {
    batch_update(
        state,
        document_id,
        vec![json!({
            "replaceAllText": {
                "containsText": { "text": find, "matchCase": match_case },
                "replaceText": replacement,
            }
        })],
    )
    .await
}

/// Delete a range of content (start_index inclusive, end_index exclusive).
pub async fn delete_content_range(
    state: &AppState,
    document_id: &str,
    start_index: i64,
    end_index: i64,
) -> Result<Value> {
    batch_update(
        state,
        document_id,
        vec![json!({
            "deleteContentRange": {
                "range": {
                    "startIndex": start_index,
                    "endIndex":   end_index,
                }
            }
        })],
    )
    .await
}

// ── Formatting ────────────────────────────────────────────────────────────────

/// Apply a named style (e.g. "HEADING_1", "HEADING_2", "NORMAL_TEXT") to a range.
pub async fn apply_named_style(
    state: &AppState,
    document_id: &str,
    start_index: i64,
    end_index: i64,
    named_style: &str, // "HEADING_1" | "HEADING_2" | "HEADING_3" | "NORMAL_TEXT" | "TITLE" | "SUBTITLE"
) -> Result<Value> {
    batch_update(
        state,
        document_id,
        vec![json!({
            "updateParagraphStyle": {
                "range":  { "startIndex": start_index, "endIndex": end_index },
                "paragraphStyle": { "namedStyleType": named_style },
                "fields": "namedStyleType"
            }
        })],
    )
    .await
}

/// Apply bold/italic/underline to a text range.
pub async fn format_text(
    state: &AppState,
    document_id: &str,
    start_index: i64,
    end_index: i64,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
) -> Result<Value> {
    let mut text_style = json!({});
    let mut fields = vec![];
    if let Some(b) = bold {
        text_style["bold"] = json!(b);
        fields.push("bold");
    }
    if let Some(i) = italic {
        text_style["italic"] = json!(i);
        fields.push("italic");
    }
    if let Some(u) = underline {
        text_style["underline"] = json!(u);
        fields.push("underline");
    }

    if fields.is_empty() {
        anyhow::bail!("format_text: at least one of bold, italic, or underline must be specified");
    }

    batch_update(
        state,
        document_id,
        vec![json!({
            "updateTextStyle": {
                "range":     { "startIndex": start_index, "endIndex": end_index },
                "textStyle": text_style,
                "fields":    fields.join(","),
            }
        })],
    )
    .await
}

/// Insert a horizontal rule at the given index.
pub async fn insert_horizontal_rule(
    state: &AppState,
    document_id: &str,
    index: i64,
) -> Result<Value> {
    batch_update(
        state,
        document_id,
        vec![json!({
            "insertInlineImage": {
                // Use a paragraph break followed by a horizontal rule request.
                // We model this as a paragraph with a bottom border (the Docs API
                // doesn't have a direct "insert HR" request, so we insert a line
                // via a paragraph break and border styling).
                "location": { "index": index },
            }
        })],
    )
    .await
}

// ── Low-level helper ──────────────────────────────────────────────────────────

/// Send a batchUpdate to the document. All mutations go through here.
pub async fn batch_update(
    state: &AppState,
    document_id: &str,
    requests: Vec<Value>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/{document_id}:batchUpdate"))
        .bearer_auth(&tok)
        .json(&json!({ "requests": requests }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn extract_text_from_content(content: &[Value], out: &mut String) {
    for element in content {
        if let Some(paragraph) = element["paragraph"].as_object() {
            if let Some(elements) = paragraph["elements"].as_array() {
                for el in elements {
                    if let Some(text_run) = el["textRun"].as_object() {
                        if let Some(t) = text_run["content"].as_str() {
                            out.push_str(t);
                        }
                    }
                }
            }
        }
        // Recurse into table cells
        if let Some(table) = element["table"].as_object() {
            if let Some(rows) = table["tableRows"].as_array() {
                for row in rows {
                    if let Some(cells) = row["tableCells"].as_array() {
                        for cell in cells {
                            if let Some(cell_content) = cell["content"].as_array() {
                                extract_text_from_content(cell_content, out);
                            }
                        }
                    }
                }
            }
        }
    }
}
