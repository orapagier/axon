use super::*;

pub async fn get_google_sheets(State(state): State<AppState>) -> Json<Value> {
    if let Ok(res) = state
        .tools
        .run("gsheets_list", json!({"max_results": 100}))
        .await
    {
        Json(res)
    } else {
        Json(json!({"files": []}))
    }
}

pub async fn get_google_sheet_tabs(
    State(state): State<AppState>,
    Path(spreadsheet_id): Path<String>,
) -> Json<Value> {
    let res = match state
        .tools
        .run("gsheets_get", json!({"spreadsheet_id": spreadsheet_id}))
        .await
    {
        Ok(value) => value,
        Err(e) => {
            return Json(json!({
                "tabs": [],
                "sheet_id_map": {},
                "error": e.to_string(),
            }))
        }
    };

    let tabs: Vec<Value> = res
        .get("sheets")
        .and_then(|v| v.as_array())
        .map(|sheets| {
            sheets
                .iter()
                .filter_map(|sheet| {
                    let props = sheet.get("properties")?;
                    let title = props
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let sheet_id = props.get("sheetId").and_then(|v| v.as_i64())?;
                    Some(json!({
                        "title": title,
                        "sheet_id": sheet_id,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut sheet_id_map = serde_json::Map::new();
    for tab in &tabs {
        if let (Some(title), Some(sheet_id)) = (
            tab.get("title").and_then(|v| v.as_str()),
            tab.get("sheet_id"),
        ) {
            sheet_id_map.insert(title.to_string(), sheet_id.clone());
        }
    }

    Json(json!({
        "tabs": tabs,
        "sheet_id_map": sheet_id_map,
    }))
}

pub async fn get_google_calendars(State(state): State<AppState>) -> Json<Value> {
    if let Ok(res) = state.tools.run("gcal_list_calendars", json!({})).await {
        // gcal_list_calendars returns Google's raw calendarList response:
        // { kind: "calendar#calendarList", items: [ { id, summary, primary, ... } ] }
        let calendars: Vec<Value> = res
            .get("items")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|cal| {
                        let id = cal.get("id").and_then(|v| v.as_str())?.to_string();
                        let name = cal
                            .get("summary")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&id)
                            .to_string();
                        let primary = cal
                            .get("primary")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        Some(json!({ "name": name, "value": id, "primary": primary }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Json(json!({ "calendars": calendars }))
    } else {
        Json(json!({ "calendars": [] }))
    }
}
