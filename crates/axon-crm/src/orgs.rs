use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::utils::{
    check_len, inject_tags, like, page_args, require_non_empty_str, string_opt, string_patch,
    tags_json_from_value, validate_email, MAX_CONTACT_LEN, MAX_NAME_LEN, MAX_TEXT_LEN,
};

#[allow(clippy::too_many_arguments)]
fn check_org_lens(
    name: &str,
    website: Option<&str>,
    industry: Option<&str>,
    size: Option<&str>,
    country: Option<&str>,
    phone: Option<&str>,
    email: Option<&str>,
    notes: Option<&str>,
) -> Result<()> {
    check_len("name", Some(name), MAX_NAME_LEN)?;
    check_len("website", website, MAX_NAME_LEN)?;
    check_len("industry", industry, MAX_NAME_LEN)?;
    check_len("size", size, MAX_NAME_LEN)?;
    check_len("country", country, MAX_NAME_LEN)?;
    check_len("phone", phone, MAX_CONTACT_LEN)?;
    check_len("email", email, MAX_CONTACT_LEN)?;
    check_len("notes", notes, MAX_TEXT_LEN)?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct OrgRow {
    id: String,
    name: String,
    website: Option<String>,
    industry: Option<String>,
    size: Option<String>,
    country: Option<String>,
    phone: Option<String>,
    email: Option<String>,
    tags: String,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

impl OrgRow {
    fn into_json(self) -> Value {
        let tags = self.tags.clone();
        inject_tags(
            serde_json::json!({
                "id": self.id,
                "name": self.name,
                "website": self.website,
                "industry": self.industry,
                "size": self.size,
                "country": self.country,
                "phone": self.phone,
                "email": self.email,
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
    let website = string_opt(args, "website")?;
    let industry = string_opt(args, "industry")?;
    let size = string_opt(args, "size")?;
    let country = string_opt(args, "country")?;
    let phone = string_opt(args, "phone")?;
    let email = string_opt(args, "email")?;
    let notes = string_opt(args, "notes")?;
    let tags = tags_json_from_value(args.get("tags"))?.unwrap_or_else(|| "[]".to_owned());

    validate_email("email", email.as_deref())?;
    check_org_lens(
        name,
        website.as_deref(),
        industry.as_deref(),
        size.as_deref(),
        country.as_deref(),
        phone.as_deref(),
        email.as_deref(),
        notes.as_deref(),
    )?;

    // Duplicate guard: same pattern as crm_lead_create — a teaching error with
    // the existing id unless the caller explicitly allows the duplicate.
    let allow_duplicate = crate::utils::bool_arg(args, "allow_duplicate")?.unwrap_or(false);
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM orgs WHERE deleted_at IS NULL AND lower(name) = lower(?) LIMIT 1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await?;
    if let Some((dup_id,)) = &existing {
        if !allow_duplicate {
            return Err(anyhow::anyhow!(
                "An organization named '{name}' already exists (id: {dup_id}). \
                 Use the existing record or crm_org_update, or pass 'allow_duplicate': true to create a second one anyway."
            ));
        }
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO orgs
        (id, name, website, industry, size, country, phone, email, tags, notes, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(website.as_deref())
    .bind(industry.as_deref())
    .bind(size.as_deref())
    .bind(country.as_deref())
    .bind(phone.as_deref())
    .bind(email.as_deref())
    .bind(&tags)
    .bind(notes.as_deref())
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    let mut result = serde_json::json!({ "success": true, "id": id, "name": name });
    if let Some((dup_id,)) = &existing {
        result["warning"] = Value::String(format!(
            "Created as an allowed duplicate: another organization named '{name}' exists (id: {dup_id})."
        ));
    }
    Ok(result)
}

pub async fn list(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let industry = string_opt(args, "industry")?;
    let (limit, offset) = page_args(args);

    let (total, rows): (i64, Vec<OrgRow>) = if let Some(industry) = industry.as_deref() {
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM orgs WHERE deleted_at IS NULL AND lower(industry) = lower(?)",
        )
        .bind(industry)
        .fetch_one(pool)
        .await?;
        let rows = sqlx::query_as::<_, OrgRow>(
            "SELECT * FROM orgs
            WHERE deleted_at IS NULL AND lower(industry) = lower(?)
            ORDER BY updated_at DESC
            LIMIT ? OFFSET ?",
        )
        .bind(industry)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        (total, rows)
    } else {
        let total =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM orgs WHERE deleted_at IS NULL")
                .fetch_one(pool)
                .await?;
        let rows = sqlx::query_as::<_, OrgRow>(
            "SELECT * FROM orgs WHERE deleted_at IS NULL ORDER BY updated_at DESC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        (total, rows)
    };

    Ok(serde_json::json!({
        "organizations": rows.into_iter().map(OrgRow::into_json).collect::<Vec<_>>(),
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Value> {
    sqlx::query_as::<_, OrgRow>("SELECT * FROM orgs WHERE id = ? AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(OrgRow::into_json)
        .ok_or_else(|| anyhow::anyhow!("Organization '{id}' not found."))
}

pub async fn update(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let id = require_non_empty_str(args, "id")?;
    let existing =
        sqlx::query_as::<_, OrgRow>("SELECT * FROM orgs WHERE id = ? AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Organization '{id}' not found."))?;

    let name = match string_patch(args, "name")? {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'name' cannot be empty")),
        None => existing.name,
    };
    let website = patch_or_existing(args, "website", existing.website)?;
    let industry = patch_or_existing(args, "industry", existing.industry)?;
    let size = patch_or_existing(args, "size", existing.size)?;
    let country = patch_or_existing(args, "country", existing.country)?;
    let phone = patch_or_existing(args, "phone", existing.phone)?;
    let email = patch_or_existing(args, "email", existing.email)?;
    let notes = patch_or_existing(args, "notes", existing.notes)?;
    let tags = tags_json_from_value(args.get("tags"))?.unwrap_or(existing.tags);
    let now = Utc::now().to_rfc3339();

    validate_email("email", email.as_deref())?;
    check_org_lens(
        &name,
        website.as_deref(),
        industry.as_deref(),
        size.as_deref(),
        country.as_deref(),
        phone.as_deref(),
        email.as_deref(),
        notes.as_deref(),
    )?;

    // Same duplicate-name guard as create, applied only when this call renames
    // the org, and never tripped by the org's own current name.
    let allow_duplicate = crate::utils::bool_arg(args, "allow_duplicate")?.unwrap_or(false);
    if !allow_duplicate && args.contains_key("name") {
        if let Some((dup_id,)) = sqlx::query_as::<_, (String,)>(
            "SELECT id FROM orgs
            WHERE deleted_at IS NULL AND lower(name) = lower(?) AND id != ? LIMIT 1",
        )
        .bind(&name)
        .bind(id)
        .fetch_optional(pool)
        .await?
        {
            return Err(anyhow::anyhow!(
                "An organization named '{name}' already exists (id: {dup_id}). \
                 Use that record, or pass 'allow_duplicate': true to rename anyway."
            ));
        }
    }

    sqlx::query(
        "UPDATE orgs
        SET name = ?, website = ?, industry = ?, size = ?, country = ?, phone = ?, email = ?,
            tags = ?, notes = ?, updated_at = ?
        WHERE id = ?",
    )
    .bind(&name)
    .bind(website.as_deref())
    .bind(industry.as_deref())
    .bind(size.as_deref())
    .bind(country.as_deref())
    .bind(phone.as_deref())
    .bind(email.as_deref())
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

    sqlx::query("DELETE FROM activities WHERE entity_type = 'org' AND entity_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    let result = sqlx::query("DELETE FROM orgs WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    if result.rows_affected() == 0 {
        tx.rollback().await?;
        return Err(anyhow::anyhow!("Organization '{id}' not found."));
    }

    tx.commit().await?;
    Ok(serde_json::json!({ "success": true, "deleted_id": id }))
}

pub async fn search(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let query = require_non_empty_str(args, "query")?;
    let (limit, offset) = page_args(args);
    let pattern = like(query);

    // Phone matches both verbatim and separator-stripped (see leads::search).
    let phone_pattern = like(&crate::utils::normalize_phone(query));
    let where_clause = format!(
        "deleted_at IS NULL AND (
              name LIKE ?1 ESCAPE '\\'
           OR industry LIKE ?1 ESCAPE '\\'
           OR country LIKE ?1 ESCAPE '\\'
           OR website LIKE ?1 ESCAPE '\\'
           OR phone LIKE ?1 ESCAPE '\\'
           OR {} LIKE ?2 ESCAPE '\\'
           OR email LIKE ?1 ESCAPE '\\'
           OR notes LIKE ?1 ESCAPE '\\'
           OR tags LIKE ?1 ESCAPE '\\')",
        crate::utils::phone_match_sql("phone")
    );

    let total =
        sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM orgs WHERE {where_clause}"))
            .bind(&pattern)
            .bind(&phone_pattern)
            .fetch_one(pool)
            .await?;

    let rows = sqlx::query_as::<_, OrgRow>(&format!(
        "SELECT * FROM orgs WHERE {where_clause}
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
        "results": rows.into_iter().map(OrgRow::into_json).collect::<Vec<_>>(),
        "total": total,
        "query": query,
        "limit": limit,
        "offset": offset,
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
