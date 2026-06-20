use crate::auth::access_token;
use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use serde_json::{json, Value};
use uuid::Uuid;

const BASE: &str = "https://slides.googleapis.com/v1/presentations";

// ── Presentation Management ───────────────────────────────────────────────────

/// Create a new blank presentation.
pub async fn create_presentation(state: &AppState, title: &str) -> Result<Value> {
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

/// Get the full structure of a presentation (all slides and their elements).
pub async fn get_presentation(state: &AppState, presentation_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{presentation_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

/// Get a single slide by its object ID.
pub async fn get_slide(state: &AppState, presentation_id: &str, slide_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{presentation_id}/pages/{slide_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}

// ── Slide Operations ──────────────────────────────────────────────────────────

/// Add a blank slide at the given index (0-based). Returns the updated presentation.
pub async fn add_slide(
    state: &AppState,
    presentation_id: &str,
    insertion_index: u32,
    layout: Option<&str>, // e.g. "TITLE_AND_BODY", "BLANK", "TITLE_ONLY"
) -> Result<Value> {
    let slide_id = Uuid::new_v4().to_string().replace('-', "");
    let mut req = json!({
        "createSlide": {
            "objectId":       slide_id,
            "insertionIndex": insertion_index,
        }
    });
    if let Some(l) = layout {
        req["createSlide"]["slideLayoutReference"] = json!({ "predefinedLayout": l });
    }
    batch_update(state, presentation_id, vec![req]).await
}

/// Duplicate an existing slide (identified by its object ID) and insert the copy after it.
pub async fn duplicate_slide(
    state: &AppState,
    presentation_id: &str,
    slide_object_id: &str,
) -> Result<Value> {
    batch_update(
        state,
        presentation_id,
        vec![json!({
            "duplicateObject": { "objectId": slide_object_id }
        })],
    )
    .await
}

/// Delete a slide by its object ID.
pub async fn delete_slide(
    state: &AppState,
    presentation_id: &str,
    slide_object_id: &str,
) -> Result<Value> {
    batch_update(
        state,
        presentation_id,
        vec![json!({
            "deleteObject": { "objectId": slide_object_id }
        })],
    )
    .await
}

/// Move a slide to a new position (0-based index).
pub async fn move_slide(
    state: &AppState,
    presentation_id: &str,
    slide_object_id: &str,
    new_index: u32,
) -> Result<Value> {
    batch_update(
        state,
        presentation_id,
        vec![json!({
            "updateSlidesPosition": {
                "slideObjectIds": [slide_object_id],
                "insertionIndex": new_index,
            }
        })],
    )
    .await
}

// ── Text & Content ────────────────────────────────────────────────────────────

/// Replace all occurrences of a string across the entire presentation.
/// Useful for template-style presentations (e.g. "{{name}}" → "Alice").
pub async fn replace_text(
    state: &AppState,
    presentation_id: &str,
    find: &str,
    replacement: &str,
    match_case: bool,
) -> Result<Value> {
    batch_update(
        state,
        presentation_id,
        vec![json!({
            "replaceAllText": {
                "containsText": { "text": find, "matchCase": match_case },
                "replaceText":  replacement,
            }
        })],
    )
    .await
}

/// Insert a text box on a slide at a given position and size (in EMUs — 914400 EMU = 1 inch).
pub async fn insert_text_box(
    state: &AppState,
    presentation_id: &str,
    slide_object_id: &str,
    text: &str,
    x: i64,      // left position in EMUs
    y: i64,      // top position in EMUs
    width: i64,  // width in EMUs
    height: i64, // height in EMUs
) -> Result<Value> {
    let box_id = Uuid::new_v4().to_string().replace('-', "");
    batch_update(
        state,
        presentation_id,
        vec![
            json!({
                "createShape": {
                    "objectId":  box_id,
                    "shapeType": "TEXT_BOX",
                    "elementProperties": {
                        "pageObjectId": slide_object_id,
                        "size": {
                            "width":  { "magnitude": width,  "unit": "EMU" },
                            "height": { "magnitude": height, "unit": "EMU" },
                        },
                        "transform": {
                            "scaleX": 1.0, "scaleY": 1.0,
                            "translateX": x, "translateY": y,
                            "unit": "EMU",
                        },
                    }
                }
            }),
            json!({
                "insertText": {
                    "objectId": box_id,
                    "text":     text,
                }
            }),
        ],
    )
    .await
}

/// Insert an image from a public URL onto a slide.
pub async fn insert_image(
    state: &AppState,
    presentation_id: &str,
    slide_object_id: &str,
    image_url: &str,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
) -> Result<Value> {
    let image_id = Uuid::new_v4().to_string().replace('-', "");
    batch_update(
        state,
        presentation_id,
        vec![json!({
            "createImage": {
                "objectId": image_id,
                "url": image_url,
                "elementProperties": {
                    "pageObjectId": slide_object_id,
                    "size": {
                        "width":  { "magnitude": width,  "unit": "EMU" },
                        "height": { "magnitude": height, "unit": "EMU" },
                    },
                    "transform": {
                        "scaleX": 1.0, "scaleY": 1.0,
                        "translateX": x, "translateY": y,
                        "unit": "EMU",
                    },
                }
            }
        })],
    )
    .await
}

// ── Styling ───────────────────────────────────────────────────────────────────

/// Change the background color of a slide (RGB values 0.0–1.0).
pub async fn set_slide_background(
    state: &AppState,
    presentation_id: &str,
    slide_object_id: &str,
    red: f64,
    green: f64,
    blue: f64,
) -> Result<Value> {
    batch_update(
        state,
        presentation_id,
        vec![json!({
            "updatePageProperties": {
                "objectId": slide_object_id,
                "pageProperties": {
                    "pageBackgroundFill": {
                        "solidFill": {
                            "color": {
                                "rgbColor": { "red": red, "green": green, "blue": blue }
                            }
                        }
                    }
                },
                "fields": "pageBackgroundFill"
            }
        })],
    )
    .await
}

/// Update the font size and bold/italic of text within a shape.
pub async fn format_shape_text(
    state: &AppState,
    presentation_id: &str,
    shape_object_id: &str,
    font_size_pt: Option<f64>,
    bold: Option<bool>,
    italic: Option<bool>,
) -> Result<Value> {
    let mut style = json!({});
    let mut fields = vec![];
    if let Some(fs) = font_size_pt {
        style["fontSize"] = json!({ "magnitude": fs, "unit": "PT" });
        fields.push("fontSize");
    }
    if let Some(b) = bold {
        style["bold"] = json!(b);
        fields.push("bold");
    }
    if let Some(i) = italic {
        style["italic"] = json!(i);
        fields.push("italic");
    }

    if fields.is_empty() {
        anyhow::bail!("format_shape_text: specify at least one of font_size_pt, bold, italic");
    }

    batch_update(
        state,
        presentation_id,
        vec![json!({
            "updateTextStyle": {
                "objectId": shape_object_id,
                "style":    style,
                "fields":   fields.join(","),
            }
        })],
    )
    .await
}

// ── Low-level helper ──────────────────────────────────────────────────────────

/// Send a batchUpdate to the presentation. All mutations go through here.
pub async fn batch_update(
    state: &AppState,
    presentation_id: &str,
    requests: Vec<Value>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/{presentation_id}:batchUpdate"))
        .bearer_auth(&tok)
        .json(&json!({ "requests": requests }))
        .send()
        .await?
        .ensure_ok().await?
        .json()
        .await?;
    Ok(resp)
}
