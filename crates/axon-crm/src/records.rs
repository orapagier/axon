use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value};
use sqlx::{Row, SqlitePool};

use crate::utils::{bool_arg, inject_tags, page_args, require_non_empty_str};

pub async fn archive(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let entity_type = require_non_empty_str(args, "entity_type")?;
    let id = require_non_empty_str(args, "id")?;
    let archived_at = Utc::now().to_rfc3339();

    match entity_type {
        "org" => {
            let lead_count = active_count(
                pool,
                "SELECT COUNT(*) FROM leads WHERE org_id = ? AND deleted_at IS NULL",
                id,
            )
            .await?;
            let deal_count = active_count(
                pool,
                "SELECT COUNT(*) FROM deals WHERE org_id = ? AND deleted_at IS NULL",
                id,
            )
            .await?;
            if lead_count > 0 || deal_count > 0 {
                return Err(anyhow::anyhow!(
                    "Cannot archive organization '{id}': archive or reassign linked leads/deals first."
                ));
            }
            archive_row(pool, "orgs", id, &archived_at).await?;
        }
        "lead" => {
            let deal_count = active_count(
                pool,
                "SELECT COUNT(*) FROM deals WHERE contact_id = ? AND deleted_at IS NULL",
                id,
            )
            .await?;
            if deal_count > 0 {
                return Err(anyhow::anyhow!(
                    "Cannot archive lead '{id}': archive or remove linked deals first."
                ));
            }
            archive_row(pool, "leads", id, &archived_at).await?;
        }
        "deal" => archive_row(pool, "deals", id, &archived_at).await?,
        "activity" => archive_row(pool, "activities", id, &archived_at).await?,
        other => {
            return Err(anyhow::anyhow!(
                "param 'entity_type' must be one of: org, lead, deal, activity (got '{other}')"
            ))
        }
    }

    Ok(serde_json::json!({
        "success": true,
        "entity_type": entity_type,
        "id": id,
        "archived_at": archived_at,
    }))
}

pub async fn restore(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let entity_type = require_non_empty_str(args, "entity_type")?;
    let id = require_non_empty_str(args, "id")?;

    match entity_type {
        "org" => restore_row(pool, "orgs", id).await?,
        "lead" => {
            let row =
                sqlx::query("SELECT org_id FROM leads WHERE id = ? AND deleted_at IS NOT NULL")
                    .bind(id)
                    .fetch_optional(pool)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Archived lead '{id}' not found."))?;
            if let Some(org_id) = row.try_get::<Option<String>, _>("org_id")? {
                ensure_active_exists(pool, "orgs", &org_id, "org_id").await?;
            }
            restore_row(pool, "leads", id).await?;
        }
        "deal" => {
            let row = sqlx::query(
                "SELECT contact_id, org_id FROM deals WHERE id = ? AND deleted_at IS NOT NULL",
            )
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Archived deal '{id}' not found."))?;
            let contact_id = row.try_get::<String, _>("contact_id")?;
            ensure_active_exists(pool, "leads", &contact_id, "contact_id").await?;
            if let Some(org_id) = row.try_get::<Option<String>, _>("org_id")? {
                ensure_active_exists(pool, "orgs", &org_id, "org_id").await?;
            }
            restore_row(pool, "deals", id).await?;
        }
        "activity" => {
            let row = sqlx::query(
                "SELECT entity_id, entity_type FROM activities WHERE id = ? AND deleted_at IS NOT NULL",
            )
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Archived activity '{id}' not found."))?;
            let entity_id = row.try_get::<String, _>("entity_id")?;
            let entity_type_ref = row.try_get::<String, _>("entity_type")?;
            match entity_type_ref.as_str() {
                "lead" => ensure_active_exists(pool, "leads", &entity_id, "entity_id").await?,
                "deal" => ensure_active_exists(pool, "deals", &entity_id, "entity_id").await?,
                "org" => ensure_active_exists(pool, "orgs", &entity_id, "entity_id").await?,
                other => {
                    return Err(anyhow::anyhow!(
                        "unsupported activity entity_type '{other}'"
                    ))
                }
            }
            restore_row(pool, "activities", id).await?;
        }
        other => {
            return Err(anyhow::anyhow!(
                "param 'entity_type' must be one of: org, lead, deal, activity (got '{other}')"
            ))
        }
    }

    Ok(serde_json::json!({
        "success": true,
        "entity_type": entity_type,
        "id": id,
    }))
}

