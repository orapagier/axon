use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use uuid::Uuid;

use crate::utils::{
    bool_arg, check_len, format_utc, i64_arg, page_args, parse_rfc3339_utc, require_non_empty_str,
    string_opt, string_patch, validate_choice, ACTIVITY_ENTITY_TYPES, ACTIVITY_KINDS, MAX_NAME_LEN,
    MAX_TEXT_LEN,
};

#[derive(sqlx::FromRow)]
struct ActivityRow {
    id: String,
    entity_id: String,
    entity_type: String,
    kind: String,
    title: String,
    body: Option<String>,
    occurred_at: String,
    due_at: Option<String>,
    done: bool,
    created_at: String,
}

impl ActivityRow {
    fn into_json(self) -> Value {
        serde_json::json!({
            "id": self.id,
            "entity_id": self.entity_id,
            "entity_type": self.entity_type,
            "kind": self.kind,
            "title": self.title,
            "body": self.body,
            "occurred_at": self.occurred_at,
            "due_at": self.due_at,
            "done": self.done,
            "created_at": self.created_at,
        })
    }
}

pub async fn log(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let entity_id = require_non_empty_str(args, "entity_id")?;
    let entity_type = require_non_empty_str(args, "entity_type")?;
    let title = require_non_empty_str(args, "title")?;
    let kind = string_opt(args, "kind")?.unwrap_or_else(|| "note".to_owned());
    let body = string_opt(args, "body")?;
    let occurred_at = string_opt(args, "occurred_at")?;
    let due_at = string_opt(args, "due_at")?;
    let done = bool_arg(args, "done")?.unwrap_or(false);

    validate_choice(entity_type, ACTIVITY_ENTITY_TYPES, "entity_type")?;
    validate_choice(&kind, ACTIVITY_KINDS, "kind")?;
    check_len("title", Some(title), MAX_NAME_LEN)?;
    check_len("body", body.as_deref(), MAX_TEXT_LEN)?;
    let occurred_at = parse_rfc3339_utc("occurred_at", occurred_at.as_deref())?;
    let due_at = parse_rfc3339_utc("due_at", due_at.as_deref())?;

    if !entity_exists(pool, entity_type, entity_id).await? {
        return Err(anyhow::anyhow!(
            "{} '{}' does not exist",
            entity_type,
            entity_id
        ));
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let occurred_at = occurred_at.unwrap_or_else(|| format_utc(Utc::now()));

    sqlx::query(
        "INSERT INTO activities
        (id, entity_id, entity_type, kind, title, body, occurred_at, due_at, done, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(entity_id)
    .bind(entity_type)
    .bind(&kind)
    .bind(title)
    .bind(body.as_deref())
    .bind(&occurred_at)
    .bind(due_at.as_deref())
    .bind(done)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(serde_json::json!({ "success": true, "id": id, "title": title }))
}

pub async fn list(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let entity_id = string_opt(args, "entity_id")?;
    let entity_type = string_opt(args, "entity_type")?;
    let kind = string_opt(args, "kind")?;
    let done = bool_arg(args, "done")?;
    let (limit, offset) = page_args(args);

    if let Some(entity_type) = entity_type.as_deref() {
        validate_choice(entity_type, ACTIVITY_ENTITY_TYPES, "entity_type")?;
    }
    if let Some(kind) = kind.as_deref() {
        validate_choice(kind, ACTIVITY_KINDS, "kind")?;
    }

    let total = build_activity_query(
        false,
        entity_id.as_deref(),
        entity_type.as_deref(),
        kind.as_deref(),
        done,
        None,
        None,
    )
    .build_query_scalar::<i64>()
    .fetch_one(pool)
    .await?;

    let rows = build_activity_query(
        true,
        entity_id.as_deref(),
        entity_type.as_deref(),
        kind.as_deref(),
        done,
        Some(limit),
        Some(offset),
    )
    .build_query_as::<ActivityRow>()
    .fetch_all(pool)
    .await?;

    Ok(serde_json::json!({
        "activities": rows.into_iter().map(ActivityRow::into_json).collect::<Vec<_>>(),
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Value> {
    sqlx::query_as::<_, ActivityRow>("SELECT * FROM activities WHERE id = ? AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(ActivityRow::into_json)
        .ok_or_else(|| anyhow::anyhow!("Activity '{id}' not found."))
}

pub async fn update(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let id = require_non_empty_str(args, "id")?;
    let existing = sqlx::query_as::<_, ActivityRow>(
        "SELECT * FROM activities WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Activity '{id}' not found."))?;

    let entity_type_patch = string_patch(args, "entity_type")?;
    let entity_id_patch = string_patch(args, "entity_id")?;
    if entity_type_patch.is_some() ^ entity_id_patch.is_some() {
        return Err(anyhow::anyhow!(
            "params 'entity_type' and 'entity_id' must be provided together when reassigning an activity"
        ));
    }

    let entity_type = match entity_type_patch {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'entity_type' cannot be empty")),
        None => existing.entity_type,
    };
    let entity_id = match entity_id_patch {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'entity_id' cannot be empty")),
        None => existing.entity_id,
    };
    let kind = match string_patch(args, "kind")? {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'kind' cannot be empty")),
        None => existing.kind,
    };
    let title = match string_patch(args, "title")? {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'title' cannot be empty")),
        None => existing.title,
    };
    let body = match string_patch(args, "body")? {
        Some(value) => value,
        None => existing.body,
    };
    let occurred_at = match string_patch(args, "occurred_at")? {
        Some(Some(value)) => Some(value),
        Some(None) => return Err(anyhow::anyhow!("param 'occurred_at' cannot be empty")),
        None => Some(existing.occurred_at),
    };
    // due_at is clearable: null or "" removes the due date.
    let due_at = match string_patch(args, "due_at")? {
        Some(value) => value,
        None => existing.due_at,
    };
    let done = bool_arg(args, "done")?.unwrap_or(existing.done);

    validate_choice(&entity_type, ACTIVITY_ENTITY_TYPES, "entity_type")?;
    validate_choice(&kind, ACTIVITY_KINDS, "kind")?;
    check_len("title", Some(&title), MAX_NAME_LEN)?;
    check_len("body", body.as_deref(), MAX_TEXT_LEN)?;
    let occurred_at = parse_rfc3339_utc("occurred_at", occurred_at.as_deref())?;
    let due_at = parse_rfc3339_utc("due_at", due_at.as_deref())?;

    if !entity_exists(pool, &entity_type, &entity_id).await? {
        return Err(anyhow::anyhow!(
            "{} '{}' does not exist",
            entity_type,
            entity_id
        ));
    }

    sqlx::query(
        "UPDATE activities
        SET entity_id = ?, entity_type = ?, kind = ?, title = ?, body = ?, occurred_at = ?,
            due_at = ?, done = ?
        WHERE id = ?",
    )
    .bind(&entity_id)
    .bind(&entity_type)
    .bind(&kind)
    .bind(&title)
    .bind(body.as_deref())
    .bind(occurred_at.as_deref())
    .bind(due_at.as_deref())
    .bind(done)
    .bind(id)
    .execute(pool)
    .await?;

    Ok(serde_json::json!({ "success": true, "id": id, "done": done }))
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<Value> {
    let result = sqlx::query("DELETE FROM activities WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(anyhow::anyhow!("Activity '{id}' not found."));
    }

    Ok(serde_json::json!({ "success": true, "deleted_id": id }))
}

fn build_activity_query<'a>(
    select_rows: bool,
    entity_id: Option<&'a str>,
    entity_type: Option<&'a str>,
    kind: Option<&'a str>,
    done: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> QueryBuilder<'a, Sqlite> {
    let mut qb = QueryBuilder::<Sqlite>::new(if select_rows {
        "SELECT id, entity_id, entity_type, kind, title, body, occurred_at, due_at, done, created_at FROM activities WHERE deleted_at IS NULL"
    } else {
        "SELECT COUNT(*) FROM activities WHERE deleted_at IS NULL"
    });

    if let Some(entity_id) = entity_id {
        qb.push(" AND entity_id = ").push_bind(entity_id);
    }

    if let Some(entity_type) = entity_type {
        qb.push(" AND entity_type = ").push_bind(entity_type);
    }

    if let Some(kind) = kind {
        qb.push(" AND kind = ").push_bind(kind);
    }

    if let Some(done) = done {
        qb.push(" AND done = ").push_bind(done);
    }

    if select_rows {
        qb.push(" ORDER BY occurred_at DESC, created_at DESC");
        if let Some(limit) = limit {
            qb.push(" LIMIT ").push_bind(limit);
        }
        if let Some(offset) = offset {
            qb.push(" OFFSET ").push_bind(offset);
        }
    }

    qb
}

/// The follow-up worklist: open (done = 0) task activities that are overdue or
/// due within the window, oldest due first, each labeled with the name/title of
/// the record it belongs to so the caller doesn't need a lookup per row.
pub async fn tasks_due(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let due_within_days = i64_arg(args, "due_within_days")?
        .unwrap_or(7)
        .clamp(0, 3650);
    let include_overdue = bool_arg(args, "include_overdue")?.unwrap_or(true);
    let include_undated = bool_arg(args, "include_undated")?.unwrap_or(false);
    let (limit, offset) = page_args(args);

    let now = Utc::now();
    let now_ts = format_utc(now);
    let cutoff = format_utc(now + chrono::Duration::days(due_within_days));

    // due_at uses the fixed-width UTC storage format, so plain lexicographic
    // comparisons are exact (same contract as the dashboard views).
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT a.id, a.entity_id, a.entity_type, a.kind, a.title, a.body,
                a.occurred_at, a.due_at, a.done, a.created_at,
                CASE a.entity_type
                    WHEN 'lead' THEN (SELECT name FROM leads WHERE id = a.entity_id)
                    WHEN 'deal' THEN (SELECT title FROM deals WHERE id = a.entity_id)
                    WHEN 'org'  THEN (SELECT name FROM orgs WHERE id = a.entity_id)
                END AS entity_label
        FROM activities a
        WHERE a.deleted_at IS NULL AND a.kind = 'task' AND a.done = 0 AND (",
    );
    qb.push("(a.due_at IS NOT NULL AND a.due_at <= ")
        .push_bind(cutoff.clone());
    if !include_overdue {
        qb.push(" AND a.due_at >= ").push_bind(now_ts.clone());
    }
    qb.push(")");
    if include_undated {
        qb.push(" OR a.due_at IS NULL");
    }
    qb.push(") ORDER BY a.due_at IS NULL, a.due_at ASC, a.created_at ASC");
    qb.push(" LIMIT ").push_bind(limit);
    qb.push(" OFFSET ").push_bind(offset);

    #[derive(sqlx::FromRow)]
    struct TaskRow {
        id: String,
        entity_id: String,
        entity_type: String,
        kind: String,
        title: String,
        body: Option<String>,
        occurred_at: String,
        due_at: Option<String>,
        done: bool,
        created_at: String,
        entity_label: Option<String>,
    }

    let rows = qb.build_query_as::<TaskRow>().fetch_all(pool).await?;

    let mut overdue_count = 0usize;
    let tasks: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let overdue = row
                .due_at
                .as_deref()
                .map(|due| due < now_ts.as_str())
                .unwrap_or(false);
            if overdue {
                overdue_count += 1;
            }
            serde_json::json!({
                "id": row.id,
                "entity_id": row.entity_id,
                "entity_type": row.entity_type,
                "entity_label": row.entity_label,
                "kind": row.kind,
                "title": row.title,
                "body": row.body,
                "occurred_at": row.occurred_at,
                "due_at": row.due_at,
                "done": row.done,
                "overdue": overdue,
                "created_at": row.created_at,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "generated_at": now.to_rfc3339(),
        "due_within_days": due_within_days,
        "count": tasks.len(),
        "overdue_count": overdue_count,
        "tasks": tasks,
        "limit": limit,
        "offset": offset,
    }))
}

async fn entity_exists(pool: &SqlitePool, entity_type: &str, entity_id: &str) -> Result<bool> {
    let sql = match entity_type {
        "lead" => "SELECT EXISTS(SELECT 1 FROM leads WHERE id = ? AND deleted_at IS NULL)",
        "deal" => "SELECT EXISTS(SELECT 1 FROM deals WHERE id = ? AND deleted_at IS NULL)",
        "org" => "SELECT EXISTS(SELECT 1 FROM orgs WHERE id = ? AND deleted_at IS NULL)",
        other => return Err(anyhow::anyhow!("unsupported entity_type '{other}'")),
    };

    let exists: i64 = sqlx::query_scalar(sql)
        .bind(entity_id)
        .fetch_one(pool)
        .await?;
    Ok(exists != 0)
}
