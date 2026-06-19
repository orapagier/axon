use anyhow::Result;
use chrono::{Duration, Utc};
use serde_json::{Map, Value};
use sqlx::SqlitePool;

use crate::utils::{i64_arg, inject_tags, like, require_non_empty_str, DEAL_STAGES, LEAD_STATUSES};

#[derive(sqlx::FromRow)]
struct LeadCardRow {
    id: String,
    name: String,
    email: Option<String>,
    company: Option<String>,
    status: String,
    org_id: Option<String>,
    tags: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct DealCardRow {
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
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct OrgCardRow {
    id: String,
    name: String,
    website: Option<String>,
    industry: Option<String>,
    email: Option<String>,
    tags: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct ActivityCardRow {
    id: String,
    entity_id: String,
    entity_type: String,
    kind: String,
    title: String,
    body: Option<String>,
    occurred_at: String,
    created_at: String,
}

#[derive(sqlx::FromRow)]
struct CountByKeyRow {
    key: String,
    count: i64,
}

#[derive(sqlx::FromRow)]
struct DealStageRollupRow {
    stage: String,
    count: i64,
    total_value: f64,
}

impl LeadCardRow {
    fn to_json(self) -> Value {
        let tags = self.tags.clone();
        inject_tags(
            serde_json::json!({
                "id": self.id,
                "name": self.name,
                "email": self.email,
                "company": self.company,
                "status": self.status,
                "org_id": self.org_id,
                "updated_at": self.updated_at,
            }),
            &tags,
        )
    }
}

impl DealCardRow {
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
                "updated_at": self.updated_at,
            }),
            &tags,
        )
    }
}

impl OrgCardRow {
    fn to_json(self) -> Value {
        let tags = self.tags.clone();
        inject_tags(
            serde_json::json!({
                "id": self.id,
                "name": self.name,
                "website": self.website,
                "industry": self.industry,
                "email": self.email,
                "updated_at": self.updated_at,
            }),
            &tags,
        )
    }
}

