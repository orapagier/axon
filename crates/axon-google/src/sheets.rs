use crate::auth::access_token;
use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use serde_json::{json, Value};

const BASE: &str = "https://sheets.googleapis.com/v4/spreadsheets";
const DRIVE_BASE: &str = "https://www.googleapis.com/drive/v3";

// ── Spreadsheet Listing (via Drive API) ───────────────────────────────────────

/// List spreadsheets in the user's Google Drive account.
/// Uses Drive files.list API filtered to Google Sheets mime type.
pub async fn list_spreadsheets(state: &AppState, max_results: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let q = "mimeType='application/vnd.google-apps.spreadsheet' and trashed=false";
    let resp: Value = state
        .client
        .get(format!("{DRIVE_BASE}/files"))
        .bearer_auth(&tok)
        .query(&[
            ("pageSize", max_results.to_string()),
            ("q", q.into()),
            ("orderBy", "modifiedTime desc".into()),
            (
                "fields",
                "files(id,name,modifiedTime,webViewLink,shared)".into(),
            ),
        ])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

// ── Spreadsheet Management ────────────────────────────────────────────────────

/// Create a new spreadsheet, optionally with a title and initial sheet names.
pub async fn create_spreadsheet(
    state: &AppState,
    title: &str,
    sheet_names: Option<Vec<String>>,
) -> Result<Value> {
    let tok = access_token(state).await?;

    let sheets = sheet_names
        .unwrap_or_else(|| vec!["Sheet1".to_string()])
        .iter()
        .map(|name| json!({ "properties": { "title": name } }))
        .collect::<Vec<_>>();

    let resp: Value = state
        .client
        .post(BASE)
        .bearer_auth(&tok)
        .json(&json!({
            "properties": { "title": title },
            "sheets": sheets,
        }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Get spreadsheet metadata (title, sheets, properties).
/// Does not return cell data — use `read_range` for that.
pub async fn get_spreadsheet(state: &AppState, spreadsheet_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{spreadsheet_id}"))
        .bearer_auth(&tok)
        .query(&[("includeGridData", "false")])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

// ── Reading & Writing Values ──────────────────────────────────────────────────

/// Read cell values from a range (e.g. "Sheet1!A1:D10" or "A1:Z100").
pub async fn read_range(state: &AppState, spreadsheet_id: &str, range: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let enc_range = urlenc(range);
    let resp: Value = state
        .client
        .get(format!("{BASE}/{spreadsheet_id}/values/{enc_range}"))
        .bearer_auth(&tok)
        .query(&[("valueRenderOption", "FORMATTED_VALUE")])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Read multiple ranges in a single request.
pub async fn batch_read(
    state: &AppState,
    spreadsheet_id: &str,
    ranges: Vec<String>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut params: Vec<(&str, String)> =
        ranges.iter().map(|r| ("ranges", r.to_string())).collect();
    params.push(("valueRenderOption", "FORMATTED_VALUE".to_owned()));

    let resp: Value = state
        .client
        .get(format!("{BASE}/{spreadsheet_id}/values:batchGet"))
        .bearer_auth(&tok)
        .query(&params)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Write values to a range. `values` is a 2-D array (rows × columns).
/// Example: `vec![vec!["Name", "Age"], vec!["Alice", "30"]]`
pub async fn write_range(
    state: &AppState,
    spreadsheet_id: &str,
    range: &str,
    values: Vec<Vec<serde_json::Value>>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let enc_range = urlenc(range);
    let resp: Value = state
        .client
        .put(format!("{BASE}/{spreadsheet_id}/values/{enc_range}"))
        .bearer_auth(&tok)
        .query(&[("valueInputOption", "USER_ENTERED")])
        .json(&json!({ "range": range, "values": values }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Write to multiple ranges in a single request.
/// Each entry is (range_string, 2D values array).
pub async fn batch_write(
    state: &AppState,
    spreadsheet_id: &str,
    data: Vec<(String, Vec<Vec<Value>>)>,
) -> Result<Value> {
    // Guard against the silent no-op: Google's values:batchUpdate happily returns
    // HTTP 200 for an empty `data` array, so an unparseable / blank `data` field
    // would otherwise report "success" while writing nothing. Fail loudly instead.
    if data.is_empty() {
        anyhow::bail!(
            "gsheets_batch_write: no valid ranges to write. Each item in 'data' needs a non-empty \
             'range' and 'values'. Check that the rows are filled in and that any range/values \
             expressions resolve to real values (a range that resolves to null/blank is skipped)."
        );
    }
    let tok = access_token(state).await?;
    let value_ranges: Vec<Value> = data
        .into_iter()
        .map(|(range, values)| json!({ "range": range, "values": values }))
        .collect();

    let resp: Value = state
        .client
        .post(format!("{BASE}/{spreadsheet_id}/values:batchUpdate"))
        .bearer_auth(&tok)
        .json(&json!({
            "valueInputOption": "USER_ENTERED",
            "data": value_ranges,
        }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Append rows after the last row that contains data in the given range.
pub async fn append_rows(
    state: &AppState,
    spreadsheet_id: &str,
    range: &str,
    values: Vec<Vec<serde_json::Value>>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let enc_range = urlenc(range);
    let resp: Value = state
        .client
        .post(format!("{BASE}/{spreadsheet_id}/values/{enc_range}:append"))
        .bearer_auth(&tok)
        .query(&[
            ("valueInputOption", "USER_ENTERED"),
            ("insertDataOption", "INSERT_ROWS"),
        ])
        .json(&json!({ "values": values }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Clear all values in a range (keeps formatting intact).
pub async fn clear_range(state: &AppState, spreadsheet_id: &str, range: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let enc_range = urlenc(range);
    let resp: Value = state
        .client
        .post(format!("{BASE}/{spreadsheet_id}/values/{enc_range}:clear"))
        .bearer_auth(&tok)
        .json(&json!({}))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Search for a value in a sheet range. Returns matching cell addresses and values.
pub async fn find_in_sheet(
    state: &AppState,
    spreadsheet_id: &str,
    range: &str,
    query: &str,
) -> Result<Value> {
    let data = read_range(state, spreadsheet_id, range).await?;
    let rows = data
        .get("values")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Parse the sheet name and start cell from range to compute addresses
    let (sheet_prefix, start_col, start_row) = parse_range_start(range);
    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();

    for (ri, row) in rows.iter().enumerate() {
        if let Some(cells) = row.as_array() {
            for (ci, cell) in cells.iter().enumerate() {
                let cell_str = match cell {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => continue,
                };
                if cell_str.to_lowercase().contains(&query_lower) {
                    let col_letter = col_index_to_letter(start_col + ci);
                    let row_num = start_row + ri + 1;
                    matches.push(json!({
                        "cell": format!("{}{}{}", sheet_prefix, col_letter, row_num),
                        "row": ri,
                        "column": ci,
                        "value": cell_str,
                    }));
                }
            }
        }
    }

    Ok(json!({
        "query": query,
        "range": range,
        "matches": matches,
        "total": matches.len(),
    }))
}

// ── Sheet (Tab) Management ────────────────────────────────────────────────────

/// Export a specific sheet tab to PDF, XLSX, CSV, etc.
pub async fn export_sheet(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    format: &str,
    range: Option<&str>,
    portrait: Option<bool>,
    fitw: Option<bool>,
    gridlines: Option<bool>,
) -> Result<Value> {
    let tok = access_token(state).await?;

    let (mime_type, ext) = match format.to_lowercase().as_str() {
        "pdf" => ("application/pdf", "pdf"),
        "xlsx" => (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "xlsx",
        ),
        "csv" => ("text/csv", "csv"),
        "tsv" => ("text/tab-separated-values", "tsv"),
        "ods" => ("application/x-vnd.oasis.opendocument.spreadsheet", "ods"),
        "zip" => ("application/zip", "zip"), // zip of html
        _ => ("application/pdf", "pdf"),
    };

    let mut query = format!("format={}&gid={}", ext, sheet_id);
    if let Some(r) = range {
        query.push_str(&format!("&range={}", urlenc(r)));
    }
    if let Some(p) = portrait {
        query.push_str(&format!("&portrait={}", p));
    }
    if let Some(f) = fitw {
        query.push_str(&format!("&fitw={}", f));
    }
    if let Some(g) = gridlines {
        query.push_str(&format!("&gridlines={}", g));
    }

    let url = format!(
        "https://docs.google.com/spreadsheets/d/{}/export?{}",
        spreadsheet_id, query
    );

    let bytes = state
        .client
        .get(&url)
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?
        .bytes()
        .await?;

    let meta = get_spreadsheet(state, spreadsheet_id)
        .await
        .unwrap_or(json!({}));
    let title = meta
        .pointer("/properties/title")
        .and_then(|v| v.as_str())
        .unwrap_or("exported_sheet");

    let export_name = format!("{}_{}.{}", title, sheet_id, ext);
    let download_dir = axon_core::data_files_dir();
    std::fs::create_dir_all(&download_dir)?;
    let path = download_dir.join(&export_name);
    std::fs::write(&path, &bytes)?;

    Ok(json!({
        "name": export_name,
        "file_path": path.to_string_lossy(),
        "mime_type": mime_type,
        "message": format!("Sheet exported as {} successfully.", ext.to_uppercase())
    }))
}

/// Add a new sheet tab to an existing spreadsheet.
pub async fn add_sheet(state: &AppState, spreadsheet_id: &str, title: &str) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "addSheet": {
                "properties": { "title": title }
            }
        })],
    )
    .await
}

/// Delete a sheet tab by its numeric sheet ID (not its title).
/// Use `get_spreadsheet` to find the sheetId for a given tab name.
pub async fn delete_sheet(state: &AppState, spreadsheet_id: &str, sheet_id: u64) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({ "deleteSheet": { "sheetId": sheet_id } })],
    )
    .await
}

/// Rename a sheet tab.
/// `sheet_id` is the numeric ID (find it via `get_spreadsheet`).
pub async fn rename_sheet(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    new_title: &str,
) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "updateSheetProperties": {
                "properties": { "sheetId": sheet_id, "title": new_title },
                "fields": "title"
            }
        })],
    )
    .await
}

/// Duplicate (copy) an existing sheet tab within the same spreadsheet.
pub async fn duplicate_sheet(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    new_title: Option<&str>,
) -> Result<Value> {
    let mut req = json!({
        "duplicateSheet": {
            "sourceSheetId": sheet_id,
        }
    });
    if let Some(t) = new_title {
        req["duplicateSheet"]["newSheetName"] = json!(t);
    }
    batch_update(state, spreadsheet_id, vec![req]).await
}

/// Copy a sheet tab to a different spreadsheet.
pub async fn copy_sheet_to(
    state: &AppState,
    source_spreadsheet_id: &str,
    sheet_id: u64,
    destination_spreadsheet_id: &str,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!(
            "{BASE}/{source_spreadsheet_id}/sheets/{sheet_id}:copyTo"
        ))
        .bearer_auth(&tok)
        .json(&json!({
            "destinationSpreadsheetId": destination_spreadsheet_id
        }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

// ── Row / Column Manipulation ─────────────────────────────────────────────────

/// Insert empty rows or columns.
/// `dimension` must be `"ROWS"` or `"COLUMNS"`.
/// Inserts `count` items starting at `start_index` (0-based).
pub async fn insert_dimension(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    dimension: &str,
    start_index: u32,
    count: u32,
) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "insertDimension": {
                "range": {
                    "sheetId":    sheet_id,
                    "dimension":  dimension,
                    "startIndex": start_index,
                    "endIndex":   start_index + count,
                },
                "inheritFromBefore": start_index > 0,
            }
        })],
    )
    .await
}

/// Delete rows or columns.
/// `dimension` must be `"ROWS"` or `"COLUMNS"`.
/// Deletes from `start_index` (inclusive) to `end_index` (exclusive), 0-based.
pub async fn delete_dimension(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    dimension: &str,
    start_index: u32,
    end_index: u32,
) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "deleteDimension": {
                "range": {
                    "sheetId":    sheet_id,
                    "dimension":  dimension,
                    "startIndex": start_index,
                    "endIndex":   end_index,
                }
            }
        })],
    )
    .await
}