pub async fn archived_list(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let entity_type = args.get("entity_type").and_then(Value::as_str);
    let (limit, offset) = page_args(args);

    let sql = match entity_type {
        Some("org") => {
            "SELECT 'org' AS entity_type, id, name AS label, deleted_at FROM orgs WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC LIMIT ? OFFSET ?"
        }
        Some("lead") => {
            "SELECT 'lead' AS entity_type, id, name AS label, deleted_at FROM leads WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC LIMIT ? OFFSET ?"
        }
        Some("deal") => {
            "SELECT 'deal' AS entity_type, id, title AS label, deleted_at FROM deals WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC LIMIT ? OFFSET ?"
        }
        Some("activity") => {
            "SELECT 'activity' AS entity_type, id, title AS label, deleted_at FROM activities WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC LIMIT ? OFFSET ?"
        }
        Some(other) => {
            return Err(anyhow::anyhow!(
                "param 'entity_type' must be one of: org, lead, deal, activity (got '{other}')"
            ))
        }
        None => {
            "SELECT * FROM (
                SELECT 'org' AS entity_type, id, name AS label, deleted_at FROM orgs WHERE deleted_at IS NOT NULL
                UNION ALL
                SELECT 'lead' AS entity_type, id, name AS label, deleted_at FROM leads WHERE deleted_at IS NOT NULL
                UNION ALL
                SELECT 'deal' AS entity_type, id, title AS label, deleted_at FROM deals WHERE deleted_at IS NOT NULL
                UNION ALL
                SELECT 'activity' AS entity_type, id, title AS label, deleted_at FROM activities WHERE deleted_at IS NOT NULL
            ) archived_records ORDER BY deleted_at DESC LIMIT ? OFFSET ?"
        }
    };

    let rows = sqlx::query(sql)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

    let items = rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "entity_type": row.get::<String, _>("entity_type"),
                "id": row.get::<String, _>("id"),
                "label": row.get::<String, _>("label"),
                "deleted_at": row.get::<String, _>("deleted_at"),
            })
        })
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "archived_records": items,
        "limit": limit,
        "offset": offset,
    }))
}

pub async fn export_snapshot(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let include_archived = bool_arg(args, "include_archived")?.unwrap_or(true);

    let orgs = export_orgs(pool, include_archived).await?;
    let leads = export_leads(pool, include_archived).await?;
    let deals = export_deals(pool, include_archived).await?;
    let activities = export_activities(pool, include_archived).await?;

    Ok(serde_json::json!({
        "exported_at": Utc::now().to_rfc3339(),
        "include_archived": include_archived,
        "counts": {
            "organizations": orgs.len(),
            "leads": leads.len(),
            "deals": deals.len(),
            "activities": activities.len(),
        },
        "organizations": orgs,
        "leads": leads,
        "deals": deals,
        "activities": activities,
    }))
}

pub async fn require_confirmed_delete(args: &Map<String, Value>) -> Result<()> {
    let confirmed = bool_arg(args, "confirm_permanent")?.unwrap_or(false);
    if confirmed {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Permanent delete requires 'confirm_permanent': true. Prefer archive tools for safer record removal."
        ))
    }
}

async fn archive_row(pool: &SqlitePool, table: &str, id: &str, archived_at: &str) -> Result<()> {
    let sql = format!("UPDATE {table} SET deleted_at = ? WHERE id = ? AND deleted_at IS NULL");
    let result = sqlx::query(&sql)
        .bind(archived_at)
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(anyhow::anyhow!(
            "Active record '{id}' not found in {table}."
        ));
    }
    Ok(())
}

async fn restore_row(pool: &SqlitePool, table: &str, id: &str) -> Result<()> {
    let sql =
        format!("UPDATE {table} SET deleted_at = NULL WHERE id = ? AND deleted_at IS NOT NULL");
    let result = sqlx::query(&sql).bind(id).execute(pool).await?;

    if result.rows_affected() == 0 {
        return Err(anyhow::anyhow!(
            "Archived record '{id}' not found in {table}."
        ));
    }
    Ok(())
}

async fn ensure_active_exists(pool: &SqlitePool, table: &str, id: &str, field: &str) -> Result<()> {
    let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ? AND deleted_at IS NULL)");
    let exists: i64 = sqlx::query_scalar(&sql).bind(id).fetch_one(pool).await?;
    if exists == 0 {
        Err(anyhow::anyhow!(
            "Cannot restore record because {field} '{id}' is missing or archived."
        ))
    } else {
        Ok(())
    }
}

async fn active_count(pool: &SqlitePool, sql: &str, id: &str) -> Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>(sql)
        .bind(id)
        .fetch_one(pool)
        .await?)
}

