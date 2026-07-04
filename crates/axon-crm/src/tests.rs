use crate::{activities, db, deals, leads, orgs, records, views};
use anyhow::Result;
use chrono::{Duration, FixedOffset, Utc};
use serde_json::{json, Map, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::{fs, path::PathBuf};
use uuid::Uuid;

fn args(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("expected JSON object")
}

fn string_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field '{key}'"))
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-tmp")
            .join(Uuid::new_v4().to_string());
        fs::create_dir_all(&path).expect("failed to create test dir");
        Self { path }
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

async fn test_pool() -> Result<(TestDir, SqlitePool)> {
    let dir = TestDir::new();
    let pool = db::open(dir.path()).await?;
    Ok((dir, pool))
}

#[tokio::test]
async fn crm_round_trip_crud_and_summary() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let org = orgs::create(
        &pool,
        &args(json!({
            "name": "Acme Corp",
            "industry": "Software",
            "email": "ops@acme.test",
            "tags": ["vip", "software", "vip"]
        })),
    )
    .await?;
    let org_id = string_field(&org, "id").to_owned();

    let lead = leads::create(
        &pool,
        &args(json!({
            "name": "Taylor Buyer",
            "email": "taylor@buyer.test",
            "org_id": org_id,
            "status": "Qualified",
            "tags": ["inbound"]
        })),
    )
    .await?;
    let lead_id = string_field(&lead, "id").to_owned();

    let deal = deals::create(
        &pool,
        &args(json!({
            "title": "Enterprise Expansion",
            "contact_id": lead_id,
            "amount": 12500.0,
            "currency": "USD",
            "stage": "Proposal",
            "probability": 70,
            "expected_close": "2026-05-01T12:00:00Z"
        })),
    )
    .await?;
    let deal_id = string_field(&deal, "id").to_owned();

    let _activity = activities::log(
        &pool,
        &args(json!({
            "entity_id": deal_id,
            "entity_type": "deal",
            "kind": "meeting",
            "title": "Proposal review",
            "occurred_at": "2026-04-25T10:00:00Z"
        })),
    )
    .await?;

    let pipeline = deals::pipeline_summary(&pool).await?;
    assert_eq!(pipeline["total_deals"], json!(1));
    assert_eq!(pipeline["closed_deals"], json!(0));
    assert_eq!(pipeline["total_value"], json!({ "USD": 12500.0 }));

    let activities_for_deal = activities::list(
        &pool,
        &args(json!({
            "entity_id": string_field(&deal, "id"),
            "entity_type": "deal"
        })),
    )
    .await?;
    assert_eq!(activities_for_deal["total"], json!(1));

    let listed_orgs = orgs::list(&pool, &args(json!({ "industry": "Software" }))).await?;
    assert_eq!(listed_orgs["total"], json!(1));

    Ok(())
}

#[tokio::test]
async fn deleting_lead_with_open_deals_is_blocked_and_activity_requires_real_entity() -> Result<()>
{
    let (_dir, pool) = test_pool().await?;

    let lead = leads::create(&pool, &args(json!({ "name": "Jordan Prospect" }))).await?;
    let lead_id = string_field(&lead, "id").to_owned();

    deals::create(
        &pool,
        &args(json!({
            "title": "Pilot Contract",
            "contact_id": lead_id,
            "amount": 5000.0,
            "currency": "USD"
        })),
    )
    .await?;

    let delete_err = leads::delete(&pool, string_field(&lead, "id"))
        .await
        .expect_err("expected delete to be blocked");
    assert!(delete_err.to_string().contains("linked deal(s) exist"));

    let activity_err = activities::log(
        &pool,
        &args(json!({
            "entity_id": "missing-id",
            "entity_type": "lead",
            "title": "Ghost note"
        })),
    )
    .await
    .expect_err("expected invalid activity entity to fail");
    assert!(activity_err.to_string().contains("does not exist"));

    Ok(())
}

#[tokio::test]
async fn legacy_json_is_imported_once_into_sqlite() -> Result<()> {
    let dir = TestDir::new();
    let now = "2026-04-25T00:00:00Z";
    let org_id = Uuid::new_v4().to_string();

    fs::write(
        dir.path().join("crm_orgs.json"),
        serde_json::to_string_pretty(&json!([
            {
                "id": org_id,
                "name": "Legacy Org",
                "website": "https://legacy.test",
                "industry": "Consulting",
                "size": "11-50",
                "country": "US",
                "phone": null,
                "email": "legacy@org.test",
                "tags": ["legacy"],
                "notes": "Migrated from JSON",
                "created_at": now,
                "updated_at": now
            }
        ]))?,
    )?;

    fs::write(
        dir.path().join("crm_activities.json"),
        serde_json::to_string_pretty(&json!([
            {
                "id": Uuid::new_v4().to_string(),
                "entity_id": org_id,
                "entity_type": "org",
                "kind": "note",
                "title": "Legacy note",
                "body": "Imported activity",
                "occurred_at": now,
                "created_at": now
            }
        ]))?,
    )?;

    let pool = db::open(dir.path()).await?;

    let org_results = orgs::search(&pool, &args(json!({ "query": "Legacy Org" }))).await?;
    assert_eq!(org_results["total"], json!(1));

    let activity_results = activities::list(
        &pool,
        &args(json!({
            "entity_id": org_id,
            "entity_type": "org"
        })),
    )
    .await?;
    assert_eq!(activity_results["total"], json!(1));

    Ok(())
}

