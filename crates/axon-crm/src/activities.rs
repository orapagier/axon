use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use uuid::Uuid;

use crate::utils::{
    check_len, format_utc, page_args, parse_rfc3339_utc, require_non_empty_str, string_opt,
    string_patch, validate_choice, ACTIVITY_ENTITY_TYPES, ACTIVITY_KINDS, MAX_NAME_LEN,
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

    validate_choice(entity_type, ACTIVITY_ENTITY_TYPES, "entity_type")?;
    validate_choice(&kind, ACTIVITY_KINDS, "kind")?;
    check_len("title", Some(title), MAX_NAME_LEN)?;
    check_len("body", body.as_deref(), MAX_TEXT_LEN)?;
    let occurred_at = parse_rfc3339_utc("occurred_at", occurred_at.as_deref())?;

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
        (id, entity_id, entity_type, kind, title, body, occurred_at, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(entity_id)
    .bind(entity_type)
    .bind(&kind)
    .bind(title)
    .bind(body.as_deref())
    .bind(&occurred_at)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(serde_json::json!({ "success": true, "id": id, "title": title }))
}

pub async fn list(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let entity_id = string_opt(args, "entity_id")?;
    let entity_type = string_opt(args, "entity_type")?;
    let kind = string_opt(args, "kind")?;
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

    validate_choice(&entity_type, ACTIVITY_ENTITY_TYPES, "entity_type")?;
    validate_choice(&kind, ACTIVITY_KINDS, "kind")?;
    check_len("title", Some(&title), MAX_NAME_LEN)?;
    check_len("body", body.as_deref(), MAX_TEXT_LEN)?;
    let occurred_at = parse_rfc3339_utc("occurred_at", occurred_at.as_deref())?;

    if !entity_exists(pool, &entity_type, &entity_id).await? {
        return Err(anyhow::anyhow!(
            "{} '{}' does not exist",
            entity_type,
            entity_id
        ));
    }

    sqlx::query(
        "UPDATE activities
        SET entity_id = ?, entity_type = ?, kind = ?, title = ?, body = ?, occurred_at = ?
        WHERE id = ?",
    )
    .bind(&entity_id)
    .bind(&entity_type)
    .bind(&kind)
    .bind(&title)
    .bind(body.as_deref())
    .bind(occurred_at.as_deref())
    .bind(id)
    .execute(pool)
    .await?;

    Ok(serde_json::json!({ "success": true, "id": id }))
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
    limit: Option<i64>,
    offset: Option<i64>,
) -> QueryBuilder<'a, Sqlite> {
    let mut qb = QueryBuilder::<Sqlite>::new(if select_rows {
        "SELECT id, entity_id, entity_type, kind, title, body, occurred_at, created_at FROM activities WHERE deleted_at IS NULL"
    } else {
        "SELECT COUNT(*) FROM activities WHERE deleted_at IS NULL"
    });

    let mut has_filter = false;
    if entity_id.is_some() || entity_type.is_some() || kind.is_some() {
        qb.push(" AND ");
    }

    if let Some(entity_id) = entity_id {
        if has_filter {
            qb.push(" AND ");
        }
        has_filter = true;
        qb.push("entity_id = ").push_bind(entity_id);
    }

    if let Some(entity_type) = entity_type {
        if has_filter {
            qb.push(" AND ");
        }
        has_filter = true;
        qb.push("entity_type = ").push_bind(entity_type);
    }

    if let Some(kind) = kind {
        if has_filter {
            qb.push(" AND ");
        }
        qb.push("kind = ").push_bind(kind);
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
