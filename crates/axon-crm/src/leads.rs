use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::deals::{ensure_no_duplicate_deal, ensure_org_exists};
use crate::utils::{
    amount_arg_minor, check_len, i64_arg, inject_tags, like, normalize_phone, page_args,
    parse_rfc3339_utc, phone_match_sql, require_non_empty_str, string_opt, string_patch,
    tags_json_from_value, validate_choice, validate_currency, validate_email, DEAL_STAGES,
    LEAD_STATUSES, MAX_CONTACT_LEN, MAX_NAME_LEN, MAX_TEXT_LEN,
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
    fn into_json(self) -> Value {
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

fn check_lead_lens(
    name: &str,
    email: Option<&str>,
    phone: Option<&str>,
    company: Option<&str>,
    source: Option<&str>,
    notes: Option<&str>,
) -> Result<()> {
    check_len("name", Some(name), MAX_NAME_LEN)?;
    check_len("email", email, MAX_CONTACT_LEN)?;
    check_len("phone", phone, MAX_CONTACT_LEN)?;
    check_len("company", company, MAX_NAME_LEN)?;
    check_len("source", source, MAX_NAME_LEN)?;
    check_len("notes", notes, MAX_TEXT_LEN)?;
    Ok(())
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
    check_lead_lens(
        name,
        email.as_deref(),
        phone.as_deref(),
        company.as_deref(),
        source.as_deref(),
        notes.as_deref(),
    )?;

    // Duplicate guard: agents in a loop will happily re-create the same
    // contact. A teaching error carrying the existing id steers them to
    // crm_lead_update instead; 'allow_duplicate': true overrides deliberately.
    // Phone matters as much as email here — walk-in/call-in customers often
    // have no email at all.
    let allow_duplicate = crate::utils::bool_arg(args, "allow_duplicate")?.unwrap_or(false);
    if !allow_duplicate {
        ensure_no_duplicate_contact(pool, email.as_deref(), phone.as_deref(), None).await?;
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let mut tx = pool.begin().await?;
    if let Some(org_id) = org_id.as_deref() {
        ensure_org_exists(&mut tx, org_id).await?;
    }

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
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

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
        "leads": rows.into_iter().map(LeadRow::into_json).collect::<Vec<_>>(),
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
        .map(LeadRow::into_json)
        .ok_or_else(|| anyhow::anyhow!("Lead '{id}' not found."))
}

pub async fn update(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let id = require_non_empty_str(args, "id")?;
    let mut tx = pool.begin().await?;

    let existing =
        sqlx::query_as::<_, LeadRow>("SELECT * FROM leads WHERE id = ? AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&mut *tx)
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
    check_lead_lens(
        &name,
        email.as_deref(),
        phone.as_deref(),
        company.as_deref(),
        source.as_deref(),
        notes.as_deref(),
    )?;

    // Same duplicate guard as create, but only for the fields this call is
    // actually changing — untouched values never block an unrelated update.
    let allow_duplicate = crate::utils::bool_arg(args, "allow_duplicate")?.unwrap_or(false);
    if !allow_duplicate {
        let changed_email = args.contains_key("email");
        let changed_phone = args.contains_key("phone");
        if changed_email || changed_phone {
            ensure_no_duplicate_contact(
                pool,
                email.as_deref().filter(|_| changed_email),
                phone.as_deref().filter(|_| changed_phone),
                Some(id),
            )
            .await?;
        }
    }

    if let Some(org_id) = org_id.as_deref() {
        ensure_org_exists(&mut tx, org_id).await?;
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
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(serde_json::json!({ "success": true, "id": id }))
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<Value> {
    let active_deals = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM deals WHERE contact_id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;

    if active_deals > 0 {
        return Err(anyhow::anyhow!(
            "Cannot delete lead '{id}': {active_deals} linked deal(s) exist. Remove them first."
        ));
    }

    // Archived deals still hold an ON DELETE RESTRICT reference; surface a
    // teaching error instead of letting the raw FK failure through.
    let archived_deals = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM deals WHERE contact_id = ? AND deleted_at IS NOT NULL",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;

    if archived_deals > 0 {
        return Err(anyhow::anyhow!(
            "Cannot delete lead '{id}': {archived_deals} archived deal(s) still reference it. Restore and delete them first, or archive the lead instead."
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

    // Phone matches both verbatim and separator-stripped, so "0917 555 1234"
    // finds a lead stored as "0917-555-1234".
    let phone_pattern = crate::utils::like(&normalize_phone(query));
    let where_clause = format!(
        "deleted_at IS NULL AND (
              name LIKE ?1 ESCAPE '\\'
           OR email LIKE ?1 ESCAPE '\\'
           OR phone LIKE ?1 ESCAPE '\\'
           OR {} LIKE ?2 ESCAPE '\\'
           OR company LIKE ?1 ESCAPE '\\'
           OR notes LIKE ?1 ESCAPE '\\'
           OR tags LIKE ?1 ESCAPE '\\')",
        phone_match_sql("phone")
    );

    let total =
        sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM leads WHERE {where_clause}"))
            .bind(&pattern)
            .bind(&phone_pattern)
            .fetch_one(pool)
            .await?;

    let rows = sqlx::query_as::<_, LeadRow>(&format!(
        "SELECT * FROM leads WHERE {where_clause}
        ORDER BY updated_at DESC
        LIMIT ?3 OFFSET ?4"
    ))
    .bind(&pattern)
    .bind(&phone_pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    Ok(serde_json::json!({
        "results": rows.into_iter().map(LeadRow::into_json).collect::<Vec<_>>(),
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
    let amount_minor = amount_arg_minor(args, "amount")?.unwrap_or(0);
    let currency = string_opt(args, "currency")?.unwrap_or_else(crate::default_currency);
    let stage = string_opt(args, "stage")?.unwrap_or_else(|| "Prospecting".to_owned());
    let probability = i64_arg(args, "probability")?;
    let org_id = string_opt(args, "org_id")?.or_else(|| lead.org_id.clone());
    let expected_close = string_opt(args, "expected_close")?;
    let notes = string_opt(args, "notes")?
        .or_else(|| Some(format!("Converted from lead '{}' ({})", lead.name, lead.id)));
    let mark_lead_status =
        string_opt(args, "lead_status")?.unwrap_or_else(|| "Qualified".to_owned());
    let tags = tags_json_from_value(args.get("tags"))?.unwrap_or(lead.tags.clone());

    if let Some(probability) = probability {
        if !(0..=100).contains(&probability) {
            return Err(anyhow::anyhow!(
                "param 'probability' must be between 0 and 100"
            ));
        }
    }
    check_len("title", Some(&title), MAX_NAME_LEN)?;
    check_len("notes", notes.as_deref(), MAX_TEXT_LEN)?;
    validate_currency("currency", &currency)?;
    validate_choice(&stage, DEAL_STAGES, "stage")?;
    validate_choice(&mark_lead_status, LEAD_STATUSES, "lead_status")?;
    let expected_close = parse_rfc3339_utc("expected_close", expected_close.as_deref())?;

    // Idempotency guard: a retrying agent must not convert the same lead into
    // two deals. The default title is deterministic, so a plain re-conversion
    // collides here and gets the existing deal id back.
    let allow_duplicate = crate::utils::bool_arg(args, "allow_duplicate")?.unwrap_or(false);
    if !allow_duplicate {
        ensure_no_duplicate_deal(pool, lead_id, &title, "crm_lead_convert_to_deal").await?;
    }

    let deal_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;

    if let Some(org_id) = org_id.as_deref() {
        ensure_org_exists(&mut tx, org_id).await?;
    }

    sqlx::query(
        "INSERT INTO deals
        (id, title, amount_minor, currency, stage, probability, contact_id, org_id, expected_close, tags, notes, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&deal_id)
    .bind(&title)
    .bind(amount_minor)
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

/// Duplicate-contact guard shared by create and update: rejects (with a
/// teaching error carrying the existing id) when another active lead already
/// uses this email (case-insensitive) or phone (separator-insensitive, see
/// [`normalize_phone`]). `exclude_id` skips the record being updated.
async fn ensure_no_duplicate_contact(
    pool: &SqlitePool,
    email: Option<&str>,
    phone: Option<&str>,
    exclude_id: Option<&str>,
) -> Result<()> {
    let exclude = exclude_id.unwrap_or("");

    if let Some(email) = email {
        if let Some((dup_id, dup_name)) = sqlx::query_as::<_, (String, String)>(
            "SELECT id, name FROM leads
            WHERE deleted_at IS NULL AND lower(email) = lower(?) AND id != ? LIMIT 1",
        )
        .bind(email)
        .bind(exclude)
        .fetch_optional(pool)
        .await?
        {
            return Err(anyhow::anyhow!(
                "A lead with email '{email}' already exists: '{dup_name}' (id: {dup_id}). \
                 Update it with crm_lead_update, or pass 'allow_duplicate': true to keep both."
            ));
        }
    }

    if let Some(phone) = phone {
        let normalized = normalize_phone(phone);
        if !normalized.is_empty() {
            let sql = format!(
                "SELECT id, name FROM leads
                WHERE deleted_at IS NULL AND phone IS NOT NULL AND {} = ? AND id != ? LIMIT 1",
                phone_match_sql("phone")
            );
            if let Some((dup_id, dup_name)) = sqlx::query_as::<_, (String, String)>(&sql)
                .bind(&normalized)
                .bind(exclude)
                .fetch_optional(pool)
                .await?
            {
                return Err(anyhow::anyhow!(
                    "A lead with phone '{phone}' already exists: '{dup_name}' (id: {dup_id}). \
                     Update it with crm_lead_update, or pass 'allow_duplicate': true to keep both."
                ));
            }
        }
    }

    Ok(())
}
