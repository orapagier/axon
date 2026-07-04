use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value};
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use crate::utils::{
    amount_arg_minor, check_len, i64_arg, inject_tags, like, minor_to_amount, page_args,
    parse_rfc3339_utc, require_non_empty_str, string_opt, string_patch, tags_json_from_value,
    validate_choice, validate_currency, DEAL_STAGES, MAX_NAME_LEN, MAX_TEXT_LEN,
};

#[derive(sqlx::FromRow)]
struct DealRow {
    id: String,
    title: String,
    amount_minor: i64,
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
    fn into_json(self) -> Value {
        let tags = self.tags.clone();
        inject_tags(
            serde_json::json!({
                "id": self.id,
                "title": self.title,
                "amount": minor_to_amount(self.amount_minor),
                "amount_minor": self.amount_minor,
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

/// One `SUM(amount_minor)` bucket of a per-currency aggregation.
#[derive(sqlx::FromRow)]
pub(crate) struct CurrencyTotalRow {
    pub(crate) currency: String,
    pub(crate) total_minor: i64,
}

/// `{"USD": 125000.0, "EUR": 4000.0}` â€” amounts in different currencies must
/// never be added into one number.
pub(crate) fn currency_totals_json(rows: Vec<CurrencyTotalRow>) -> Value {
    let mut map = Map::new();
    for row in rows {
        map.insert(row.currency, Value::from(minor_to_amount(row.total_minor)));
    }
    Value::Object(map)
}

pub async fn create(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let title = require_non_empty_str(args, "title")?;
    let contact_id = require_non_empty_str(args, "contact_id")?;
    let amount_minor = amount_arg_minor(args, "amount")?.unwrap_or(0);
    let currency = string_opt(args, "currency")?.unwrap_or_else(crate::default_currency);
    let stage = string_opt(args, "stage")?.unwrap_or_else(|| "Prospecting".to_owned());
    let probability = i64_arg(args, "probability")?;
    let org_id = string_opt(args, "org_id")?;
    let expected_close = string_opt(args, "expected_close")?;
    let notes = string_opt(args, "notes")?;
    let tags = tags_json_from_value(args.get("tags"))?.unwrap_or_else(|| "[]".to_owned());

    if let Some(probability) = probability {
        if !(0..=100).contains(&probability) {
            return Err(anyhow::anyhow!(
                "param 'probability' must be between 0 and 100"
            ));
        }
    }
    check_len("title", Some(title), MAX_NAME_LEN)?;
    check_len("notes", notes.as_deref(), MAX_TEXT_LEN)?;
    validate_currency("currency", &currency)?;
    validate_choice(&stage, DEAL_STAGES, "stage")?;
    let expected_close = parse_rfc3339_utc("expected_close", expected_close.as_deref())?;

    // Idempotency guard: a retrying agent re-creating the same deal gets a
    // teaching error with the existing id instead of a second pipeline entry.
    // A contact may hold many deals, so only the same title collides.
    let allow_duplicate = crate::utils::bool_arg(args, "allow_duplicate")?.unwrap_or(false);
    if !allow_duplicate {
        ensure_no_duplicate_deal(pool, contact_id, title, "crm_deal_create").await?;
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let mut tx = pool.begin().await?;
    ensure_lead_exists(&mut tx, contact_id).await?;
    if let Some(org_id) = org_id.as_deref() {
        ensure_org_exists(&mut tx, org_id).await?;
    }

    sqlx::query(
        "INSERT INTO deals
        (id, title, amount_minor, currency, stage, probability, contact_id, org_id, expected_close, tags, notes, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(title)
    .bind(amount_minor)
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
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(serde_json::json!({ "success": true, "id": id, "title": title }))
}

pub async fn list(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let stage = string_opt(args, "stage")?.unwrap_or_else(|| "All".to_owned());
    let (limit, offset) = page_args(args);

    let (total, total_value, rows): (i64, Value, Vec<DealRow>) = if stage == "All" {
        let total =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL")
                .fetch_one(pool)
                .await?;
        let totals = sqlx::query_as::<_, CurrencyTotalRow>(
            "SELECT currency, SUM(amount_minor) AS total_minor
            FROM deals WHERE deleted_at IS NULL GROUP BY currency",
        )
        .fetch_all(pool)
        .await?;
        let rows = sqlx::query_as::<_, DealRow>(
            "SELECT * FROM deals WHERE deleted_at IS NULL ORDER BY updated_at DESC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        (total, currency_totals_json(totals), rows)
    } else {
        validate_choice(&stage, DEAL_STAGES, "stage")?;
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL AND stage = ?",
        )
        .bind(&stage)
        .fetch_one(pool)
        .await?;
        let totals = sqlx::query_as::<_, CurrencyTotalRow>(
            "SELECT currency, SUM(amount_minor) AS total_minor
            FROM deals WHERE deleted_at IS NULL AND stage = ? GROUP BY currency",
        )
        .bind(&stage)
        .fetch_all(pool)
        .await?;
        let rows = sqlx::query_as::<_, DealRow>(
            "SELECT * FROM deals WHERE deleted_at IS NULL AND stage = ? ORDER BY updated_at DESC LIMIT ? OFFSET ?",
        )
        .bind(&stage)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        (total, currency_totals_json(totals), rows)
    };

    Ok(serde_json::json!({
        "deals": rows.into_iter().map(DealRow::into_json).collect::<Vec<_>>(),
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
        .map(DealRow::into_json)
        .ok_or_else(|| anyhow::anyhow!("Deal '{id}' not found."))
}

pub async fn update(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let id = require_non_empty_str(args, "id")?;
    let mut tx = pool.begin().await?;

    let existing =
        sqlx::query_as::<_, DealRow>("SELECT * FROM deals WHERE id = ? AND deleted_at IS NULL")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Deal '{id}' not found."))?;

    let title = match string_patch(args, "title")? {
        Some(Some(value)) => value,
        Some(None) => return Err(anyhow::anyhow!("param 'title' cannot be empty")),
        None => existing.title,
    };
    let amount_minor = amount_arg_minor(args, "amount")?.unwrap_or(existing.amount_minor);
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
    let expected_close = parse_rfc3339_utc("expected_close", expected_close.as_deref())?;
    ensure_lead_exists(&mut tx, &contact_id).await?;
    if let Some(org_id) = org_id.as_deref() {
        ensure_org_exists(&mut tx, org_id).await?;
    }

    sqlx::query(
        "UPDATE deals
        SET title = ?, amount_minor = ?, currency = ?, stage = ?, probability = ?, contact_id = ?,
            org_id = ?, expected_close = ?, tags = ?, notes = ?, updated_at = ?
        WHERE id = ?",
    )
    .bind(&title)
    .bind(amount_minor)
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
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

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
        "results": rows.into_iter().map(DealRow::into_json).collect::<Vec<_>>(),
        "total": total,
        "query": query,
        "limit": limit,
        "offset": offset,
    }))
}

pub async fn pipeline_summary(pool: &SqlitePool) -> Result<Value> {
    #[derive(sqlx::FromRow)]
    struct StageCurrencyRow {
        stage: String,
        currency: String,
        count: i64,
        total_minor: i64,
    }

    let rows = sqlx::query_as::<_, StageCurrencyRow>(
        "SELECT stage, currency, COUNT(*) AS count, SUM(amount_minor) AS total_minor
        FROM deals
        WHERE deleted_at IS NULL
        GROUP BY stage, currency
        ORDER BY CASE stage
            WHEN 'Prospecting' THEN 1
            WHEN 'Qualified' THEN 2
            WHEN 'Proposal' THEN 3
            WHEN 'Negotiation' THEN 4
            WHEN 'Won' THEN 5
            WHEN 'Lost' THEN 6
            ELSE 7
        END, currency",
    )
    .fetch_all(pool)
    .await?;

    // Rows arrive ordered by stage, so consecutive rows with the same stage
    // fold into one entry with a per-currency value map.
    let mut pipeline: Vec<Value> = Vec::new();
    for row in rows {
        let is_same_stage = pipeline
            .last()
            .map(|entry| entry["stage"] == row.stage.as_str())
            .unwrap_or(false);
        if !is_same_stage {
            pipeline.push(serde_json::json!({
                "stage": row.stage,
                "count": 0,
                "total_value": {},
            }));
        }
        let entry = pipeline.last_mut().expect("entry pushed above");
        entry["count"] = Value::from(entry["count"].as_i64().unwrap_or(0) + row.count);
        entry["total_value"][&row.currency] = Value::from(minor_to_amount(row.total_minor));
    }

    let total_deals =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL")
            .fetch_one(pool)
            .await?;
    let total_value = sqlx::query_as::<_, CurrencyTotalRow>(
        "SELECT currency, SUM(amount_minor) AS total_minor
        FROM deals WHERE deleted_at IS NULL GROUP BY currency",
    )
    .fetch_all(pool)
    .await?;
    let won_value = sqlx::query_as::<_, CurrencyTotalRow>(
        "SELECT currency, SUM(amount_minor) AS total_minor
        FROM deals WHERE deleted_at IS NULL AND stage = 'Won' GROUP BY currency",
    )
    .fetch_all(pool)
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
        "pipeline": pipeline,
        "total_deals": total_deals,
        "closed_deals": closed_deals,
        "total_value": currency_totals_json(total_value),
        "won_value": currency_totals_json(won_value),
        "win_rate_pct": win_rate,
        "won_share_of_all_deals_pct": won_share_of_all_deals,
    }))
}

/// Duplicate-deal guard shared by `crm_deal_create` and
/// `crm_lead_convert_to_deal`: another ACTIVE deal for the same contact with
/// the same title (case-insensitive) is almost always an agent retry, not a
/// real second opportunity.
pub(crate) async fn ensure_no_duplicate_deal(
    pool: &SqlitePool,
    contact_id: &str,
    title: &str,
    caller: &str,
) -> Result<()> {
    if let Some((dup_id, dup_stage)) = sqlx::query_as::<_, (String, String)>(
        "SELECT id, stage FROM deals
        WHERE deleted_at IS NULL AND contact_id = ? AND lower(title) = lower(?) LIMIT 1",
    )
    .bind(contact_id)
    .bind(title)
    .fetch_optional(pool)
    .await?
    {
        return Err(anyhow::anyhow!(
            "A deal titled '{title}' already exists for this contact (id: {dup_id}, stage: {dup_stage}). \
             Update it with crm_deal_update, or pass 'allow_duplicate': true to {caller} to create a second one anyway."
        ));
    }
    Ok(())
}

pub(crate) async fn ensure_lead_exists(conn: &mut SqliteConnection, lead_id: &str) -> Result<()> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM leads WHERE id = ? AND deleted_at IS NULL)",
    )
    .bind(lead_id)
    .fetch_one(conn)
    .await?;

    if exists == 0 {
        Err(anyhow::anyhow!(
            "contact_id '{lead_id}' does not match any lead."
        ))
    } else {
        Ok(())
    }
}

pub(crate) async fn ensure_org_exists(conn: &mut SqliteConnection, org_id: &str) -> Result<()> {
    let exists: i64 =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM orgs WHERE id = ? AND deleted_at IS NULL)")
            .bind(org_id)
            .fetch_one(conn)
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
