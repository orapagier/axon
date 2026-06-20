use crate::auth::access_token;
use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use serde_json::{json, Value};

const BASE: &str = "https://forms.googleapis.com/v1/forms";

// ── Form Management ───────────────────────────────────────────────────────────

/// Create a new Google Form with a title (and optional document title).
/// Returns the created form including its `formId` and `responderUri`.
pub async fn create_form(
    state: &AppState,
    title: &str,
    document_title: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(BASE)
        .bearer_auth(&tok)
        .json(&json!({
            "info": {
                "title":         title,
                "documentTitle": document_title.unwrap_or(title),
            }
        }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Get a form's full structure including all questions.
pub async fn get_form(state: &AppState, form_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{form_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Update the form's title or description.
pub async fn update_form_info(
    state: &AppState,
    form_id: &str,
    title: Option<&str>,
    description: Option<&str>,
) -> Result<Value> {
    let mut info = json!({});
    let mut fields = vec![];
    if let Some(t) = title {
        info["title"] = json!(t);
        fields.push("title");
    }
    if let Some(d) = description {
        info["description"] = json!(d);
        fields.push("description");
    }

    if fields.is_empty() {
        anyhow::bail!("update_form_info: provide at least one of title or description");
    }

    batch_update(
        state,
        form_id,
        vec![json!({
            "updateFormInfo": {
                "info":       info,
                "updateMask": fields.join(","),
            }
        })],
    )
    .await
}

// ── Questions ─────────────────────────────────────────────────────────────────

/// Add a short-answer (text) question to the form.
pub async fn add_short_answer_question(
    state: &AppState,
    form_id: &str,
    question_text: &str,
    required: bool,
    index: u32,
) -> Result<Value> {
    batch_update(
        state,
        form_id,
        vec![create_item_request(
            question_text,
            required,
            index,
            json!({ "textQuestion": { "paragraph": false } }),
        )],
    )
    .await
}

/// Add a paragraph (long-answer) question.
pub async fn add_paragraph_question(
    state: &AppState,
    form_id: &str,
    question_text: &str,
    required: bool,
    index: u32,
) -> Result<Value> {
    batch_update(
        state,
        form_id,
        vec![create_item_request(
            question_text,
            required,
            index,
            json!({ "textQuestion": { "paragraph": true } }),
        )],
    )
    .await
}

/// Add a multiple-choice question (single answer).
pub async fn add_multiple_choice_question(
    state: &AppState,
    form_id: &str,
    question_text: &str,
    options: Vec<&str>,
    required: bool,
    index: u32,
) -> Result<Value> {
    let choices = options
        .iter()
        .map(|o| json!({ "value": o }))
        .collect::<Vec<_>>();
    batch_update(
        state,
        form_id,
        vec![create_item_request(
            question_text,
            required,
            index,
            json!({
                "choiceQuestion": {
                    "type":    "RADIO",
                    "options": choices,
                    "shuffle": false,
                }
            }),
        )],
    )
    .await
}

/// Add a checkbox question (multiple answers allowed).
pub async fn add_checkbox_question(
    state: &AppState,
    form_id: &str,
    question_text: &str,
    options: Vec<&str>,
    required: bool,
    index: u32,
) -> Result<Value> {
    let choices = options
        .iter()
        .map(|o| json!({ "value": o }))
        .collect::<Vec<_>>();
    batch_update(
        state,
        form_id,
        vec![create_item_request(
            question_text,
            required,
            index,
            json!({
                "choiceQuestion": {
                    "type":    "CHECKBOX",
                    "options": choices,
                    "shuffle": false,
                }
            }),
        )],
    )
    .await
}

/// Add a dropdown question.
pub async fn add_dropdown_question(
    state: &AppState,
    form_id: &str,
    question_text: &str,
    options: Vec<&str>,
    required: bool,
    index: u32,
) -> Result<Value> {
    let choices = options
        .iter()
        .map(|o| json!({ "value": o }))
        .collect::<Vec<_>>();
    batch_update(
        state,
        form_id,
        vec![create_item_request(
            question_text,
            required,
            index,
            json!({
                "choiceQuestion": {
                    "type":    "DROP_DOWN",
                    "options": choices,
                }
            }),
        )],
    )
    .await
}

/// Add a linear scale question (e.g. 1–5 rating).
pub async fn add_scale_question(
    state: &AppState,
    form_id: &str,
    question_text: &str,
    low: u32,
    high: u32,
    low_label: Option<&str>,
    high_label: Option<&str>,
    required: bool,
    index: u32,
) -> Result<Value> {
    let mut scale = json!({ "low": low, "high": high });
    if let Some(l) = low_label {
        scale["lowLabel"] = json!(l);
    }
    if let Some(h) = high_label {
        scale["highLabel"] = json!(h);
    }
    batch_update(
        state,
        form_id,
        vec![create_item_request(
            question_text,
            required,
            index,
            json!({ "scaleQuestion": scale }),
        )],
    )
    .await
}

/// Add a date question.
pub async fn add_date_question(
    state: &AppState,
    form_id: &str,
    question_text: &str,
    include_time: bool,
    required: bool,
    index: u32,
) -> Result<Value> {
    batch_update(
        state,
        form_id,
        vec![create_item_request(
            question_text,
            required,
            index,
            json!({ "dateQuestion": { "includeTime": include_time, "includeYear": true } }),
        )],
    )
    .await
}

/// Add a section header (non-question divider) to the form.
pub async fn add_section_header(
    state: &AppState,
    form_id: &str,
    title: &str,
    description: Option<&str>,
    index: u32,
) -> Result<Value> {
    let mut item = json!({
        "title": title,
        "pageBreakItem": {}
    });
    if let Some(d) = description {
        item["description"] = json!(d);
    }
    batch_update(
        state,
        form_id,
        vec![json!({
            "createItem": {
                "item":     item,
                "location": { "index": index },
            }
        })],
    )
    .await
}

/// Delete a question/item by its item ID.
pub async fn delete_item(state: &AppState, form_id: &str, item_id: &str) -> Result<Value> {
    batch_update(
        state,
        form_id,
        vec![json!({
            "deleteItem": { "location": { "itemId": item_id } }
        })],
    )
    .await
}

// ── Responses ─────────────────────────────────────────────────────────────────

/// List all responses for a form.
pub async fn list_responses(state: &AppState, form_id: &str, page_size: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{form_id}/responses"))
        .bearer_auth(&tok)
        .query(&[("pageSize", page_size.to_string())])
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Get a single form response by its response ID.
pub async fn get_response(state: &AppState, form_id: &str, response_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{form_id}/responses/{response_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

// ── Low-level helper ──────────────────────────────────────────────────────────

/// Send a batchUpdate request. All form mutations use this.
pub async fn batch_update(state: &AppState, form_id: &str, requests: Vec<Value>) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/{form_id}:batchUpdate"))
        .bearer_auth(&tok)
        .json(&json!({ "requests": requests }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Build a createItem request for a question.
/// `question_type` should be a JSON object with exactly one key naming the question variant,
/// e.g. `json!({ "textQuestion": { "paragraph": false } })`.
fn create_item_request(
    question_text: &str,
    required: bool,
    index: u32,
    question_type: Value,
) -> Value {
    // Merge `required` into the question_type object so the final "question" field
    // contains both the required flag and the specific question variant in one object.
    let mut question = question_type;
    question["required"] = json!(required);

    json!({
        "createItem": {
            "item": {
                "title": question_text,
                "questionItem": { "question": question }
            },
            "location": { "index": index },
        }
    })
}
