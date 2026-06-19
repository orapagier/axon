use anyhow::{Context, Result};
use serde::Deserialize;
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool};
use std::{fs, path::Path};
use tracing::{info, warn};

pub async fn open(data_dir: &Path) -> Result<SqlitePool> {
    fs::create_dir_all(data_dir)?;
    let db_path = data_dir.join("crm.db");
    let url = format!("sqlite:{}?mode=rwc", db_path.display());

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .map_err(|e| {
            anyhow::anyhow!("Failed to open CRM database at {}: {e}", db_path.display())
        })?;

    migrate(&pool).await?;
    import_legacy_json(&pool, data_dir).await?;
    Ok(pool)
}

async fn migrate(pool: &SqlitePool) -> Result<()> {
    sqlx::query("PRAGMA journal_mode = WAL")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA synchronous = NORMAL")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA temp_store = MEMORY")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS orgs (
            id         TEXT PRIMARY KEY NOT NULL,
            name       TEXT NOT NULL,
            website    TEXT,
            industry   TEXT,
            size       TEXT,
            country    TEXT,
            phone      TEXT,
            email      TEXT,
            tags       TEXT NOT NULL DEFAULT '[]',
            notes      TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS leads (
            id         TEXT PRIMARY KEY NOT NULL,
            name       TEXT NOT NULL,
            email      TEXT,
            phone      TEXT,
            company    TEXT,
            org_id     TEXT REFERENCES orgs(id) ON DELETE SET NULL,
            status     TEXT NOT NULL CHECK(status IN ('Open', 'Contacted', 'Qualified', 'Lost')),
            source     TEXT,
            tags       TEXT NOT NULL DEFAULT '[]',
            notes      TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS deals (
            id             TEXT PRIMARY KEY NOT NULL,
            title          TEXT NOT NULL,
            amount         REAL NOT NULL DEFAULT 0 CHECK(amount >= 0),
            currency       TEXT NOT NULL DEFAULT 'USD',
            stage          TEXT NOT NULL CHECK(stage IN ('Prospecting', 'Qualified', 'Proposal', 'Negotiation', 'Won', 'Lost')),
            probability    INTEGER CHECK(probability IS NULL OR (probability >= 0 AND probability <= 100)),
            contact_id     TEXT NOT NULL REFERENCES leads(id) ON DELETE RESTRICT,
            org_id         TEXT REFERENCES orgs(id) ON DELETE SET NULL,
            expected_close TEXT,
            tags           TEXT NOT NULL DEFAULT '[]',
            notes          TEXT,
            created_at     TEXT NOT NULL,
            updated_at     TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS activities (
            id          TEXT PRIMARY KEY NOT NULL,
            entity_id   TEXT NOT NULL,
            entity_type TEXT NOT NULL CHECK(entity_type IN ('lead', 'deal', 'org')),
            kind        TEXT NOT NULL CHECK(kind IN ('note', 'call', 'email', 'meeting', 'task', 'other')),
            title       TEXT NOT NULL,
            body        TEXT,
            occurred_at TEXT NOT NULL,
            created_at  TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    ensure_column(pool, "orgs", "deleted_at", "TEXT").await?;
    ensure_column(pool, "leads", "deleted_at", "TEXT").await?;
    ensure_column(pool, "deals", "deleted_at", "TEXT").await?;
    ensure_column(pool, "activities", "deleted_at", "TEXT").await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_orgs_name ON orgs(name COLLATE NOCASE)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_orgs_industry ON orgs(industry)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_orgs_updated_at ON orgs(updated_at)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_orgs_deleted_at ON orgs(deleted_at)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_leads_status ON leads(status)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_leads_email ON leads(email COLLATE NOCASE)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_leads_org_id ON leads(org_id)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_leads_updated_at ON leads(updated_at)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_leads_deleted_at ON leads(deleted_at)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_deals_stage ON deals(stage)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_deals_contact_id ON deals(contact_id)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_deals_org_id ON deals(org_id)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_deals_expected_close ON deals(expected_close)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_deals_updated_at ON deals(updated_at)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_deals_deleted_at ON deals(deleted_at)")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_activities_entity ON activities(entity_id, entity_type)",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_activities_kind ON activities(kind)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_activities_occurred_at ON activities(occurred_at)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_activities_deleted_at ON activities(deleted_at)")
        .execute(pool)
        .await?;

    Ok(())
}

async fn ensure_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    column_type: &str,
) -> Result<()> {
    let pragma = format!("PRAGMA table_info({table})");
    let rows = sqlx::query(&pragma).fetch_all(pool).await?;
    let exists = rows
        .iter()
        .any(|row| row.get::<String, _>("name") == column);

    if !exists {
        let alter = format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}");
        sqlx::query(&alter).execute(pool).await?;
    }

    Ok(())
}

async fn import_legacy_json(pool: &SqlitePool, data_dir: &Path) -> Result<()> {
    let org_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orgs")
        .fetch_one(pool)
        .await?;
    if org_count == 0 {
        import_legacy_orgs(pool, &data_dir.join("crm_orgs.json")).await?;
    }

    let activity_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM activities")
        .fetch_one(pool)
        .await?;
    if activity_count == 0 {
        import_legacy_activities(pool, &data_dir.join("crm_activities.json")).await?;
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
struct LegacyOrg {
    id: String,
    name: String,
    website: Option<String>,
    industry: Option<String>,
    size: Option<String>,
    country: Option<String>,
    phone: Option<String>,
    email: Option<String>,
    tags: Vec<String>,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

async fn import_legacy_orgs(pool: &SqlitePool, path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let orgs: Vec<LegacyOrg> =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;

    if orgs.is_empty() {
        return Ok(());
    }

    info!(
        "Importing {} legacy CRM organizations from {}",
        orgs.len(),
        path.display()
    );
    for org in orgs {
        let tags = serde_json::to_string(&org.tags)?;
        sqlx::query(
            "INSERT OR IGNORE INTO orgs
            (id, name, website, industry, size, country, phone, email, tags, notes, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(org.id)
        .bind(org.name)
        .bind(org.website)
        .bind(org.industry)
        .bind(org.size)
        .bind(org.country)
        .bind(org.phone)
        .bind(org.email)
        .bind(tags)
        .bind(org.notes)
        .bind(org.created_at)
        .bind(org.updated_at)
        .execute(pool)
        .await?;
    }

    Ok(())
}

#[derive(Debug, Deserialize)]
struct LegacyActivity {
    id: String,
    entity_id: String,
    entity_type: String,
    kind: String,
    title: String,
    body: Option<String>,
    occurred_at: String,
    created_at: String,
}

async fn import_legacy_activities(pool: &SqlitePool, path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let activities: Vec<LegacyActivity> =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;

    if activities.is_empty() {
        return Ok(());
    }

    info!(
        "Importing {} legacy CRM activities from {}",
        activities.len(),
        path.display()
    );
    for activity in activities {
        if !entity_exists(pool, &activity.entity_type, &activity.entity_id).await? {
            warn!(
                "Skipping legacy activity {} because {} '{}' does not exist",
                activity.id, activity.entity_type, activity.entity_id
            );
            continue;
        }

        sqlx::query(
            "INSERT OR IGNORE INTO activities
            (id, entity_id, entity_type, kind, title, body, occurred_at, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(activity.id)
        .bind(activity.entity_id)
        .bind(activity.entity_type)
        .bind(activity.kind)
        .bind(activity.title)
        .bind(activity.body)
        .bind(activity.occurred_at)
        .bind(activity.created_at)
        .execute(pool)
        .await?;
    }

    Ok(())
}

async fn entity_exists(pool: &SqlitePool, entity_type: &str, entity_id: &str) -> Result<bool> {
    let sql = match entity_type {
        "lead" => "SELECT EXISTS(SELECT 1 FROM leads WHERE id = ?)",
        "deal" => "SELECT EXISTS(SELECT 1 FROM deals WHERE id = ?)",
        "org" => "SELECT EXISTS(SELECT 1 FROM orgs WHERE id = ?)",
        other => return Err(anyhow::anyhow!("unsupported entity type '{other}'")),
    };

    let exists: i64 = sqlx::query_scalar(sql)
        .bind(entity_id)
        .fetch_one(pool)
        .await?;
    Ok(exists != 0)
}