impl ActivityCardRow {
    fn to_json(self) -> Value {
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

pub async fn search_all(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let query = require_non_empty_str(args, "query")?;
    let limit_per_type = i64_arg(args, "limit_per_type")?.unwrap_or(10).clamp(1, 50);
    let pattern = like(query);

    let orgs = sqlx::query_as::<_, OrgCardRow>(
        "SELECT id, name, website, industry, email, tags, updated_at
        FROM orgs
        WHERE deleted_at IS NULL AND (
              name LIKE ? ESCAPE '\\'
           OR industry LIKE ? ESCAPE '\\'
           OR country LIKE ? ESCAPE '\\'
           OR website LIKE ? ESCAPE '\\'
           OR notes LIKE ? ESCAPE '\\'
           OR tags LIKE ? ESCAPE '\\')
        ORDER BY updated_at DESC
        LIMIT ?",
    )
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(limit_per_type)
    .fetch_all(pool)
    .await?;

    let leads = sqlx::query_as::<_, LeadCardRow>(
        "SELECT id, name, email, company, status, org_id, tags, updated_at
        FROM leads
        WHERE deleted_at IS NULL AND (
              name LIKE ? ESCAPE '\\'
           OR email LIKE ? ESCAPE '\\'
           OR company LIKE ? ESCAPE '\\'
           OR notes LIKE ? ESCAPE '\\'
           OR tags LIKE ? ESCAPE '\\')
        ORDER BY updated_at DESC
        LIMIT ?",
    )
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(limit_per_type)
    .fetch_all(pool)
    .await?;

    let deals = sqlx::query_as::<_, DealCardRow>(
        "SELECT id, title, amount, currency, stage, probability, contact_id, org_id, expected_close, tags, updated_at
        FROM deals
        WHERE deleted_at IS NULL AND (
              title LIKE ? ESCAPE '\\'
           OR notes LIKE ? ESCAPE '\\'
           OR tags LIKE ? ESCAPE '\\')
        ORDER BY updated_at DESC
        LIMIT ?",
    )
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(limit_per_type)
    .fetch_all(pool)
    .await?;

    Ok(serde_json::json!({
        "query": query,
        "limit_per_type": limit_per_type,
        "total_results": orgs.len() + leads.len() + deals.len(),
        "counts": {
            "organizations": orgs.len(),
            "leads": leads.len(),
            "deals": deals.len(),
        },
        "organizations": orgs.into_iter().map(OrgCardRow::to_json).collect::<Vec<_>>(),
        "leads": leads.into_iter().map(LeadCardRow::to_json).collect::<Vec<_>>(),
        "deals": deals.into_iter().map(DealCardRow::to_json).collect::<Vec<_>>(),
    }))
}

pub async fn record_overview(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let entity_type = require_non_empty_str(args, "entity_type")?;
    let id = require_non_empty_str(args, "id")?;
    let related_limit = i64_arg(args, "related_limit")?.unwrap_or(10).clamp(1, 50);
    let activity_limit = i64_arg(args, "activity_limit")?.unwrap_or(20).clamp(1, 100);

    match entity_type {
        "lead" => lead_overview(pool, id, related_limit, activity_limit).await,
        "deal" => deal_overview(pool, id, related_limit, activity_limit).await,
        "org" => org_overview(pool, id, related_limit, activity_limit).await,
        other => Err(anyhow::anyhow!(
            "param 'entity_type' must be one of: lead, deal, org (got '{other}')"
        )),
    }
}

pub async fn dashboard_summary(pool: &SqlitePool, args: &Map<String, Value>) -> Result<Value> {
    let stale_days = i64_arg(args, "stale_days")?.unwrap_or(30).clamp(1, 3650);
    let closing_within_days = i64_arg(args, "closing_within_days")?
        .unwrap_or(30)
        .clamp(1, 3650);
    let activity_window_days = i64_arg(args, "activity_window_days")?
        .unwrap_or(30)
        .clamp(1, 3650);
    let list_limit = i64_arg(args, "list_limit")?.unwrap_or(10).clamp(1, 50);

    let now = Utc::now();
    let now_rfc3339 = now.to_rfc3339();
    let stale_cutoff = (now - Duration::days(stale_days)).to_rfc3339();
    let closing_cutoff = (now + Duration::days(closing_within_days)).to_rfc3339();
    let activity_cutoff = (now - Duration::days(activity_window_days)).to_rfc3339();

    let total_orgs =
        scalar_count(pool, "SELECT COUNT(*) FROM orgs WHERE deleted_at IS NULL").await?;
    let total_leads =
        scalar_count(pool, "SELECT COUNT(*) FROM leads WHERE deleted_at IS NULL").await?;
    let total_deals =
        scalar_count(pool, "SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL").await?;
    let recent_activity_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM activities WHERE deleted_at IS NULL AND occurred_at >= ?",
    )
    .bind(&activity_cutoff)
    .fetch_one(pool)
    .await?;

    let lead_status_counts = sqlx::query_as::<_, CountByKeyRow>(
        "SELECT status AS key, COUNT(*) AS count FROM leads WHERE deleted_at IS NULL GROUP BY status",
    )
    .fetch_all(pool)
    .await?;
    let deal_stage_rollup = sqlx::query_as::<_, DealStageRollupRow>(
        "SELECT stage, COUNT(*) AS count, CAST(COALESCE(SUM(amount), 0) AS REAL) AS total_value
        FROM deals
        WHERE deleted_at IS NULL
        GROUP BY stage",
    )
    .fetch_all(pool)
    .await?;

    let active_pipeline_value = sqlx::query_scalar::<_, f64>(
        "SELECT CAST(COALESCE(SUM(amount), 0) AS REAL)
        FROM deals
        WHERE deleted_at IS NULL AND stage NOT IN ('Won', 'Lost')",
    )
    .fetch_one(pool)
    .await?;
    let weighted_pipeline_value = sqlx::query_scalar::<_, f64>(
        "SELECT CAST(COALESCE(SUM(amount * COALESCE(probability, 0) / 100.0), 0) AS REAL)
        FROM deals
        WHERE deleted_at IS NULL AND stage NOT IN ('Won', 'Lost')",
    )
    .fetch_one(pool)
    .await?;
    let stale_leads = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM leads
        WHERE deleted_at IS NULL AND updated_at < ? AND status NOT IN ('Qualified', 'Lost')",
    )
    .bind(&stale_cutoff)
    .fetch_one(pool)
    .await?;
    let stale_deals = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM deals
        WHERE deleted_at IS NULL AND updated_at < ? AND stage NOT IN ('Won', 'Lost')",
    )
    .bind(&stale_cutoff)
    .fetch_one(pool)
    .await?;
    let overdue_deals_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM deals
        WHERE deleted_at IS NULL
          AND expected_close IS NOT NULL
          AND expected_close < ?
          AND stage NOT IN ('Won', 'Lost')",
    )
    .bind(&now_rfc3339)
    .fetch_one(pool)
    .await?;
    let closing_soon_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM deals
        WHERE deleted_at IS NULL
          AND expected_close IS NOT NULL
          AND expected_close >= ?
          AND expected_close <= ?
          AND stage NOT IN ('Won', 'Lost')",
    )
    .bind(&now_rfc3339)
    .bind(&closing_cutoff)
    .fetch_one(pool)
    .await?;

    let stale_deal_list = sqlx::query_as::<_, DealCardRow>(
        "SELECT id, title, amount, currency, stage, probability, contact_id, org_id, expected_close, tags, updated_at
        FROM deals
        WHERE deleted_at IS NULL
          AND updated_at < ?
          AND stage NOT IN ('Won', 'Lost')
        ORDER BY updated_at ASC
        LIMIT ?",
    )
    .bind(&stale_cutoff)
    .bind(list_limit)
    .fetch_all(pool)
    .await?;
    let closing_soon_deals = sqlx::query_as::<_, DealCardRow>(
        "SELECT id, title, amount, currency, stage, probability, contact_id, org_id, expected_close, tags, updated_at
        FROM deals
        WHERE deleted_at IS NULL
          AND expected_close IS NOT NULL
          AND expected_close >= ?
          AND expected_close <= ?
          AND stage NOT IN ('Won', 'Lost')
        ORDER BY expected_close ASC
        LIMIT ?",
    )
    .bind(&now_rfc3339)
    .bind(&closing_cutoff)
    .bind(list_limit)
    .fetch_all(pool)
    .await?;

    Ok(serde_json::json!({
        "generated_at": now_rfc3339,
        "parameters": {
            "stale_days": stale_days,
            "closing_within_days": closing_within_days,
            "activity_window_days": activity_window_days,
            "list_limit": list_limit,
        },
        "totals": {
            "organizations": total_orgs,
            "leads": total_leads,
            "deals": total_deals,
            "recent_activities": recent_activity_count,
        },
        "lead_status_counts": counts_with_defaults(LEAD_STATUSES, &lead_status_counts),
        "deal_stage_rollup": stage_rollup_with_defaults(DEAL_STAGES, &deal_stage_rollup),
        "pipeline": {
            "active_pipeline_value": active_pipeline_value,
            "weighted_pipeline_value": weighted_pipeline_value,
            "stale_leads": stale_leads,
            "stale_deals": stale_deals,
            "overdue_deals_count": overdue_deals_count,
            "closing_soon_count": closing_soon_count,
        },
        "closing_soon_deals": closing_soon_deals.into_iter().map(DealCardRow::to_json).collect::<Vec<_>>(),
        "stale_deals": stale_deal_list.into_iter().map(DealCardRow::to_json).collect::<Vec<_>>(),
    }))
}

