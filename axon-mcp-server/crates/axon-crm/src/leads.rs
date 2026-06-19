use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::utils::{
    f64_arg, i64_arg, inject_tags, like, page_args, require_non_empty_str, string_opt,
    string_patch, tags_json_from_value, validate_choice, validate_currency, validate_email,
    validate_rfc3339_opt, DEAL_STAGES, LEAD_STATUSES,
};

#[derive(sqlx::FromRow)]
struct LeadRow {
    id: String,
    name: String,
    email: Option<String>,
    phone: Option<String>,
    company: Option<String>,
    org_id: Option<String>,
    status: String,
    source: Option<String>,
    tags: String,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

impl LeadRow {
    fn to_json(self) -> Value {
        let tags = self.tags.clone();
        inject_tags(
            serde_json::json!({
                "id": self.id,
                "name": self.name,
                "email": self.email,
                "phone": self.phone,
                "company": self.company,
                "org_id": self.org_id,
                "status": self.status,
                "source": self.source,
                "notes": self.notes,
                "created_at": self.created_at,
                "updated_at": self.updated_at,
            }),
            &tags,
        )
    }
}

pub async fn create(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let name = require_non_empty_str(args, "name")?;
    let email = string_opt(args, "email")?;
    let phone = string_opt(args, "phone")?;
    let company = string_opt(args, "company")?;
    let org_id = string_opt(args, "org_id")?;
    let status = string_opt(args, "status")?.unwrap_or_else(|| "Open".to_owned());
    let source = string_opt(args, "source")?;
    let notes = string_opt(args, "notes")?;
    let tags = tags_json_from_value(args.get("tags"))?.unwrap_or_else(|| "[]".to_owned());

    validate_choice(&status, LEAD_STATUSES, "status")?;
    validate_email("email", email.as_deref())?;

    if let Some(org_id) = org_id.as_deref() {
        ensure_org_exists(pool, org_id).await?;
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO leads
        (id, name, email, phone, company, org_id, status, source, tags, notes, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(email.as_deref())
    .bind(phone.as_deref())
    .bind(company.as_deref())
    .bind(org_id.as_deref())
    .bind(&status)
    .bind(source.as_deref())
    .bind(&tags)
    .bind(notes.as_deref())
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(serde_json::json!({ "success": true, "id": id, "name": name }))
}

pub async fn list(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let status = string_opt(args, "status")?.unwrap_or_else(|| "All".to_owned());
    let (limit, offset) = page_args(args);

    let (total, rows): (i64, Vec<LeadRow>) = if status == "All" {
        let total =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM leads WHERE deleted_at IS NULL")
                .fetch_one(pool)
                .await?;
        let rows = sqlx::query_as::<_, LeadRow>(
            "SELECT * FROM leads WHERE deleted_at IS NULL ORDER BY updated_at DESC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        (total, rows)
    } else {
        validate_choice(&status, LEAD_STATUSES, "status")?;
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM leads WHERE deleted_at IS NULL AND status = ?",
        )
        .bind(&status)
        .fetch_one(pool)
        .await?;
        let rows = sqlx::query_as::<_, LeadRow>(
            "SELECT * FROM leads WHERE deleted_at IS NULL AND status = ? ORDER BY updated_at DESC LIMIT ? OFFSET ?",
        )
        .bind(&status)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        (total, rows)
    };

    Ok(serde_json::json!({
        "leads": rows.into_iter().map(LeadRow::to_json).collect::<Vec<_>>(),
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Value> {
    sqlx::query_as::<_, LeadRow>("SELECT * FROM leads WHERE id = ? AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(LeadRow::to_json)
        .ok_or_else(|| anyhow::anyhow!("Lead '{id}' not found."))
}

pub async fn update(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let id = require_non_empty_str(args, "id")?;
    let existing =
        sqlx::query_as::<_, LeadRow>("SELECT * FROM leads WHERE id = ? AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Lead '{id}' not found."))?;

    let name = match string_patch(args, "name")? {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'name' cannot be empty")),
        None => existing.name,
    };
    let email = patch_or_existing(args, "email", existing.email)?;
    let phone = patch_or_existing(args, "phone", existing.phone)?;
    let company = patch_or_existing(args, "company", existing.company)?;
    let source = patch_or_existing(args, "source", existing.source)?;
    let notes = patch_or_existing(args, "notes", existing.notes)?;
    let org_id = patch_or_existing(args, "org_id", existing.org_id)?;
    let status = match string_patch(args, "status")? {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'status' cannot be empty")),
        None => existing.status,
    };
    let tags = tags_json_from_value(args.get("tags"))?.unwrap_or(existing.tags);
    let now = Utc::now().to_rfc3339();

    validate_choice(&status, LEAD_STATUSES, "status")?;
    validate_email("email", email.as_deref())?;

    if let Some(org_id) = org_id.as_deref() {
        ensure_org_exists(pool, org_id).await?;
    }

    sqlx::query(
        "UPDATE leads
        SET name = ?, email = ?, phone = ?, company = ?, org_id = ?, status = ?, source = ?,
            tags = ?, notes = ?, updated_at = ?
        WHERE id = ?",
    )
    .bind(&name)
    .bind(email.as_deref())
    .bind(phone.as_deref())
    .bind(company.as_deref())
    .bind(org_id.as_deref())
    .bind(&status)
    .bind(source.as_deref())
    .bind(&tags)
    .bind(notes.as_deref())
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;

    Ok(serde_json::json!({ "success": true, "id": id }))
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<Value> {
    let deal_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM deals WHERE contact_id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;

    if deal_count > 0 {
        return Err(anyhow::anyhow!(
            "Cannot delete lead '{id}': {deal_count} linked deal(s) exist. Remove them first."
        ));
    }

    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM activities WHERE entity_type = 'lead' AND entity_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    let result = sqlx::query("DELETE FROM leads WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    if result.rows_affected() == 0 {
        tx.rollback().await?;
        return Err(anyhow::anyhow!("Lead '{id}' not found."));
    }

    tx.commit().await?;
    Ok(serde_json::json!({ "success": true, "deleted_id": id }))
}

pub async fn search(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let query = require_non_empty_str(args, "query")?;
    let (limit, offset) = page_args(args);
    let pattern = like(query);

    let total = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM leads
        WHERE deleted_at IS NULL AND (
              name LIKE ? ESCAPE '\\'
           OR email LIKE ? ESCAPE '\\'
           OR company LIKE ? ESCAPE '\\'
           OR notes LIKE ? ESCAPE '\\'
           OR tags LIKE ? ESCAPE '\\')",
    )
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .fetch_one(pool)
    .await?;

    let rows = sqlx::query_as::<_, LeadRow>(
        "SELECT * FROM leads
        WHERE deleted_at IS NULL AND (
              name LIKE ? ESCAPE '\\'
           OR email LIKE ? ESCAPE '\\'
           OR company LIKE ? ESCAPE '\\'
           OR notes LIKE ? ESCAPE '\\'
           OR tags LIKE ? ESCAPE '\\')
        ORDER BY updated_at DESC
        LIMIT ? OFFSET ?",
    )
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    Ok(serde_json::json!({
        "results": rows.into_iter().map(LeadRow::to_json).collect::<Vec<_>>(),
        "total": total,
        "query": query,
        "limit": limit,
        "offset": offset,
    }))
}

pub async fn convert_to_deal(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let lead_id = require_non_empty_str(args, "lead_id")?;
    let lead =
        sqlx::query_as::<_, LeadRow>("SELECT * FROM leads WHERE id = ? AND deleted_at IS NULL")
            .bind(lead_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Lead '{lead_id}' not found."))?;

    let title = string_opt(args, "title")?.unwrap_or_else(|| {
        if let Some(company) = lead.company.as_deref() {
            format!("Opportunity - {company}")
        } else {
            format!("Opportunity - {}", lead.name)
        }
    });
    let amount = f64_arg(args, "amount")?.unwrap_or(0.0);
    let currency = string_opt(args, "currency")?.unwrap_or_else(|| "USD".to_owned());
    let stage = string_opt(args, "stage")?.unwrap_or_else(|| "Prospecting".to_owned());
    let probability = i64_arg(args, "probability")?;
    let org_id = string_opt(args, "org_id")?.or_else(|| lead.org_id.clone());
    let expected_close = string_opt(args, "expected_close")?;
    let notes = string_opt(args, "notes")?
        .or_else(|| Some(format!("Converted from lead '{}' ({})", lead.name, lead.id)));
    let mark_lead_status =
        string_opt(args, "lead_status")?.unwrap_or_else(|| "Qualified".to_owned());
    let tags = tags_json_from_value(args.get("tags"))?.unwrap_or(lead.tags.clone());

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
    validate_choice(&mark_lead_status, LEAD_STATUSES, "lead_status")?;
    validate_rfc3339_opt("expected_close", expected_close.as_deref())?;

    if let Some(org_id) = org_id.as_deref() {
        ensure_org_exists(pool, org_id).await?;
    }

    let deal_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO deals
        (id, title, amount, currency, stage, probability, contact_id, org_id, expected_close, tags, notes, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&deal_id)
    .bind(&title)
    .bind(amount)
    .bind(&currency)
    .bind(&stage)
    .bind(probability)
    .bind(lead_id)
    .bind(org_id.as_deref())
    .bind(expected_close.as_deref())
    .bind(&tags)
    .bind(notes.as_deref())
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE leads SET status = ?, updated_at = ? WHERE id = ?")
        .bind(&mark_lead_status)
        .bind(&now)
        .bind(lead_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(serde_json::json!({
        "success": true,
        "lead_id": lead_id,
        "deal_id": deal_id,
        "deal_title": title,
        "lead_status": mark_lead_status,
    }))
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