#[tokio::test]
async fn workflow_tools_cover_search_overview_conversion_and_activity_updates() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let org = orgs::create(
        &pool,
        &args(json!({
            "name": "Northwind Health",
            "industry": "Healthcare",
            "email": "hello@northwind.test"
        })),
    )
    .await?;
    let org_id = string_field(&org, "id").to_owned();

    let lead = leads::create(
        &pool,
        &args(json!({
            "name": "Morgan Wells",
            "email": "morgan@northwind.test",
            "company": "Northwind Health",
            "org_id": org_id,
            "status": "Contacted",
            "notes": "Interested in a pilot rollout",
            "tags": ["priority", "healthcare"]
        })),
    )
    .await?;
    let lead_id = string_field(&lead, "id").to_owned();

    let activity = activities::log(
        &pool,
        &args(json!({
            "entity_id": lead_id,
            "entity_type": "lead",
            "kind": "call",
            "title": "Discovery call",
            "body": "Good fit",
            "occurred_at": "2026-04-24T09:00:00Z"
        })),
    )
    .await?;
    let activity_id = string_field(&activity, "id").to_owned();

    activities::update(
        &pool,
        &args(json!({
            "id": activity_id,
            "kind": "meeting",
            "title": "Discovery meeting",
            "body": "Expanded scope discussion",
            "occurred_at": "2026-04-24T10:00:00Z"
        })),
    )
    .await?;

    // Relative close date: a fixed date rots once real time passes it and the
    // closing_soon_count assertion below silently starts failing.
    let expected_close = (chrono::Utc::now() + chrono::Duration::days(30))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let converted = leads::convert_to_deal(
        &pool,
        &args(json!({
            "lead_id": lead_id,
            "amount": 22000.0,
            "currency": "USD",
            "stage": "Qualified",
            "probability": 60,
            "expected_close": expected_close
        })),
    )
    .await?;
    let deal_id = string_field(&converted, "deal_id").to_owned();

    let search = views::search_all(
        &pool,
        &args(json!({ "query": "Northwind", "limit_per_type": 5 })),
    )
    .await?;
    assert_eq!(search["counts"]["organizations"], json!(1));
    assert_eq!(search["counts"]["leads"], json!(1));
    assert_eq!(search["counts"]["deals"], json!(1));

    let lead_overview = views::record_overview(
        &pool,
        &args(json!({
            "entity_type": "lead",
            "id": lead_id,
            "related_limit": 5,
            "activity_limit": 5
        })),
    )
    .await?;
    assert_eq!(lead_overview["summary"]["deal_count"], json!(1));
    assert_eq!(
        lead_overview["recent_activities"].as_array().unwrap().len(),
        1
    );

    let deal_overview = views::record_overview(
        &pool,
        &args(json!({
            "entity_type": "deal",
            "id": deal_id
        })),
    )
    .await?;
    assert_eq!(deal_overview["linked"]["lead"]["id"], json!(lead_id));

    let dashboard = views::dashboard_summary(
        &pool,
        &args(json!({
            "stale_days": 365,
            "closing_within_days": 60,
            "activity_window_days": 30,
            "list_limit": 5
        })),
    )
    .await?;
    assert_eq!(dashboard["totals"]["organizations"], json!(1));
    assert_eq!(dashboard["totals"]["leads"], json!(1));
    assert_eq!(dashboard["totals"]["deals"], json!(1));
    assert_eq!(dashboard["pipeline"]["closing_soon_count"], json!(1));

    Ok(())
}

#[tokio::test]
async fn archive_restore_and_export_protect_active_views() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let lead = leads::create(&pool, &args(json!({ "name": "Archive Target" }))).await?;
    let lead_id = string_field(&lead, "id").to_owned();
    let deal = deals::create(
        &pool,
        &args(json!({
            "title": "Archive Me",
            "contact_id": lead_id,
            "amount": 9000.0,
            "currency": "USD"
        })),
    )
    .await?;
    let deal_id = string_field(&deal, "id").to_owned();

    let confirm_err = records::require_confirmed_delete(&args(json!({ "id": deal_id })))
        .await
        .expect_err("expected delete confirmation to be required");
    assert!(confirm_err.to_string().contains("confirm_permanent"));

    records::archive(
        &pool,
        &args(json!({
            "entity_type": "deal",
            "id": deal_id
        })),
    )
    .await?;

    let deal_search = views::search_all(&pool, &args(json!({ "query": "Archive Me" }))).await?;
    assert_eq!(deal_search["counts"]["deals"], json!(0));

    let archived = records::archived_list(&pool, &args(json!({ "entity_type": "deal" }))).await?;
    assert_eq!(archived["archived_records"].as_array().unwrap().len(), 1);

    let snapshot =
        records::export_snapshot(&pool, &args(json!({ "include_archived": true }))).await?;
    let exported_deal = snapshot["deals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == json!(deal_id))
        .expect("expected archived deal in snapshot");
    assert!(exported_deal["deleted_at"].is_string());

    records::restore(
        &pool,
        &args(json!({
            "entity_type": "deal",
            "id": deal_id
        })),
    )
    .await?;

    let deal_search_restored =
        views::search_all(&pool, &args(json!({ "query": "Archive Me" }))).await?;
    assert_eq!(deal_search_restored["counts"]["deals"], json!(1));

    Ok(())
}

