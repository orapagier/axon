use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::utils::{
    f64_arg, i64_arg, inject_tags, like, page_args, require_non_empty_str, string_opt,
    string_patch, tags_json_from_value, validate_choice, validate_currency, validate_rfc3339_opt,
    DEAL_STAGES,
};

#[derive(sqlx::FromRow)]
struct DealRow {
    id: String,
    title: String,
    amount: f64,
    currency: String,
    stage: String,
    probability: Option<i64>,
    contact_id: String,
    org_id: Option<String>,
    expected_close: Option<String>,
    tags: String,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

impl DealRow {
    fn to_json(self) -> Value {
        let tags = self.tags.clone();
        inject_tags(
            serde_json::json!({
                "id": self.id,
                "title": self.title,
                "amount": self.amount,
                "currency": self.currency,
                "stage": self.stage,
                "probability": self.probability,
                "contact_id": self.contact_id,
                "org_id": self.org_id,
                "expected_close": self.expected_close,
                "notes": self.notes,
                "created_at": self.created_at,
                "updated_at": self.updated_at,
            }),
            &tags,
        )
    }
}

pub async fn create(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let title = require_non_empty_str(args, "title")?;
    let contact_id = require_non_empty_str(args, "contact_id")?;
    let amount = f64_arg(args, "amount")?.unwrap_or(0.0);
    let currency = string_opt(args, "currency")?.unwrap_or_else(|| "USD".to_owned());
    let stage = string_opt(args, "stage")?.unwrap_or_else(|| "Prospecting".to_owned());
    let probability = i64_arg(args, "probability")?;
    let org_id = string_opt(args, "org_id")?;
    let expected_close = string_opt(args, "expected_close")?;
    let notes = string_opt(args, "notes")?;
    let tags = tags_json_from_value(args.get("tags"))?.unwrap_or_else(|| "[]".to_owned());

    if amount < 0.0 {
        return Err(anyhow::anyhow!("param 'amount' must be >= 0"));
    }
    if let Some(probability) = probability {
        if !(0..=100).contains(&probability) {
            return Err(anyhow::anyhow!(
                "param 'probability' must be between 0 and 100"
            ));
        }
    }
    validate_currency("currency", &currency)?;
    validate_choice(&stage, DEAL_STAGES, "stage")?;
    validate_rfc3339_opt("expected_close", expected_close.as_deref())?;
    ensure_lead_exists(pool, contact_id).await?;
    if let Some(org_id) = org_id.as_deref() {
        ensure_org_exists(pool, org_id).await?;
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO deals
        (id, title, amount, currency, stage, probability, contact_id, org_id, expected_close, tags, notes, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(title)
    .bind(amount)
    .bind(&currency)
    .bind(&stage)
    .bind(probability)
    .bind(contact_id)
    .bind(org_id.as_deref())
    .bind(expected_close.as_deref())
    .bind(&tags)
    .bind(notes.as_deref())
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(serde_json::json!({ "success": true, "id": id, "title": title }))
}

pub async fn list(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let stage = string_opt(args, "stage")?.unwrap_or_else(|| "All".to_owned());
    let (limit, offset) = page_args(args);

    let (total, total_value, rows): (i64, f64, Vec<DealRow>) = if stage == "All" {
        let total =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL")
                .fetch_one(pool)
                .await?;
        let total_value = sqlx::query_scalar::<_, f64>(
            "SELECT CAST(COALESCE(SUM(amount), 0) AS REAL) FROM deals WHERE deleted_at IS NULL",
        )
        .fetch_one(pool)
        .await?;
        let rows = sqlx::query_as::<_, DealRow>(
            "SELECT * FROM deals WHERE deleted_at IS NULL ORDER BY updated_at DESC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        (total, total_value, rows)
    } else {
        validate_choice(&stage, DEAL_STAGES, "stage")?;
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL AND stage = ?",
        )
        .bind(&stage)
        .fetch_one(pool)
        .await?;
        let total_value = sqlx::query_scalar::<_, f64>(
            "SELECT CAST(COALESCE(SUM(amount), 0) AS REAL) FROM deals WHERE deleted_at IS NULL AND stage = ?",
        )
        .bind(&stage)
        .fetch_one(pool)
        .await?;
        let rows = sqlx::query_as::<_, DealRow>(
            "SELECT * FROM deals WHERE deleted_at IS NULL AND stage = ? ORDER BY updated_at DESC LIMIT ? OFFSET ?",
        )
        .bind(&stage)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        (total, total_value, rows)
    };

    Ok(serde_json::json!({
        "deals": rows.into_iter().map(DealRow::to_json).collect::<Vec<_>>(),
        "total": total,
        "total_value": total_value,
        "limit": limit,
        "offset": offset,
    }))
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Value> {
    sqlx::query_as::<_, DealRow>("SELECT * FROM deals WHERE id = ? AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(DealRow::to_json)
        .ok_or_else(|| anyhow::anyhow!("Deal '{id}' not found."))
}

pub async fn update(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let id = require_non_empty_str(args, "id")?;
    let existing =
        sqlx::query_as::<_, DealRow>("SELECT * FROM deals WHERE id = ? AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Deal '{id}' not found."))?;

    let title = match string_patch(args, "title")? {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'title' cannot be empty")),
        None => existing.title,
    };
    let amount = f64_arg(args, "amount")?.unwrap_or(existing.amount);
    let currency = match string_patch(args, "currency")? {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'currency' cannot be empty")),
        None => existing.currency,
    };
    let stage = match string_patch(args, "stage")? {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'stage' cannot be empty")),
        None => existing.stage,
    };
    let probability = match i64_arg(args, "probability")? {
        Some(value) => Some(value),
        None if args.contains_key("probability") => None,
        None => existing.probability,
    };
    let contact_id = match string_patch(args, "contact_id")? {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'contact_id' cannot be empty")),
        None => existing.contact_id,
    };
    let org_id = patch_or_existing(args, "org_id", existing.org_id)?;
    let expected_close = patch_or_existing(args, "expected_close", existing.expected_close)?;
    let notes = patch_or_existing(args, "notes", existing.notes)?;
    let tags = tags_json_from_value(args.get("tags"))?.unwrap_or(existing.tags);
    let now = Utc::now().to_rfc3339();

    if amount < 0.0 {
        return Err(anyhow::anyhow!("param 'amount' must be >= 0"));
    }
    if let Some(probability) = probability {
        if !(0..=100).contains(&probability) {
            return Err(anyhow::anyhow!(
                "param 'probability' must be between 0 and 100"
            ));
        }
    }
    validate_currency("currency", &currency)?;
    validate_choice(&stage, DEAL_STAGES, "stage")?;
    validate_rfc3339_opt("expected_close", expected_close.as_deref())?;
    ensure_lead_exists(pool, &contact_id).await?;
    if let Some(org_id) = org_id.as_deref() {
        ensure_org_exists(pool, org_id).await?;
    }

    sqlx::query(
        "UPDATE deals
        SET title = ?, amount = ?, currency = ?, stage = ?, probability = ?, contact_id = ?,
            org_id = ?, expected_close = ?, tags = ?, notes = ?, updated_at = ?
        WHERE id = ?",
    )
    .bind(&title)
    .bind(amount)
    .bind(&currency)
    .bind(&stage)
    .bind(probability)
    .bind(&contact_id)
    .bind(org_id.as_deref())
    .bind(expected_close.as_deref())
    .bind(&tags)
    .bind(notes.as_deref())
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;

    Ok(serde_json::json!({ "success": true, "id": id }))
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<Value> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM activities WHERE entity_type = 'deal' AND entity_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    let result = sqlx::query("DELETE FROM deals WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    if result.rows_affected() == 0 {
        tx.rollback().await?;
        return Err(anyhow::anyhow!("Deal '{id}' not found."));
    }

    tx.commit().await?;
    Ok(serde_json::json!({ "success": true, "deleted_id": id }))
}

pub async fn search(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let query = require_non_empty_str(args, "query")?;
    let (limit, offset) = page_args(args);
    let pattern = like(query);

    let total = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM deals
        WHERE deleted_at IS NULL AND (
              title LIKE ? ESCAPE '\\'
           OR notes LIKE ? ESCAPE '\\'
           OR tags LIKE ? ESCAPE '\\')",
    )
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .fetch_one(pool)
    .await?;

    let rows = sqlx::query_as::<_, DealRow>(
        "SELECT * FROM deals
        WHERE deleted_at IS NULL AND (
              title LIKE ? ESCAPE '\\'
           OR notes LIKE ? ESCAPE '\\'
           OR tags LIKE ? ESCAPE '\\')
        ORDER BY updated_at DESC
        LIMIT ? OFFSET ?",
    )
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    Ok(serde_json::json!({
        "results": rows.into_iter().map(DealRow::to_json).collect::<Vec<_>>(),
        "total": total,
        "query": query,
        "limit": limit,
        "offset": offset,
    }))
}

pub async fn pipeline_summary(pool: &SqlitePool) -> Result<Value> {
    #[derive(sqlx::FromRow)]
    struct StageSummary {
        stage: String,
        count: i64,
        total_value: f64,
    }

    let rows = sqlx::query_as::<_, StageSummary>(
        "SELECT stage, COUNT(*) AS count, CAST(COALESCE(SUM(amount), 0) AS REAL) AS total_value
        FROM deals
        WHERE deleted_at IS NULL
        GROUP BY stage
        ORDER BY CASE stage
            WHEN 'Prospecting' THEN 1
            WHEN 'Qualified' THEN 2
            WHEN 'Proposal' THEN 3
            WHEN 'Negotiation' THEN 4
            WHEN 'Won' THEN 5
            WHEN 'Lost' THEN 6
            ELSE 7
        END",
    )
    .fetch_all(pool)
    .await?;

    let total_deals =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL")
            .fetch_one(pool)
            .await?;
    let total_value = sqlx::query_scalar::<_, f64>(
        "SELECT CAST(COALESCE(SUM(amount), 0) AS REAL) FROM deals WHERE deleted_at IS NULL",
    )
    .fetch_one(pool)
    .await?;
    let won_value = sqlx::query_scalar::<_, f64>(
        "SELECT CAST(COALESCE(SUM(amount), 0) AS REAL) FROM deals WHERE deleted_at IS NULL AND stage = 'Won'",
    )
    .fetch_one(pool)
    .await?;
    let won_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL AND stage = 'Won'",
    )
    .fetch_one(pool)
    .await?;
    let lost_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL AND stage = 'Lost'",
    )
    .fetch_one(pool)
    .await?;

    let closed_deals = won_count + lost_count;
    let win_rate = if closed_deals > 0 {
        (won_count as f64 / closed_deals as f64 * 1000.0).round() / 10.0
    } else {
        0.0
    };
    let won_share_of_all_deals = if total_deals > 0 {
        (won_count as f64 / total_deals as f64 * 1000.0).round() / 10.0
    } else {
        0.0
    };

    Ok(serde_json::json!({
        "pipeline": rows.into_iter().map(|row| serde_json::json!({
            "stage": row.stage,
            "count": row.count,
            "total_value": row.total_value,
        })).collect::<Vec<_>>(),
        "total_deals": total_deals,
        "closed_deals": closed_deals,
        "total_value": total_value,
        "won_value": won_value,
        "win_rate_pct": win_rate,
        "won_share_of_all_deals_pct": won_share_of_all_deals,
    }))
}

async fn ensure_lead_exists(pool: &SqlitePool, lead_id: &str) -> Result<()> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM leads WHERE id = ? AND deleted_at IS NULL)",
    )
    .bind(lead_id)
    .fetch_one(pool)
    .await?;

    if exists == 0 {
        Err(anyhow::anyhow!(
            "contact_id '{lead_id}' does not match any lead."
        ))
    } else {
        Ok(())
    }
}

async fn ensure_org_exists(pool: &SqlitePool, org_id: &str) -> Result<()> {
    let exists: i64 =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM orgs WHERE id = ? AND deleted_at IS NULL)")
            .bind(org_id)
            .fetch_one(pool)
            .await?;

    if exists == 0 {
        Err(anyhow::anyhow!(
            "org_id '{org_id}' does not match any organization."
        ))
    } else {
        Ok(())
    }
}

fn patch_or_existing(
    args: &Map<String, Value>,
    key: &str,
    existing: Option<String>,
) -> Result<Option<String>> {
    match string_patch(args, key)? {
        Some(value) => Ok(value),
        None => Ok(existing),
    }
}
