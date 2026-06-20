use crate::{activities, db, deals, leads, orgs, records, views};
use anyhow::Result;
use serde_json::{json, Map, Value};
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
    assert_eq!(pipeline["total_value"], json!(12500.0));

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

    let converted = leads::convert_to_deal(
        &pool,
        &args(json!({
            "lead_id": lead_id,
            "amount": 22000.0,
            "currency": "USD",
            "stage": "Qualified",
            "probability": 60,
            "expected_close": "2026-05-20T12:00:00Z"
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