#[tokio::test]
async fn mixed_currency_totals_are_grouped_not_added() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let lead = leads::create(&pool, &args(json!({ "name": "Multi Currency" }))).await?;
    let lead_id = string_field(&lead, "id").to_owned();

    for (amount, currency, stage) in [
        (1000.0, "USD", "Prospecting"),
        (500.0, "EUR", "Prospecting"),
        (250.5, "EUR", "Won"),
    ] {
        deals::create(
            &pool,
            &args(json!({
                "title": format!("{currency} {stage} deal"),
                "contact_id": lead_id,
                "amount": amount,
                "currency": currency,
                "stage": stage
            })),
        )
        .await?;
    }

    let listed = deals::list(&pool, &args(json!({}))).await?;
    assert_eq!(
        listed["total_value"],
        json!({ "USD": 1000.0, "EUR": 750.5 })
    );

    let pipeline = deals::pipeline_summary(&pool).await?;
    assert_eq!(
        pipeline["total_value"],
        json!({ "USD": 1000.0, "EUR": 750.5 })
    );
    assert_eq!(pipeline["won_value"], json!({ "EUR": 250.5 }));
    let prospecting = &pipeline["pipeline"][0];
    assert_eq!(prospecting["stage"], json!("Prospecting"));
    assert_eq!(prospecting["count"], json!(2));
    assert_eq!(
        prospecting["total_value"],
        json!({ "USD": 1000.0, "EUR": 500.0 })
    );

    let dashboard = views::dashboard_summary(&pool, &args(json!({}))).await?;
    assert_eq!(
        dashboard["pipeline"]["active_pipeline_value"],
        json!({ "USD": 1000.0, "EUR": 500.0 })
    );

    Ok(())
}

#[tokio::test]
async fn cents_prevent_float_drift_in_sums() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let lead = leads::create(&pool, &args(json!({ "name": "Dime Collector" }))).await?;
    let lead_id = string_field(&lead, "id").to_owned();

    for i in 0..3 {
        deals::create(
            &pool,
            &args(json!({
                "title": format!("Dime {i}"),
                "contact_id": lead_id,
                "amount": 0.1,
                "currency": "USD"
            })),
        )
        .await?;
    }

    // 3 × $0.10 must be exactly $0.30 — REAL summation gave 0.30000000000000004.
    let listed = deals::list(&pool, &args(json!({}))).await?;
    assert_eq!(listed["total_value"], json!({ "USD": 0.3 }));

    Ok(())
}

#[tokio::test]
async fn legacy_real_db_migrates_to_cents_and_utc() -> Result<()> {
    let dir = TestDir::new();
    let db_path = dir.path().join("crm.db");

    // A database exactly as the pre-versioning code left it: REAL amounts,
    // verbatim non-UTC timestamps, user_version 0.
    let legacy = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::raw_sql(
        "CREATE TABLE orgs (
            id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, website TEXT, industry TEXT,
            size TEXT, country TEXT, phone TEXT, email TEXT, tags TEXT NOT NULL DEFAULT '[]',
            notes TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
        );
        CREATE TABLE leads (
            id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, email TEXT, phone TEXT,
            company TEXT, org_id TEXT REFERENCES orgs(id) ON DELETE SET NULL,
            status TEXT NOT NULL CHECK(status IN ('Open', 'Contacted', 'Qualified', 'Lost')),
            source TEXT, tags TEXT NOT NULL DEFAULT '[]', notes TEXT,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
        );
        CREATE TABLE deals (
            id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL,
            amount REAL NOT NULL DEFAULT 0 CHECK(amount >= 0),
            currency TEXT NOT NULL DEFAULT 'USD',
            stage TEXT NOT NULL CHECK(stage IN ('Prospecting', 'Qualified', 'Proposal', 'Negotiation', 'Won', 'Lost')),
            probability INTEGER,
            contact_id TEXT NOT NULL REFERENCES leads(id) ON DELETE RESTRICT,
            org_id TEXT REFERENCES orgs(id) ON DELETE SET NULL,
            expected_close TEXT, tags TEXT NOT NULL DEFAULT '[]', notes TEXT,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
        );
        CREATE TABLE activities (
            id TEXT PRIMARY KEY NOT NULL, entity_id TEXT NOT NULL,
            entity_type TEXT NOT NULL, kind TEXT NOT NULL, title TEXT NOT NULL, body TEXT,
            occurred_at TEXT NOT NULL, created_at TEXT NOT NULL, deleted_at TEXT
        );
        INSERT INTO leads (id, name, status, created_at, updated_at)
            VALUES ('lead-legacy', 'Legacy Lead', 'Open', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00');
        INSERT INTO deals (id, title, amount, currency, stage, contact_id, expected_close, created_at, updated_at)
            VALUES ('deal-legacy', 'Legacy Deal', 123.45, 'USD', 'Prospecting', 'lead-legacy',
                    '2027-01-02T10:00:00+10:00', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00');
        INSERT INTO activities (id, entity_id, entity_type, kind, title, occurred_at, created_at)
            VALUES ('act-legacy', 'deal-legacy', 'deal', 'note', 'Legacy note',
                    '2026-01-02T10:00:00+10:00', '2026-01-01T00:00:00+00:00');",
    )
    .execute(&legacy)
    .await?;
    legacy.close().await;

    let pool = db::open(dir.path()).await?;

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await?;
    assert_eq!(version, 4, "all migrations should be recorded");

    let deal = deals::get(&pool, "deal-legacy").await?;
    assert_eq!(deal["amount"], json!(123.45));
    assert_eq!(deal["amount_minor"], json!(12345));
    assert_eq!(deal["expected_close"], json!("2027-01-02T00:00:00.000Z"));

    let activity = activities::get(&pool, "act-legacy").await?;
    assert_eq!(activity["occurred_at"], json!("2026-01-02T00:00:00.000Z"));

    // Reopening must be a clean no-op.
    let reopened = db::open(dir.path()).await?;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&reopened)
        .await?;
    assert_eq!(version, 4);

    Ok(())
}

