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

/// Calendars for the node's calendar picker.
///
/// `?credential_id=` runs the lookup as the Google account that node is
/// configured to act as. Without it the picker would always list the globally
/// signed-in account's calendars, so a node pointed at a second account would
/// offer calendars that account cannot even see.
pub async fn get_google_calendars(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let credential_id = params
        .get("credential_id")
        .map(String::as_str)
        .unwrap_or_default();

    let listed = crate::google_accounts::scoped(&state, credential_id, async {
        state
            .tools
            .run("gcal_list_calendars", json!({}))
            .await
            .map_err(|e| e.to_string())
    })
    .await;

    // A failure here used to collapse into an empty list, which the picker
    // rendered as "you only have a primary calendar" — indistinguishable from
    // an expired token, and impossible for the user to act on. Report it.
    let res = match listed {
        Ok(res) => res,
        Err(e) => {
            tracing::warn!("Calendar list failed: {e}");
            return Json(json!({ "calendars": [], "error": e }));
        }
    };

    let mut calendars: Vec<Value> = res
        .get("items")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(calendar_option).collect())
        .unwrap_or_default();

    // Primary first, then calendars the user owns or can edit ("My calendars"
    // in Google's own sidebar), then everything subscribed to ("Other
    // calendars") — the order the user already knows from Google.
    calendars.sort_by(|a, b| {
        let rank = |c: &Value| c.get("rank").and_then(|v| v.as_i64()).unwrap_or(9);
        let name = |c: &Value| {
            c.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase()
        };
        rank(a).cmp(&rank(b)).then_with(|| name(a).cmp(&name(b)))
    });

    Json(json!({ "calendars": calendars }))
}

/// One `calendarList` entry reshaped for the picker, or `None` for entries the
/// user should never be offered.
fn calendar_option(cal: &Value) -> Option<Value> {
    let id = cal.get("id").and_then(|v| v.as_str())?.to_string();
    // Removed calendars linger in the list with `deleted: true`.
    if cal.get("deleted").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }

    // `summaryOverride` is the nickname the user gave a subscribed calendar; it
    // is what Google's own sidebar shows, so matching it is what makes a
    // calendar findable here by the name the user knows it by.
    let name = cal
        .get("summaryOverride")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| cal.get("summary").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .unwrap_or(&id)
        .to_string();

    let primary = cal
        .get("primary")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let access = cal
        .get("accessRole")
        .and_then(|v| v.as_str())
        .unwrap_or("reader");
    let owned = matches!(access, "owner" | "writer");

    let (rank, group) = match (primary, owned) {
        (true, _) => (0, "Primary"),
        (_, true) => (1, "My calendars"),
        _ => (2, "Other calendars"),
    };

    Some(json!({
        "name": name,
        "value": id,
        "primary": primary,
        "group": group,
        "rank": rank,
        "accessRole": access,
        "canEdit": owned,
        // True for calendars unticked in Google's sidebar. They are still
        // perfectly usable from a workflow, so they are offered — just labelled.
        "hidden": cal.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false)
            || cal.get("selected").and_then(|v| v.as_bool()) == Some(false),
        "backgroundColor": cal.get("backgroundColor").cloned().unwrap_or(Value::Null),
        "timeZone": cal.get("timeZone").cloned().unwrap_or(Value::Null),
    }))
}