async fn lead_overview(
    pool: &SqlitePool,
    id: &str,
    related_limit: i64,
    activity_limit: i64,
) -> Result<Value> {
    let lead = sqlx::query_as::<_, LeadCardRow>(
        "SELECT id, name, email, company, status, org_id, tags, updated_at FROM leads WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Lead '{id}' not found."))?;

    let related_org = if let Some(org_id) = lead.org_id.as_deref() {
        sqlx::query_as::<_, OrgCardRow>(
            "SELECT id, name, website, industry, email, tags, updated_at FROM orgs WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(org_id)
        .fetch_optional(pool)
        .await?
        .map(OrgCardRow::to_json)
    } else {
        None
    };

    let related_deals = sqlx::query_as::<_, DealCardRow>(
        "SELECT id, title, amount, currency, stage, probability, contact_id, org_id, expected_close, tags, updated_at
        FROM deals
        WHERE deleted_at IS NULL AND contact_id = ?
        ORDER BY updated_at DESC
        LIMIT ?",
    )
    .bind(id)
    .bind(related_limit)
    .fetch_all(pool)
    .await?;
    let activities = recent_activities(pool, "lead", id, activity_limit).await?;

    Ok(serde_json::json!({
        "entity_type": "lead",
        "entity": lead.to_json(),
        "linked": {
            "organization": related_org,
            "deals": related_deals.into_iter().map(DealCardRow::to_json).collect::<Vec<_>>(),
        },
        "recent_activities": activities,
        "summary": {
            "deal_count": sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL AND contact_id = ?").bind(id).fetch_one(pool).await?,
            "activity_count": sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM activities WHERE deleted_at IS NULL AND entity_type = 'lead' AND entity_id = ?").bind(id).fetch_one(pool).await?,
        }
    }))
}