#[tokio::test]
async fn non_utc_offsets_compare_correctly_in_views() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let lead = leads::create(&pool, &args(json!({ "name": "Offset Prospect" }))).await?;
    let lead_id = string_field(&lead, "id").to_owned();

    // Two hours in the past, written with a +10:00 offset — the verbatim
    // string sorts AFTER "now" in UTC, which made this deal invisible to the
    // overdue check before normalization.
    let past = (Utc::now() - Duration::hours(2))
        .with_timezone(&FixedOffset::east_opt(10 * 3600).unwrap())
        .to_rfc3339();
    let future = (Utc::now() + Duration::days(3))
        .with_timezone(&FixedOffset::west_opt(7 * 3600).unwrap())
        .to_rfc3339();

    let overdue = deals::create(
        &pool,
        &args(json!({
            "title": "Overdue in Brisbane",
            "contact_id": lead_id,
            "amount": 10.0,
            "expected_close": past
        })),
    )
    .await?;
    deals::create(
        &pool,
        &args(json!({
            "title": "Closing soon in Denver",
            "contact_id": lead_id,
            "amount": 20.0,
            "expected_close": future
        })),
    )
    .await?;

    // Stored value is rewritten as fixed-format UTC.
    let stored = deals::get(&pool, string_field(&overdue, "id")).await?;
    let expected_utc =
        crate::utils::format_utc(chrono::DateTime::parse_from_rfc3339(&past)?.with_timezone(&Utc));
    assert_eq!(stored["expected_close"], json!(expected_utc));

    let dashboard =
        views::dashboard_summary(&pool, &args(json!({ "closing_within_days": 30 }))).await?;
    assert_eq!(dashboard["pipeline"]["overdue_deals_count"], json!(1));
    assert_eq!(dashboard["pipeline"]["closing_soon_count"], json!(1));

    Ok(())
}

#[tokio::test]
async fn deleting_lead_with_archived_deals_teaches_instead_of_fk_error() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let lead = leads::create(&pool, &args(json!({ "name": "Archived Refs" }))).await?;
    let lead_id = string_field(&lead, "id").to_owned();
    let deal = deals::create(
        &pool,
        &args(json!({
            "title": "Old Deal",
            "contact_id": lead_id,
            "amount": 100.0
        })),
    )
    .await?;

    records::archive(
        &pool,
        &args(json!({ "entity_type": "deal", "id": string_field(&deal, "id") })),
    )
    .await?;

    let err = leads::delete(&pool, &lead_id)
        .await
        .expect_err("expected archived-deal guard");
    assert!(err.to_string().contains("archived deal(s)"), "got: {err}");

    Ok(())
}

#[tokio::test]
async fn length_caps_reject_oversized_fields() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let notes_err = leads::create(
        &pool,
        &args(json!({ "name": "Cap Test", "notes": "x".repeat(64 * 1024 + 1) })),
    )
    .await
    .expect_err("expected notes cap");
    assert!(
        notes_err.to_string().contains("too long"),
        "got: {notes_err}"
    );

    let name_err = leads::create(&pool, &args(json!({ "name": "n".repeat(501) })))
        .await
        .expect_err("expected name cap");
    assert!(name_err.to_string().contains("too long"), "got: {name_err}");

    let too_many_tags: Vec<String> = (0..51).map(|i| format!("tag-{i}")).collect();
    let tags_err = orgs::create(
        &pool,
        &args(json!({ "name": "Tag Overflow", "tags": too_many_tags })),
    )
    .await
    .expect_err("expected tag count cap");
    assert!(
        tags_err.to_string().contains("at most 50"),
        "got: {tags_err}"
    );

    let long_tag_err = orgs::create(
        &pool,
        &args(json!({ "name": "Tag Length", "tags": ["t".repeat(101)] })),
    )
    .await
    .expect_err("expected tag length cap");
    assert!(
        long_tag_err.to_string().contains("at most 100"),
        "got: {long_tag_err}"
    );

    Ok(())
}

#[tokio::test]
async fn duplicate_guards_teach_and_allow_override() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let first = leads::create(
        &pool,
        &args(json!({ "name": "Dana First", "email": "dana@dup.test" })),
    )
    .await?;

    // Case-insensitive email match, and the teaching error carries the id.
    let dup_err = leads::create(
        &pool,
        &args(json!({ "name": "Dana Second", "email": "DANA@dup.test" })),
    )
    .await
    .expect_err("expected duplicate-email guard");
    assert!(
        dup_err.to_string().contains(string_field(&first, "id")),
        "got: {dup_err}"
    );

    leads::create(
        &pool,
        &args(json!({ "name": "Dana Second", "email": "dana@dup.test", "allow_duplicate": true })),
    )
    .await?;

    let org = orgs::create(&pool, &args(json!({ "name": "Dup Org" }))).await?;
    let org_err = orgs::create(&pool, &args(json!({ "name": "dup org" })))
        .await
        .expect_err("expected duplicate-name guard");
    assert!(
        org_err.to_string().contains(string_field(&org, "id")),
        "got: {org_err}"
    );

    let second = orgs::create(
        &pool,
        &args(json!({ "name": "Dup Org", "allow_duplicate": true })),
    )
    .await?;
    assert!(
        second.get("warning").is_some(),
        "allowed duplicate should carry a warning"
    );

    Ok(())
}