// ── Sorting & Filtering ───────────────────────────────────────────────────────

/// Sort a range by a column.
/// `sort_column` is 0-based column index within the range.
/// `ascending` controls sort direction.
pub async fn sort_range(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    start_row: u32,
    end_row: u32,
    start_col: u32,
    end_col: u32,
    sort_column: u32,
    ascending: bool,
) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "sortRange": {
                "range": {
                    "sheetId":          sheet_id,
                    "startRowIndex":    start_row,
                    "endRowIndex":      end_row,
                    "startColumnIndex": start_col,
                    "endColumnIndex":   end_col,
                },
                "sortSpecs": [{
                    "dimensionIndex": sort_column,
                    "sortOrder": if ascending { "ASCENDING" } else { "DESCENDING" },
                }]
            }
        })],
    )
    .await
}

/// Add an auto-filter (basic filter) to a range.
pub async fn create_filter(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    start_row: u32,
    end_row: u32,
    start_col: u32,
    end_col: u32,
) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "setBasicFilter": {
                "filter": {
                    "range": {
                        "sheetId":          sheet_id,
                        "startRowIndex":    start_row,
                        "endRowIndex":      end_row,
                        "startColumnIndex": start_col,
                        "endColumnIndex":   end_col,
                    }
                }
            }
        })],
    )
    .await
}