async fn export_orgs(pool: &SqlitePool, include_archived: bool) -> Result<Vec<Value>> {
    let sql = if include_archived {
        "SELECT id, name, website, industry, size, country, phone, email, tags, notes, created_at, updated_at, deleted_at FROM orgs ORDER BY updated_at DESC"
    } else {
        "SELECT id, name, website, industry, size, country, phone, email, tags, notes, created_at, updated_at, deleted_at FROM orgs WHERE deleted_at IS NULL ORDER BY updated_at DESC"
    };
    let rows = sqlx::query(sql).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            inject_tags(
                serde_json::json!({
                    "id": row.get::<String, _>("id"),
                    "name": row.get::<String, _>("name"),
                    "website": row.get::<Option<String>, _>("website"),
                    "industry": row.get::<Option<String>, _>("industry"),
                    "size": row.get::<Option<String>, _>("size"),
                    "country": row.get::<Option<String>, _>("country"),
                    "phone": row.get::<Option<String>, _>("phone"),
                    "email": row.get::<Option<String>, _>("email"),
                    "notes": row.get::<Option<String>, _>("notes"),
                    "created_at": row.get::<String, _>("created_at"),
                    "updated_at": row.get::<String, _>("updated_at"),
                    "deleted_at": row.get::<Option<String>, _>("deleted_at"),
                }),
                &row.get::<String, _>("tags"),
            )
        })
        .collect())
}

async fn export_leads(pool: &SqlitePool, include_archived: bool) -> Result<Vec<Value>> {
    let sql = if include_archived {
        "SELECT id, name, email, phone, company, org_id, status, source, tags, notes, created_at, updated_at, deleted_at FROM leads ORDER BY updated_at DESC"
    } else {
        "SELECT id, name, email, phone, company, org_id, status, source, tags, notes, created_at, updated_at, deleted_at FROM leads WHERE deleted_at IS NULL ORDER BY updated_at DESC"
    };
    let rows = sqlx::query(sql).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            inject_tags(
                serde_json::json!({
                    "id": row.get::<String, _>("id"),
                    "name": row.get::<String, _>("name"),
                    "email": row.get::<Option<String>, _>("email"),
                    "phone": row.get::<Option<String>, _>("phone"),
                    "company": row.get::<Option<String>, _>("company"),
                    "org_id": row.get::<Option<String>, _>("org_id"),
                    "status": row.get::<String, _>("status"),
                    "source": row.get::<Option<String>, _>("source"),
                    "notes": row.get::<Option<String>, _>("notes"),
                    "created_at": row.get::<String, _>("created_at"),
                    "updated_at": row.get::<String, _>("updated_at"),
                    "deleted_at": row.get::<Option<String>, _>("deleted_at"),
                }),
                &row.get::<String, _>("tags"),
            )
        })
        .collect())
}

async fn export_deals(pool: &SqlitePool, include_archived: bool) -> Result<Vec<Value>> {
    let sql = if include_archived {
        "SELECT id, title, amount_minor, currency, stage, probability, contact_id, org_id, expected_close, tags, notes, created_at, updated_at, deleted_at FROM deals ORDER BY updated_at DESC"
    } else {
        "SELECT id, title, amount_minor, currency, stage, probability, contact_id, org_id, expected_close, tags, notes, created_at, updated_at, deleted_at FROM deals WHERE deleted_at IS NULL ORDER BY updated_at DESC"
    };
    let rows = sqlx::query(sql).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            inject_tags(
                serde_json::json!({
                    "id": row.get::<String, _>("id"),
                    "title": row.get::<String, _>("title"),
                    "amount": crate::utils::minor_to_amount(row.get::<i64, _>("amount_minor")),
                    "amount_minor": row.get::<i64, _>("amount_minor"),
                    "currency": row.get::<String, _>("currency"),
                    "stage": row.get::<String, _>("stage"),
                    "probability": row.get::<Option<i64>, _>("probability"),
                    "contact_id": row.get::<String, _>("contact_id"),
                    "org_id": row.get::<Option<String>, _>("org_id"),
                    "expected_close": row.get::<Option<String>, _>("expected_close"),
                    "notes": row.get::<Option<String>, _>("notes"),
                    "created_at": row.get::<String, _>("created_at"),
                    "updated_at": row.get::<String, _>("updated_at"),
                    "deleted_at": row.get::<Option<String>, _>("deleted_at"),
                }),
                &row.get::<String, _>("tags"),
            )
        })
        .collect())
}

async fn export_activities(pool: &SqlitePool, include_archived: bool) -> Result<Vec<Value>> {
    let sql = if include_archived {
        "SELECT id, entity_id, entity_type, kind, title, body, occurred_at, created_at, deleted_at FROM activities ORDER BY occurred_at DESC, created_at DESC"
    } else {
        "SELECT id, entity_id, entity_type, kind, title, body, occurred_at, created_at, deleted_at FROM activities WHERE deleted_at IS NULL ORDER BY occurred_at DESC, created_at DESC"
    };
    let rows = sqlx::query(sql).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "id": row.get::<String, _>("id"),
                "entity_id": row.get::<String, _>("entity_id"),
                "entity_type": row.get::<String, _>("entity_type"),
                "kind": row.get::<String, _>("kind"),
                "title": row.get::<String, _>("title"),
                "body": row.get::<Option<String>, _>("body"),
                "occurred_at": row.get::<String, _>("occurred_at"),
                "created_at": row.get::<String, _>("created_at"),
                "deleted_at": row.get::<Option<String>, _>("deleted_at"),
            })
        })
        .collect())
}