#[tokio::test]
async fn export_to_file_and_backup_land_in_files_dir() -> Result<()> {
    let (_dir, pool) = test_pool().await?;
    let files_base = TestDir::new();
    // Only this test reads AXON_DATA_DIR in this binary (db::open takes an
    // explicit dir), so the process-global env var is safe to set here.
    std::env::set_var("AXON_DATA_DIR", files_base.path());

    leads::create(&pool, &args(json!({ "name": "Solo Lead" }))).await?;

    // Small dataset → inline by default.
    let inline = records::export_snapshot(&pool, &args(json!({}))).await?;
    assert!(inline.get("leads").is_some(), "small export should inline");

    // Explicit to_file → JSON lands under <AXON_DATA_DIR>/files, result is slim.
    let filed = records::export_snapshot(&pool, &args(json!({ "to_file": true }))).await?;
    assert!(
        filed.get("leads").is_none(),
        "file export must not inline records"
    );
    let path = PathBuf::from(string_field(&filed, "file"));
    assert!(path.starts_with(files_base.path().join("files")));
    let content: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
    assert_eq!(content["counts"]["leads"], json!(1));

    // Over the inline cap → defaults to file without being asked.
    for i in 0..205 {
        leads::create(&pool, &args(json!({ "name": format!("Bulk {i}") }))).await?;
    }
    let auto = records::export_snapshot(&pool, &args(json!({}))).await?;
    assert!(
        auto.get("file").is_some(),
        ">200 records should default to a file export"
    );

    // Backup is a real, openable SQLite database.
    let backup = records::backup_db(&pool).await?;
    let backup_path = PathBuf::from(string_field(&backup, "file"));
    let bpool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(&backup_path))
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leads")
        .fetch_one(&bpool)
        .await?;
    assert_eq!(count, 206);
    bpool.close().await;

    std::env::remove_var("AXON_DATA_DIR");
    Ok(())
}

#[tokio::test]
async fn changes_since_tracks_creates_updates_and_cursor() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    // Records that exist BEFORE the cursor must not appear.
    let old_lead = leads::create(&pool, &args(json!({ "name": "Pre-cursor Lead" }))).await?;
    let old_lead_id = string_field(&old_lead, "id").to_owned();

    // Cursor written with a non-UTC offset — must still compare correctly
    // against the stored to_rfc3339 timestamps.
    let cursor = Utc::now()
        .with_timezone(&FixedOffset::east_opt(10 * 3600).unwrap())
        .to_rfc3339();
    // to_rfc3339 keeps sub-millisecond precision but the feed compares at
    // milliseconds; wait one tick so post-cursor writes land after it.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let new_lead = leads::create(&pool, &args(json!({ "name": "Post-cursor Lead" }))).await?;
    let new_lead_id = string_field(&new_lead, "id").to_owned();
    let deal = deals::create(
        &pool,
        &args(json!({
            "title": "Fresh Deal",
            "contact_id": new_lead_id,
            "amount": 42.0
        })),
    )
    .await?;
    let deal_id = string_field(&deal, "id").to_owned();

    // An UPDATE to a pre-cursor record surfaces as change: "updated".
    leads::update(
        &pool,
        &args(json!({ "id": old_lead_id, "status": "Contacted" })),
    )
    .await?;

    let feed = views::changes_since(&pool, &args(json!({ "since": cursor }))).await?;
    assert_eq!(feed["count"], json!(3), "got: {feed}");
    assert_eq!(feed["has_more"], json!(false));

    let changes = feed["changes"].as_array().unwrap();
    let find = |id: &str| {
        changes
            .iter()
            .find(|c| c["id"] == json!(id))
            .unwrap_or_else(|| panic!("missing change for {id}"))
    };
    assert_eq!(find(&new_lead_id)["change"], json!("created"));
    assert_eq!(find(&new_lead_id)["entity_type"], json!("lead"));
    assert_eq!(find(&deal_id)["change"], json!("created"));
    assert_eq!(find(&deal_id)["amount"], json!(42.0));
    assert_eq!(find(&old_lead_id)["change"], json!("updated"));
    assert_eq!(find(&old_lead_id)["status"], json!("Contacted"));

    // The returned cursor is exclusive: replaying it yields nothing new.
    let cursor2 = string_field(&feed, "cursor").to_owned();
    let replay = views::changes_since(&pool, &args(json!({ "since": cursor2 }))).await?;
    assert_eq!(replay["count"], json!(0), "got: {replay}");
    assert_eq!(replay["cursor"], json!(cursor2), "empty feed echoes cursor");

    // Entity filter: deals only.
    let deals_only = views::changes_since(
        &pool,
        &args(json!({ "since": cursor, "entity_types": ["deal"] })),
    )
    .await?;
    assert_eq!(deals_only["count"], json!(1));
    assert_eq!(deals_only["changes"][0]["id"], json!(deal_id));

    // A stage change shows up as "updated" with the new stage, and archived
    // records drop out of the feed entirely.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    deals::update(&pool, &args(json!({ "id": deal_id, "stage": "Qualified" }))).await?;
    let after_stage = views::changes_since(
        &pool,
        &args(json!({ "since": cursor2, "entity_types": ["deal"] })),
    )
    .await?;
    assert_eq!(after_stage["count"], json!(1));
    assert_eq!(after_stage["changes"][0]["change"], json!("updated"));
    assert_eq!(after_stage["changes"][0]["stage"], json!("Qualified"));

    records::archive(
        &pool,
        &args(json!({ "entity_type": "deal", "id": deal_id })),
    )
    .await?;
    let after_archive = views::changes_since(
        &pool,
        &args(json!({ "since": cursor2, "entity_types": ["deal"] })),
    )
    .await?;
    assert_eq!(
        after_archive["count"],
        json!(0),
        "archived records drop out"
    );

    // limit + has_more: window cut mid-feed, cursor resumes it.
    let limited =
        views::changes_since(&pool, &args(json!({ "since": cursor, "limit": 1 }))).await?;
    assert_eq!(limited["count"], json!(1));
    assert_eq!(limited["has_more"], json!(true));

    Ok(())
}