/// Remove the basic filter from a sheet.
pub async fn clear_filter(state: &AppState, spreadsheet_id: &str, sheet_id: u64) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({ "clearBasicFilter": { "sheetId": sheet_id } })],
    )
    .await
}

// ── Merge / Unmerge ───────────────────────────────────────────────────────────

/// Merge cells in a range.
/// `merge_type`: "MERGE_ALL", "MERGE_COLUMNS", or "MERGE_ROWS".
pub async fn merge_cells(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    start_row: u32,
    end_row: u32,
    start_col: u32,
    end_col: u32,
    merge_type: &str,
) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "mergeCells": {
                "range": {
                    "sheetId":          sheet_id,
                    "startRowIndex":    start_row,
                    "endRowIndex":      end_row,
                    "startColumnIndex": start_col,
                    "endColumnIndex":   end_col,
                },
                "mergeType": merge_type,
            }
        })],
    )
    .await
}

/// Unmerge all merged cells in a range.
pub async fn unmerge_cells(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    start_row: u32,
    end_row: u32,
    start_col: u32,
    end_col: u32,
) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "unmergeCells": {
                "range": {
                    "sheetId":          sheet_id,
                    "startRowIndex":    start_row,
                    "endRowIndex":      end_row,
                    "startColumnIndex": start_col,
                    "endColumnIndex":   end_col,
                }
            }
        })],
    )
    .await
}

