use super::*;

pub async fn get_jobs(State(state): State<AppState>) -> Json<Value> {
    let jobs = state.scheduler.get_all().await.unwrap_or_default();
    Json(json!({"jobs": jobs}))
}

pub async fn update_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let task = payload.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let sched = payload
        .get("schedule_nl")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if name.is_empty() || task.is_empty() || sched.is_empty() {
        return Json(json!({"ok": false, "error": "Name, task, and schedule are required"}));
    }

    match state.scheduler.update(&id, name, task, sched).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn run_job(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    match state.scheduler.run_once(&id).await {
        Ok(result) => Json(json!({"ok": true, "result": result})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn run_api(State(state): State<AppState>, Json(payload): Json<Value>) -> Json<Value> {
    let task = payload.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .or(Some("owner")); // Default to 'owner' if missing
    let platform = payload
        .get("platform")
        .and_then(|v| v.as_str())
        .unwrap_or("api");
    let chat_id = payload.get("chat_id").and_then(|v| v.as_str());

    let job_id = payload.get("job_id").and_then(|v| v.as_str());

    let user_time = payload.get("user_time").and_then(|v| v.as_str());
    let context =
        crate::agent::RunContext::new(task, platform, session_id, chat_id, job_id, user_time, None);
    match crate::agent::run_task(task, &state, context).await {
        Ok(result) => Json(json!({"ok": true, "result": result})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn create_job(State(state): State<AppState>, Json(j): Json<Value>) -> Json<Value> {
    let name = j.get("name").and_then(|v| v.as_str()).unwrap_or("job");
    let task = j.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let sched = j
        .get("schedule_nl")
        .and_then(|v| v.as_str())
        .unwrap_or("every day");
    match state
        .scheduler
        .create(
            name,
            task,
            sched,
            "dashboard",
            None,
            Some("dashboard"),
            None,
            None,
        )
        .await
    {
        Ok(job) => Json(json!({"ok":true, "id": job.id})),
        Err(e) => Json(json!({"ok":false, "error": e.to_string()})),
    }
}

pub async fn pause_job(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    match state.scheduler.pause(&id).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn resume_job(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    match state.scheduler.resume(&id).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn delete_job(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    match state.scheduler.delete(&id).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}