#[tokio::test]
async fn phone_is_searchable_and_guards_duplicates() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let lead = leads::create(
        &pool,
        &args(json!({ "name": "Engr. Ramos", "phone": "0917-555-1234" })),
    )
    .await?;
    let lead_id = string_field(&lead, "id").to_owned();

    // Separator-insensitive search: spaces in the query, dashes in the record.
    let by_phone = leads::search(&pool, &args(json!({ "query": "0917 555 1234" }))).await?;
    assert_eq!(by_phone["total"], json!(1), "got: {by_phone}");
    let partial = leads::search(&pool, &args(json!({ "query": "555-1234" }))).await?;
    assert_eq!(partial["total"], json!(1), "got: {partial}");

    // A lead's own values never collide with itself.
    leads::update(
        &pool,
        &args(json!({ "id": lead_id, "phone": "0917-555-1234" })),
    )
    .await?;

    // Same phone with different separators is a duplicate; the teaching error
    // carries the existing id.
    let dup_err = leads::create(
        &pool,
        &args(json!({ "name": "J. Ramos", "phone": "(0917) 555 1234" })),
    )
    .await
    .expect_err("expected duplicate-phone guard");
    assert!(dup_err.to_string().contains(&lead_id), "got: {dup_err}");

    leads::create(
        &pool,
        &args(json!({ "name": "J. Ramos", "phone": "0917 555 1234", "allow_duplicate": true })),
    )
    .await?;

    // Update guard: pointing another lead's phone/email at an existing one is
    // rejected unless allowed — and updates NOT touching those fields pass.
    let other = leads::create(
        &pool,
        &args(json!({ "name": "Other Buyer", "email": "other@buyer.test" })),
    )
    .await?;
    let other_id = string_field(&other, "id").to_owned();
    let update_err = leads::update(
        &pool,
        &args(json!({ "id": other_id, "phone": "0917.555.1234" })),
    )
    .await
    .expect_err("expected duplicate-phone guard on update");
    assert!(update_err.to_string().contains(&lead_id));
    leads::update(
        &pool,
        &args(json!({ "id": other_id, "status": "Contacted" })),
    )
    .await?;
    leads::update(
        &pool,
        &args(json!({ "id": other_id, "phone": "0917-555-1234", "allow_duplicate": true })),
    )
    .await?;

    // Org search also matches phone and email now.
    orgs::create(
        &pool,
        &args(json!({ "name": "Cavite Roadworks", "phone": "046-123-4567", "email": "buy@cavite.test" })),
    )
    .await?;
    let org_by_phone = orgs::search(&pool, &args(json!({ "query": "046 123" }))).await?;
    assert_eq!(org_by_phone["total"], json!(1), "got: {org_by_phone}");
    let org_by_email = orgs::search(&pool, &args(json!({ "query": "buy@cavite" }))).await?;
    assert_eq!(org_by_email["total"], json!(1));

    let all = views::search_all(&pool, &args(json!({ "query": "555 1234" }))).await?;
    assert!(
        all["counts"]["leads"].as_i64().unwrap() >= 1,
        "search_all should match phones, got: {all}"
    );

    // Whitespace inside an email is rejected.
    let email_err = leads::create(
        &pool,
        &args(json!({ "name": "Bad Email", "email": "bad email@x.test" })),
    )
    .await
    .expect_err("expected whitespace-email rejection");
    assert!(email_err.to_string().contains("valid email"));

    Ok(())
}

