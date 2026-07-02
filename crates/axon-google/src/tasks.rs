use crate::auth::access_token;
use anyhow::Result;
use axon_core::{AppState, EnsureOk};
use serde_json::{json, Value};

const BASE: &str = "https://tasks.googleapis.com/tasks/v1";

// ── Task Lists ────────────────────────────────────────────────────────────────

/// List all task lists for the authenticated user.
pub async fn list_task_lists(state: &AppState, max_results: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/users/@me/lists"))
        .bearer_auth(&tok)
        .query(&[("maxResults", max_results.to_string())])
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Create a new task list.
pub async fn create_task_list(state: &AppState, title: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .post(format!("{BASE}/users/@me/lists"))
        .bearer_auth(&tok)
        .json(&json!({ "title": title }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Rename an existing task list.
pub async fn rename_task_list(state: &AppState, tasklist_id: &str, title: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .patch(format!("{BASE}/users/@me/lists/{tasklist_id}"))
        .bearer_auth(&tok)
        .json(&json!({ "title": title }))
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Delete a task list and all tasks inside it.
pub async fn delete_task_list(state: &AppState, tasklist_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/users/@me/lists/{tasklist_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
    Ok(json!({ "success": true, "deletedTaskListId": tasklist_id }))
}

// ── Tasks ─────────────────────────────────────────────────────────────────────

/// List tasks in a task list.
/// Pass `show_completed: true` to include already-completed tasks.
pub async fn list_tasks(
    state: &AppState,
    tasklist_id: &str,
    max_results: u32,
    show_completed: bool,
    due_min: Option<&str>,
    due_max: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut params = vec![
        ("maxResults", max_results.to_string()),
        ("showCompleted", show_completed.to_string()),
        ("showHidden", show_completed.to_string()),
    ];
    if let Some(d) = due_min {
        params.push(("dueMin", d.to_owned()));
    }
    if let Some(d) = due_max {
        params.push(("dueMax", d.to_owned()));
    }

    let resp: Value = state
        .client
        .get(format!("{BASE}/lists/{tasklist_id}/tasks"))
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

/// Get a single task by ID.
pub async fn get_task(state: &AppState, tasklist_id: &str, task_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/lists/{tasklist_id}/tasks/{task_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Create a new task. `due` should be an RFC 3339 timestamp if provided (e.g. "2025-06-01T00:00:00Z").
pub async fn create_task(
    state: &AppState,
    tasklist_id: &str,
    title: &str,
    notes: Option<&str>,
    due: Option<&str>,
    parent_task_id: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut body = json!({ "title": title, "status": "needsAction" });
    if let Some(n) = notes {
        body["notes"] = json!(n);
    }
    if let Some(d) = due {
        body["due"] = json!(d);
    }

    let mut req = state
        .client
        .post(format!("{BASE}/lists/{tasklist_id}/tasks"))
        .bearer_auth(&tok);

    // If a parent is specified, pass it as a query param to nest the task.
    if let Some(p) = parent_task_id {
        req = req.query(&[("parent", p)]);
    }

    let resp: Value = req
        .json(&body)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Update a task's title, notes, due date, or status.
pub async fn update_task(
    state: &AppState,
    tasklist_id: &str,
    task_id: &str,
    title: Option<&str>,
    notes: Option<&str>,
    due: Option<&str>,
    status: Option<&str>, // "needsAction" | "completed"
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut patch = json!({});
    if let Some(t) = title {
        patch["title"] = json!(t);
    }
    if let Some(n) = notes {
        patch["notes"] = json!(n);
    }
    if let Some(d) = due {
        patch["due"] = json!(d);
    }
    if let Some(s) = status {
        patch["status"] = json!(s);
    }

    let resp: Value = state
        .client
        .patch(format!("{BASE}/lists/{tasklist_id}/tasks/{task_id}"))
        .bearer_auth(&tok)
        .json(&patch)
        .send()
        .await?
        .ensure_ok()
        .await?
        .json()
        .await?;
    Ok(resp)
}

/// Mark a task as completed.
pub async fn complete_task(state: &AppState, tasklist_id: &str, task_id: &str) -> Result<Value> {
    update_task(
        state,
        tasklist_id,
        task_id,
        None,
        None,
        None,
        Some("completed"),
    )
    .await
}

/// Reopen a completed task (set back to needsAction).
pub async fn reopen_task(state: &AppState, tasklist_id: &str, task_id: &str) -> Result<Value> {
    update_task(
        state,
        tasklist_id,
        task_id,
        None,
        None,
        None,
        Some("needsAction"),
    )
    .await
}

/// Delete a task permanently.
pub async fn delete_task(state: &AppState, tasklist_id: &str, task_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/lists/{tasklist_id}/tasks/{task_id}"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
    Ok(json!({ "success": true, "deletedTaskId": task_id }))
}

/// Clear all completed tasks from a task list.
pub async fn clear_completed(state: &AppState, tasklist_id: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .post(format!("{BASE}/lists/{tasklist_id}/clear"))
        .bearer_auth(&tok)
        .send()
        .await?
        .ensure_ok()
        .await?;
    Ok(json!({ "success": true, "taskListId": tasklist_id }))
}

/// Move a task within a list (change its position or parent).
pub async fn move_task(
    state: &AppState,
    tasklist_id: &str,
    task_id: &str,
    parent_task_id: Option<&str>,
    previous_task_id: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;
    let mut params: Vec<(&str, String)> = vec![];
    if let Some(p) = parent_task_id {
        params.push(("parent", p.to_owned()));
    }
    if let Some(p) = previous_task_id {
        params.push(("previous", p.to_owned()));
    }

    let resp: Value = state
        .client
        .post(format!("{BASE}/lists/{tasklist_id}/tasks/{task_id}/move"))
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