// ── Formatting ────────────────────────────────────────────────────────────────

/// Make a row bold. Useful for header rows.
/// `sheet_id` is the numeric sheet ID; `row_index` is 0-based.
pub async fn bold_row(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    row_index: u32,
) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "repeatCell": {
                "range": {
                    "sheetId":          sheet_id,
                    "startRowIndex":    row_index,
                    "endRowIndex":      row_index + 1,
                },
                "cell": {
                    "userEnteredFormat": {
                        "textFormat": { "bold": true }
                    }
                },
                "fields": "userEnteredFormat.textFormat.bold"
            }
        })],
    )
    .await
}

/// Freeze the first N rows of a sheet (header freeze).
pub async fn freeze_rows(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    row_count: u32,
) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "updateSheetProperties": {
                "properties": {
                    "sheetId": sheet_id,
                    "gridProperties": { "frozenRowCount": row_count }
                },
                "fields": "gridProperties.frozenRowCount"
            }
        })],
    )
    .await
}

/// Auto-resize all columns in a sheet to fit their content.
pub async fn auto_resize_columns(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
) -> Result<Value> {
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "autoResizeDimensions": {
                "dimensions": {
                    "sheetId":   sheet_id,
                    "dimension": "COLUMNS",
                }
            }
        })],
    )
    .await
}

