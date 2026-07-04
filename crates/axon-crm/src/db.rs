use anyhow::{Context, Result};
use serde::Deserialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::time::Duration;
use std::{fs, path::Path};
use tracing::{info, warn};

pub async fn open(data_dir: &Path) -> Result<SqlitePool> {
    fs::create_dir_all(data_dir)?;
    let db_path = data_dir.join("crm.db");

    // Connect options apply to every connection the pool opens — a PRAGMA
    // executed through the pool only reaches one of its connections.
    // busy_timeout mirrors the agent DB (main.rs): without it, a connection
    // that finds the DB write-locked errors immediately with SQLITE_BUSY
    // instead of waiting (WAL still serializes writers). cache_size caps the
    // per-connection page cache at 1MB (default 2MB) for the 1GB host.
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10))
        .pragma("temp_store", "MEMORY")
        .pragma("cache_size", "-1024");

    // WAL serializes writers and CRM traffic is light; 3 connections cover
    // reads without keeping idle page caches around.
    let pool = SqlitePoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .map_err(|e| {
            anyhow::anyhow!("Failed to open CRM database at {}: {e}", db_path.display())
        })?;

    migrate(&pool).await?;
    import_legacy_json(&pool, data_dir).await?;
    Ok(pool)
}

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

/// Ordered, versioned schema migrations tracked via `PRAGMA user_version`
/// (same pattern as the agent DB in `crates/axon-agent/src/db/mod.rs`).
/// v1 is the pre-versioning schema as idempotent statements, so existing
/// databases (user_version 0) adopt versioning without changes.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "base_schema",
        sql: include_str!("migrations/0001_base_schema.sql"),
    },
    Migration {
        version: 2,
        name: "amount_minor_cents",
        sql: include_str!("migrations/0002_amount_minor_cents.sql"),
    },
    Migration {
        version: 3,
        name: "utc_timestamps",
        sql: include_str!("migrations/0003_utc_timestamps.sql"),
    },
    Migration {
        version: 4,
        name: "activity_tasks",
        sql: include_str!("migrations/0004_activity_tasks.sql"),
    },
];

async fn migrate(pool: &SqlitePool) -> Result<()> {
    let applied: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await
        .context("read CRM schema version")?;

    for m in MIGRATIONS {
        if m.version <= applied {
            continue;
        }
        let mut tx = pool.begin().await?;
        sqlx::raw_sql(m.sql)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("apply CRM migration {} ({})", m.version, m.name))?;
        sqlx::query(&format!("PRAGMA user_version = {}", m.version))
            .execute(&mut *tx)
            .await
            .with_context(|| format!("record CRM migration {}", m.version))?;
        tx.commit().await?;
        info!("CRM DB migration {} ({}) applied", m.version, m.name);
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