#[tokio::test]
async fn deal_and_conversion_duplicate_guards() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let lead = leads::create(&pool, &args(json!({ "name": "Retry Victim" }))).await?;
    let lead_id = string_field(&lead, "id").to_owned();

    let deal = deals::create(
        &pool,
        &args(json!({ "title": "Culverts 300pcs", "contact_id": lead_id, "amount": 870000.0 })),
    )
    .await?;
    let deal_id = string_field(&deal, "id").to_owned();

    // A retry with the same title (case-insensitive) teaches instead of duping.
    let dup_err = deals::create(
        &pool,
        &args(json!({ "title": "culverts 300PCS", "contact_id": lead_id, "amount": 870000.0 })),
    )
    .await
    .expect_err("expected duplicate-deal guard");
    assert!(dup_err.to_string().contains(&deal_id), "got: {dup_err}");

    // A genuinely different opportunity for the same contact is fine.
    deals::create(
        &pool,
        &args(json!({ "title": "Aggregates delivery", "contact_id": lead_id })),
    )
    .await?;
    deals::create(
        &pool,
        &args(
            json!({ "title": "Culverts 300pcs", "contact_id": lead_id, "allow_duplicate": true }),
        ),
    )
    .await?;

    // Converting twice with the same (default) title is the classic agent
    // retry — second call gets the existing deal id back.
    let lead2 = leads::create(&pool, &args(json!({ "name": "Convert Twice" }))).await?;
    let lead2_id = string_field(&lead2, "id").to_owned();
    let converted = leads::convert_to_deal(&pool, &args(json!({ "lead_id": lead2_id }))).await?;
    let convert_err = leads::convert_to_deal(&pool, &args(json!({ "lead_id": lead2_id })))
        .await
        .expect_err("expected convert idempotency guard");
    assert!(
        convert_err
            .to_string()
            .contains(string_field(&converted, "deal_id")),
        "got: {convert_err}"
    );
    leads::convert_to_deal(
        &pool,
        &args(json!({ "lead_id": lead2_id, "allow_duplicate": true })),
    )
    .await?;

    // Org rename guard: colliding rename teaches, allow_duplicate overrides,
    // and renaming an org to its own name is never a collision.
    let org_a = orgs::create(&pool, &args(json!({ "name": "Alpha Gravel" }))).await?;
    let org_b = orgs::create(&pool, &args(json!({ "name": "Beta Gravel" }))).await?;
    let rename_err = orgs::update(
        &pool,
        &args(json!({ "id": string_field(&org_b, "id"), "name": "alpha gravel" })),
    )
    .await
    .expect_err("expected org rename guard");
    assert!(rename_err.to_string().contains(string_field(&org_a, "id")));
    orgs::update(
        &pool,
        &args(json!({ "id": string_field(&org_a, "id"), "name": "Alpha Gravel", "notes": "self-rename ok" })),
    )
    .await?;
    orgs::update(
        &pool,
        &args(json!({ "id": string_field(&org_b, "id"), "name": "Alpha Gravel", "allow_duplicate": true })),
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn date_only_inputs_normalize_to_midnight_utc() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let lead = leads::create(&pool, &args(json!({ "name": "Date Fan" }))).await?;
    let lead_id = string_field(&lead, "id").to_owned();

    let deal = deals::create(
        &pool,
        &args(json!({
            "title": "Plain-date close",
            "contact_id": lead_id,
            "expected_close": "2027-07-15"
        })),
    )
    .await?;
    let stored = deals::get(&pool, string_field(&deal, "id")).await?;
    assert_eq!(stored["expected_close"], json!("2027-07-15T00:00:00.000Z"));

    let activity = activities::log(
        &pool,
        &args(json!({
            "entity_id": lead_id,
            "entity_type": "lead",
            "kind": "task",
            "title": "Follow up quote",
            "due_at": "2027-07-20"
        })),
    )
    .await?;
    let stored_activity = activities::get(&pool, string_field(&activity, "id")).await?;
    assert_eq!(stored_activity["due_at"], json!("2027-07-20T00:00:00.000Z"));

    let garbage_err = deals::update(
        &pool,
        &args(json!({ "id": string_field(&deal, "id"), "expected_close": "next tuesday" })),
    )
    .await
    .expect_err("expected date parse error");
    assert!(garbage_err.to_string().contains("YYYY-MM-DD"));

    Ok(())
}

#[tokio::test]
async fn tasks_due_worklist_and_dashboard_counts() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let lead = leads::create(&pool, &args(json!({ "name": "Task Magnet" }))).await?;
    let lead_id = string_field(&lead, "id").to_owned();

    let stamp = |days: i64| (Utc::now() + Duration::days(days)).to_rfc3339();
    let overdue_task = activities::log(
        &pool,
        &args(json!({
            "entity_id": lead_id, "entity_type": "lead", "kind": "task",
            "title": "Chase unpaid quote", "due_at": stamp(-2)
        })),
    )
    .await?;
    activities::log(
        &pool,
        &args(json!({
            "entity_id": lead_id, "entity_type": "lead", "kind": "task",
            "title": "Send delivery schedule", "due_at": stamp(3)
        })),
    )
    .await?;
    activities::log(
        &pool,
        &args(json!({
            "entity_id": lead_id, "entity_type": "lead", "kind": "task",
            "title": "Far-future check-in", "due_at": stamp(30)
        })),
    )
    .await?;
    activities::log(
        &pool,
        &args(json!({
            "entity_id": lead_id, "entity_type": "lead", "kind": "task",
            "title": "Already handled", "due_at": stamp(-5), "done": true
        })),
    )
    .await?;
    activities::log(
        &pool,
        &args(json!({
            "entity_id": lead_id, "entity_type": "lead", "kind": "task",
            "title": "Someday follow-up"
        })),
    )
    .await?;
    // Non-task kinds never enter the worklist.
    activities::log(
        &pool,
        &args(json!({
            "entity_id": lead_id, "entity_type": "lead", "kind": "note",
            "title": "Just a note", "due_at": stamp(1)
        })),
    )
    .await?;

    // Default window (7 days): overdue + due-in-3, oldest due first, labeled.
    let due = activities::tasks_due(&pool, &args(json!({}))).await?;
    assert_eq!(due["count"], json!(2), "got: {due}");
    assert_eq!(due["overdue_count"], json!(1));
    let tasks = due["tasks"].as_array().unwrap();
    assert_eq!(tasks[0]["title"], json!("Chase unpaid quote"));
    assert_eq!(tasks[0]["overdue"], json!(true));
    assert_eq!(tasks[0]["entity_label"], json!("Task Magnet"));
    assert_eq!(tasks[1]["overdue"], json!(false));

    // Widen the window and pull in undated open tasks.
    let wide = activities::tasks_due(
        &pool,
        &args(json!({ "due_within_days": 60, "include_undated": true })),
    )
    .await?;
    assert_eq!(wide["count"], json!(4), "got: {wide}");

    // Completing a task removes it from the worklist; done filter sees it.
    activities::update(
        &pool,
        &args(json!({ "id": string_field(&overdue_task, "id"), "done": true })),
    )
    .await?;
    let after_done = activities::tasks_due(&pool, &args(json!({}))).await?;
    assert_eq!(after_done["count"], json!(1));
    let done_list = activities::list(&pool, &args(json!({ "kind": "task", "done": true }))).await?;
    assert_eq!(done_list["total"], json!(2));

    let dashboard = views::dashboard_summary(&pool, &args(json!({}))).await?;
    assert_eq!(dashboard["tasks"]["open"], json!(3), "got: {dashboard}");
    assert_eq!(dashboard["tasks"]["overdue"], json!(0));

    Ok(())
}