/// Generic cell formatting: apply background color, text color, bold, italic,
/// font size, horizontal alignment to a range.
/// All format params are optional — only non-None values are applied.
pub async fn format_cells(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    start_row: u32,
    end_row: u32,
    start_col: u32,
    end_col: u32,
    bold: Option<bool>,
    italic: Option<bool>,
    font_size: Option<u32>,
    bg_color: Option<(f64, f64, f64)>, // (r, g, b) 0.0‑1.0
    fg_color: Option<(f64, f64, f64)>, // text colour
    h_align: Option<&str>,             // LEFT, CENTER, RIGHT
) -> Result<Value> {
    let mut format = serde_json::Map::new();
    let mut fields = Vec::new();

    // Text format
    let mut text_fmt = serde_json::Map::new();
    if let Some(b) = bold {
        text_fmt.insert("bold".into(), json!(b));
        fields.push("userEnteredFormat.textFormat.bold");
    }
    if let Some(i) = italic {
        text_fmt.insert("italic".into(), json!(i));
        fields.push("userEnteredFormat.textFormat.italic");
    }
    if let Some(sz) = font_size {
        text_fmt.insert("fontSize".into(), json!(sz));
        fields.push("userEnteredFormat.textFormat.fontSize");
    }
    if let Some((r, g, b)) = fg_color {
        text_fmt.insert(
            "foregroundColorStyle".into(),
            json!({ "rgbColor": { "red": r, "green": g, "blue": b } }),
        );
        fields.push("userEnteredFormat.textFormat.foregroundColorStyle");
    }
    if !text_fmt.is_empty() {
        format.insert("textFormat".into(), Value::Object(text_fmt));
    }

    // Background
    if let Some((r, g, b)) = bg_color {
        format.insert(
            "backgroundColorStyle".into(),
            json!({ "rgbColor": { "red": r, "green": g, "blue": b } }),
        );
        fields.push("userEnteredFormat.backgroundColorStyle");
    }

    // Alignment
    if let Some(align) = h_align {
        format.insert("horizontalAlignment".into(), json!(align));
        fields.push("userEnteredFormat.horizontalAlignment");
    }

    if fields.is_empty() {
        anyhow::bail!("No formatting options specified");
    }

    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "repeatCell": {
                "range": {
                    "sheetId":          sheet_id,
                    "startRowIndex":    start_row,
                    "endRowIndex":      end_row,
                    "startColumnIndex": start_col,
                    "endColumnIndex":   end_col,
                },
                "cell": {
                    "userEnteredFormat": format,
                },
                "fields": fields.join(","),
            }
        })],
    )
    .await
}

// ── Conditional Formatting ────────────────────────────────────────────────────