async fn deal_overview(
    pool: &SqlitePool,
    id: &str,
    _related_limit: i64,
    activity_limit: i64,
) -> Result<Value> {
    let deal = sqlx::query_as::<_, DealCardRow>(
        "SELECT id, title, amount, currency, stage, probability, contact_id, org_id, expected_close, tags, updated_at
        FROM deals WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Deal '{id}' not found."))?;

    let lead = sqlx::query_as::<_, LeadCardRow>(
        "SELECT id, name, email, company, status, org_id, tags, updated_at FROM leads WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(&deal.contact_id)
    .fetch_optional(pool)
    .await?
    .map(LeadCardRow::to_json);

    let org = if let Some(org_id) = deal.org_id.as_deref() {
        sqlx::query_as::<_, OrgCardRow>(
            "SELECT id, name, website, industry, email, tags, updated_at FROM orgs WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(org_id)
        .fetch_optional(pool)
        .await?
        .map(OrgCardRow::to_json)
    } else {
        None
    };

    let activities = recent_activities(pool, "deal", id, activity_limit).await?;

    Ok(serde_json::json!({
        "entity_type": "deal",
        "entity": deal.to_json(),
        "linked": {
            "lead": lead,
            "organization": org,
        },
        "recent_activities": activities,
        "summary": {
            "activity_count": sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM activities WHERE deleted_at IS NULL AND entity_type = 'deal' AND entity_id = ?").bind(id).fetch_one(pool).await?,
        }
    }))
}

async fn org_overview(
    pool: &SqlitePool,
    id: &str,
    related_limit: i64,
    activity_limit: i64,
) -> Result<Value> {
    let org = sqlx::query_as::<_, OrgCardRow>(
        "SELECT id, name, website, industry, email, tags, updated_at FROM orgs WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("Organization '{id}' not found."))?;

    let leads = sqlx::query_as::<_, LeadCardRow>(
        "SELECT id, name, email, company, status, org_id, tags, updated_at
        FROM leads
        WHERE deleted_at IS NULL AND org_id = ?
        ORDER BY updated_at DESC
        LIMIT ?",
    )
    .bind(id)
    .bind(related_limit)
    .fetch_all(pool)
    .await?;

    let deals = sqlx::query_as::<_, DealCardRow>(
        "SELECT id, title, amount, currency, stage, probability, contact_id, org_id, expected_close, tags, updated_at
        FROM deals
        WHERE deleted_at IS NULL AND org_id = ?
        ORDER BY updated_at DESC
        LIMIT ?",
    )
    .bind(id)
    .bind(related_limit)
    .fetch_all(pool)
    .await?;

    let activities = recent_activities(pool, "org", id, activity_limit).await?;

    Ok(serde_json::json!({
        "entity_type": "org",
        "entity": org.to_json(),
        "linked": {
            "leads": leads.into_iter().map(LeadCardRow::to_json).collect::<Vec<_>>(),
            "deals": deals.into_iter().map(DealCardRow::to_json).collect::<Vec<_>>(),
        },
        "recent_activities": activities,
        "summary": {
            "lead_count": sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM leads WHERE deleted_at IS NULL AND org_id = ?").bind(id).fetch_one(pool).await?,
            "deal_count": sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM deals WHERE deleted_at IS NULL AND org_id = ?").bind(id).fetch_one(pool).await?,
            "activity_count": sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM activities WHERE deleted_at IS NULL AND entity_type = 'org' AND entity_id = ?").bind(id).fetch_one(pool).await?,
        }
    }))
}

async fn recent_activities(
    pool: &SqlitePool,
    entity_type: &str,
    entity_id: &str,
    limit: i64,
) -> Result<Vec<Value>> {
    let rows = sqlx::query_as::<_, ActivityCardRow>(
        "SELECT id, entity_id, entity_type, kind, title, body, occurred_at, created_at
        FROM activities
        WHERE deleted_at IS NULL AND entity_type = ? AND entity_id = ?
        ORDER BY occurred_at DESC, created_at DESC
        LIMIT ?",
    )
    .bind(entity_type)
    .bind(entity_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(ActivityCardRow::to_json).collect())
}

async fn scalar_count(pool: &SqlitePool, sql: &str) -> Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>(sql).fetch_one(pool).await?)
}

fn counts_with_defaults(allowed: &[&str], rows: &[CountByKeyRow]) -> Vec<Value> {
    allowed
        .iter()
        .map(|name| {
            let count = rows
                .iter()
                .find(|row| row.key == *name)
                .map(|row| row.count)
                .unwrap_or(0);
            serde_json::json!({ "key": name, "count": count })
        })
        .collect()
}

fn stage_rollup_with_defaults(allowed: &[&str], rows: &[DealStageRollupRow]) -> Vec<Value> {
    allowed
        .iter()
        .map(|stage| {
            let row = rows.iter().find(|row| row.stage == *stage);
            serde_json::json!({
                "stage": stage,
                "count": row.map(|row| row.count).unwrap_or(0),
                "total_value": row.map(|row| row.total_value).unwrap_or(0.0),
            })
        })
        .collect()
}