#[tokio::test]
async fn changes_since_cursor_never_skips_same_millisecond_rows() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    // Three leads sharing one exact timestamp — a bulk import. The old
    // timestamp-only cursor lost whichever rows the limit cut off.
    let ts = "2026-06-01T08:00:00.000+00:00";
    for i in 0..3 {
        sqlx::query(
            "INSERT INTO leads (id, name, status, tags, created_at, updated_at)
             VALUES (?, ?, 'Open', '[]', ?, ?)",
        )
        .bind(format!("bulk-{i}"))
        .bind(format!("Bulk Lead {i}"))
        .bind(ts)
        .bind(ts)
        .execute(&pool)
        .await?;
    }

    let first = views::changes_since(
        &pool,
        &args(json!({ "since": "2026-06-01T00:00:00Z", "limit": 2 })),
    )
    .await?;
    assert_eq!(first["count"], json!(2), "got: {first}");
    assert_eq!(first["has_more"], json!(true));
    let cursor = string_field(&first, "cursor").to_owned();
    assert!(
        cursor.contains('|'),
        "cursor should carry an id tie-breaker"
    );

    let second = views::changes_since(&pool, &args(json!({ "since": cursor }))).await?;
    assert_eq!(
        second["count"],
        json!(1),
        "the same-millisecond leftover must surface: {second}"
    );
    assert_eq!(second["changes"][0]["id"], json!("bulk-2"));

    // And the composite cursor is exclusive on replay.
    let cursor2 = string_field(&second, "cursor").to_owned();
    let replay = views::changes_since(&pool, &args(json!({ "since": cursor2 }))).await?;
    assert_eq!(replay["count"], json!(0));
    assert_eq!(replay["cursor"], json!(cursor2));

    Ok(())
}

#[tokio::test]
async fn org_overview_rolls_up_child_activity() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    let org = orgs::create(&pool, &args(json!({ "name": "Rollup Builders" }))).await?;
    let org_id = string_field(&org, "id").to_owned();
    let lead = leads::create(
        &pool,
        &args(json!({ "name": "Site Foreman", "org_id": org_id })),
    )
    .await?;
    let lead_id = string_field(&lead, "id").to_owned();
    let deal = deals::create(
        &pool,
        &args(json!({ "title": "Roadbase order", "contact_id": lead_id, "org_id": org_id })),
    )
    .await?;

    for (entity_id, entity_type, title) in [
        (org_id.as_str(), "org", "Org-level note"),
        (lead_id.as_str(), "lead", "Call with foreman"),
        (string_field(&deal, "id"), "deal", "Quote sent"),
    ] {
        activities::log(
            &pool,
            &args(json!({
                "entity_id": entity_id, "entity_type": entity_type, "title": title
            })),
        )
        .await?;
    }

    let overview =
        views::record_overview(&pool, &args(json!({ "entity_type": "org", "id": org_id }))).await?;
    let rolled = overview["recent_activities"].as_array().unwrap();
    assert_eq!(
        rolled.len(),
        3,
        "org + lead + deal activity, got: {overview}"
    );
    assert_eq!(overview["summary"]["activity_count"], json!(3));

    Ok(())
}

#[tokio::test]
async fn default_currency_provider_applies_to_new_deals() -> Result<()> {
    let (_dir, pool) = test_pool().await?;

    // Process-global OnceLock: first set wins for the whole test binary. No
    // other test asserts the currency of a default-created deal, and the
    // trim+uppercase normalization is exercised on the way through.
    crate::set_default_currency_provider(|| " php ".to_owned());

    let lead = leads::create(&pool, &args(json!({ "name": "Peso Payer" }))).await?;
    let deal = deals::create(
        &pool,
        &args(json!({ "title": "Sand and gravel", "contact_id": string_field(&lead, "id") })),
    )
    .await?;
    let stored = deals::get(&pool, string_field(&deal, "id")).await?;
    assert_eq!(stored["currency"], json!("PHP"));

    // An explicit currency still wins.
    let usd = deals::create(
        &pool,
        &args(json!({
            "title": "Export order",
            "contact_id": string_field(&lead, "id"),
            "currency": "USD"
        })),
    )
    .await?;
    assert_eq!(
        deals::get(&pool, string_field(&usd, "id")).await?["currency"],
        json!("USD")
    );

    Ok(())
}

#[tokio::test]
async fn concurrent_pools_write_without_sqlite_busy() -> Result<()> {
    let (dir, pool_a) = test_pool().await?;
    let pool_b = db::open(dir.path()).await?;

    let writer = |pool: SqlitePool, label: &'static str| async move {
        for i in 0..10 {
            leads::create(&pool, &args(json!({ "name": format!("{label}-{i}") }))).await?;
        }
        Ok::<_, anyhow::Error>(())
    };

    let (a, b) = tokio::join!(writer(pool_a.clone(), "a"), writer(pool_b, "b"));
    a?;
    b?;

    let listed = leads::list(&pool_a, &args(json!({ "limit": 200 }))).await?;
    assert_eq!(listed["total"], json!(20));

    Ok(())
}