/// Add a conditional formatting rule based on a custom formula.
/// Example formula: `=A1>100` (applied relative to the range).
/// Color is (r, g, b) with values 0.0–1.0.
pub async fn add_conditional_format(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
    start_row: u32,
    end_row: u32,
    start_col: u32,
    end_col: u32,
    formula: &str,
    bg_color: (f64, f64, f64),
) -> Result<Value> {
    let (r, g, b) = bg_color;
    batch_update(
        state,
        spreadsheet_id,
        vec![json!({
            "addConditionalFormatRule": {
                "rule": {
                    "ranges": [{
                        "sheetId":          sheet_id,
                        "startRowIndex":    start_row,
                        "endRowIndex":      end_row,
                        "startColumnIndex": start_col,
                        "endColumnIndex":   end_col,
                    }],
                    "booleanRule": {
                        "condition": {
                            "type": "CUSTOM_FORMULA",
                            "values": [{ "userEnteredValue": formula }],
                        },
                        "format": {
                            "backgroundColor": { "red": r, "green": g, "blue": b },
                        },
                    },
                },
                "index": 0,
            }
        })],
    )
    .await
}

/// Clear all conditional formatting rules from a sheet.
pub async fn clear_conditional_formats(
    state: &AppState,
    spreadsheet_id: &str,
    sheet_id: u64,
) -> Result<Value> {
    // First get the spreadsheet to find how many rules exist
    let meta = get_spreadsheet(state, spreadsheet_id).await?;
    let mut requests = Vec::new();

    if let Some(sheets_arr) = meta.get("sheets").and_then(|v| v.as_array()) {
        for sheet in sheets_arr {
            let sid = sheet
                .pointer("/properties/sheetId")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if sid != sheet_id {
                continue;
            }
            if let Some(rules) = sheet.get("conditionalFormats").and_then(|v| v.as_array()) {
                // Remove rules in reverse order to keep indices stable
                for i in (0..rules.len()).rev() {
                    requests.push(json!({
                        "deleteConditionalFormatRule": {
                            "sheetId": sheet_id,
                            "index": i,
                        }
                    }));
                }
            }
        }
    }

    if requests.is_empty() {
        return Ok(json!({ "message": "No conditional format rules to clear" }));
    }
    batch_update(state, spreadsheet_id, requests).await
}

// ── Low-level helper ──────────────────────────────────────────────────────────

/// Send a batchUpdate request. All sheet structural changes go through this.
pub async fn batch_update(
    state: &AppState,
    spreadsheet_id: &str,
    requests: Vec<Value>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/{spreadsheet_id}:batchUpdate"))
        .bearer_auth(&tok)
        .json(&json!({ "requests": requests }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Parse the start of an A1-notation range to extract sheet prefix, column index,
/// and row index. E.g. "Sheet1!B3:D10" → ("Sheet1!", 1, 2).
fn parse_range_start(range: &str) -> (String, usize, usize) {
    let (sheet_prefix, cell_part) = if let Some(idx) = range.find('!') {
        (format!("{}!", &range[..idx]), &range[idx + 1..])
    } else {
        (String::new(), range)
    };

    // Split cell_part (e.g. "B3:D10") at ':' and take the start cell
    let start_cell = cell_part.split(':').next().unwrap_or("A1");

    let mut col_str = String::new();
    let mut row_str = String::new();
    for ch in start_cell.chars() {
        if ch.is_ascii_alphabetic() {
            col_str.push(ch);
        } else if ch.is_ascii_digit() {
            row_str.push(ch);
        }
    }

    let col_index = col_letter_to_index(&col_str);
    let row_index = row_str.parse::<usize>().unwrap_or(1).saturating_sub(1);

    (sheet_prefix, col_index, row_index)
}

fn col_letter_to_index(s: &str) -> usize {
    let mut idx = 0usize;
    for c in s.to_uppercase().chars() {
        idx = idx * 26 + (c as usize - 'A' as usize + 1);
    }
    idx.saturating_sub(1)
}

fn col_index_to_letter(mut idx: usize) -> String {
    let mut result = String::new();
    loop {
        result.insert(0, (b'A' + (idx % 26) as u8) as char);
        if idx < 26 {
            break;
        }
        idx = idx / 26 - 1;
    }
    result
}
