//! Database bootstrap: versioned schema migrations + idempotent seeding.
//!
//! Replaces the previous approach of running dozens of ad-hoc
//! `let _ = conn.execute("ALTER TABLE ...")` statements with their errors
//! swallowed. Now:
//!   * schema changes are ordered, versioned, and recorded in `schema_migrations`
//!     (each migration runs exactly once);
//!   * real errors abort startup with context instead of being ignored — the
//!     only tolerated failure is "duplicate column name" on the explicitly
//!     additive migration, which is expected on freshly-created databases;
//!   * default rows (`seed.sql`) and one-time value fixes (`normalize.sql`) are
//!     separated from schema and clearly idempotent.
//!
//! Migration SQL is embedded in the binary via `include_str!`, so a deployment
//! no longer depends on shipping `memory/schema.sql` next to the executable.

use anyhow::{Context, Result};
use rusqlite::Connection;

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
    /// When true, "duplicate column name" errors are ignored (the migration is
    /// purely additive `ALTER TABLE ADD COLUMN` and the column already exists on
    /// databases created from the current base schema). All other errors abort.
    tolerant_dup_column: bool,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "base_schema",
        sql: include_str!("migrations/0001_base_schema.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 2,
        name: "agent_tables",
        sql: include_str!("migrations/0002_agent_tables.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 3,
        name: "column_additions",
        sql: include_str!("migrations/0003_column_additions.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 4,
        name: "telegram_reply_routes",
        sql: include_str!("migrations/0004_telegram_reply_routes.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 5,
        name: "durable_wait",
        sql: include_str!("migrations/0005_durable_wait.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 6,
        name: "facebook_webhook_page",
        sql: include_str!("migrations/0006_facebook_webhook_page.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 7,
        name: "node_reliability",
        sql: include_str!("migrations/0007_node_reliability.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 8,
        name: "workflow_updated_at",
        sql: include_str!("migrations/0008_workflow_updated_at.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 9,
        name: "error_workflow_and_pins",
        sql: include_str!("migrations/0009_error_workflow_and_pins.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 10,
        name: "workflow_versions",
        sql: include_str!("migrations/0010_workflow_versions.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 11,
        name: "resume_tokens",
        sql: include_str!("migrations/0011_resume_tokens.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 12,
        name: "trigger_dedup",
        sql: include_str!("migrations/0012_trigger_dedup.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 13,
        name: "drop_resume_tokens",
        sql: include_str!("migrations/0013_drop_resume_tokens.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 14,
        name: "conversations",
        sql: include_str!("migrations/0014_conversations.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 15,
        name: "model_thinking_mode",
        sql: include_str!("migrations/0015_model_thinking_mode.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 16,
        name: "shell_tool_routing",
        sql: include_str!("migrations/0016_shell_tool_routing.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 17,
        name: "embedding_model",
        sql: include_str!("migrations/0017_embedding_model.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 18,
        name: "model_origin",
        sql: include_str!("migrations/0018_model_origin.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 19,
        name: "ssh_host_key_fingerprint",
        sql: include_str!("migrations/0019_ssh_host_key_fingerprint.sql"),
        tolerant_dup_column: true,
    },
    Migration {
        version: 20,
        name: "tool_overrides",
        sql: include_str!("migrations/0020_tool_overrides.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 21,
        name: "remove_crm_agent_write_tools_setting",
        sql: include_str!("migrations/0021_remove_crm_agent_write_tools_setting.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 22,
        name: "short_term_fts",
        sql: include_str!("migrations/0022_short_term_fts.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 23,
        name: "classifier_to_text_analysis",
        sql: include_str!("migrations/0023_classifier_to_text_analysis.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 24,
        name: "workflow_dedupe_seen",
        sql: include_str!("migrations/0024_workflow_dedupe_seen.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 25,
        name: "provider_model_cache",
        sql: include_str!("migrations/0025_provider_model_cache.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 26,
        name: "workflow_backups",
        sql: include_str!("migrations/0026_workflow_backups.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 27,
        name: "tts_piper_description",
        sql: include_str!("migrations/0027_tts_piper_description.sql"),
        tolerant_dup_column: false,
    },
    Migration {
        version: 28,
        name: "notifications",
        sql: include_str!("migrations/0028_notifications.sql"),
        tolerant_dup_column: false,
    },
];

const SEED_SQL: &str = include_str!("seed.sql");
const NORMALIZE_SQL: &str = include_str!("normalize.sql");

/// Full database bootstrap. Call once at startup with a writable connection.
pub fn init(conn: &Connection) -> Result<()> {
    run_migrations(conn).context("run schema migrations")?;
    conn.execute_batch(SEED_SQL).context("seed default rows")?;
    conn.execute_batch(NORMALIZE_SQL)
        .context("normalize legacy settings")?;
    recover_stale_state(conn).context("recover stale run state")?;
    Ok(())
}

fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY,
             name       TEXT NOT NULL,
             applied_at TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )
    .context("create schema_migrations table")?;

    let applied_max: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    for m in MIGRATIONS {
        if m.version <= applied_max {
            continue;
        }
        apply_migration(conn, m)
            .with_context(|| format!("apply migration {} ({})", m.version, m.name))?;
        conn.execute(
            "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![m.version, m.name],
        )
        .with_context(|| format!("record migration {}", m.version))?;
        tracing::info!("DB migration {} ({}) applied", m.version, m.name);
    }
    Ok(())
}

fn apply_migration(conn: &Connection, m: &Migration) -> Result<()> {
    // Strict migrations run as one batch (handles multi-statement triggers /
    // virtual tables correctly). Tolerant migrations run statement-by-statement
    // so a single expected "duplicate column" can be skipped without masking
    // any other failure.
    if !m.tolerant_dup_column {
        return conn.execute_batch(m.sql).map_err(Into::into);
    }

    // Strip `-- ...` line comments BEFORE splitting on ';'. A semicolon *inside*
    // a comment (e.g. "(ignored); the UPDATE ...") must not be treated as a
    // statement boundary — otherwise the comment's tail leaks into the following
    // statement and SQLite rejects it as a syntax error. (Migration SQL never
    // puts `--` inside a string literal, so cutting at the first `--` is safe.)
    let sql: String = m
        .sql
        .lines()
        .map(|line| match line.find("--") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");

    for raw in sql.split(';') {
        let stmt = raw.trim();
        if stmt.is_empty() {
            continue;
        }
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string().to_lowercase();
            if msg.contains("duplicate column name") {
                continue; // already present on current-schema databases — expected
            }
            return Err(anyhow::Error::new(e)).with_context(|| format!("statement: {stmt}"));
        }
    }
    Ok(())
}

/// Mark runs/workflow_runs left in `running` by a previous crash as failed.
/// Run on every boot — this is operational recovery, not a migration.
fn recover_stale_state(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "UPDATE runs
            SET status = 'failed',
                result = 'Terminated: agent restarted',
                finished_at = datetime('now')
          WHERE status = 'running';

         UPDATE workflow_runs
            SET status = 'failed',
                finished_at = datetime('now')
          WHERE status = 'running';",
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_exists(conn: &Connection, table: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    }

    fn col_exists(conn: &Connection, table: &str, col: &str) -> bool {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|c| c.ok())
            .collect();
        cols.iter().any(|c| c == col)
    }

    fn setting(conn: &Connection, key: &str) -> Option<String> {
        conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
            r.get(0)
        })
        .ok()
    }

    #[test]
    fn fresh_db_initializes_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        // Running again must be a clean no-op (recorded migrations are skipped,
        // seeds are INSERT OR IGNORE, normalizations are WHERE-guarded).
        init(&conn).unwrap();

        let max: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            max,
            MIGRATIONS.last().unwrap().version,
            "all migrations should be recorded"
        );

        for t in [
            "settings",
            "runs",
            "workflows",
            "workflow_nodes",
            "watchers",
            "oauth_tokens",
            "tool_patterns",
            "observations",
            "schema_migrations",
            "telegram_reply_routes",
            "notifications",
        ] {
            assert!(table_exists(&conn, t), "missing table {t}");
        }

        assert!(col_exists(&conn, "watchers", "trigger_condition"));
        assert!(col_exists(&conn, "workflow_nodes", "position_x"));
        assert!(col_exists(&conn, "http_requests", "limit"));
        // Durable Wait: a suspended run records when/where to resume.
        assert!(col_exists(&conn, "workflow_runs", "resume_at"));
        assert!(col_exists(&conn, "workflow_runs", "resume_node_id"));
        // Node reliability: retry config + sub-workflow parent linkage.
        assert!(col_exists(&conn, "workflow_nodes", "retries"));
        assert!(col_exists(&conn, "workflow_nodes", "retry_wait_ms"));
        assert!(col_exists(&conn, "workflow_nodes", "retry_backoff"));
        assert!(col_exists(&conn, "workflow_runs", "parent_run_id"));
        // Embedding provenance: memory vectors are tagged with their model.
        assert!(col_exists(&conn, "long_term", "embedding_model"));
        // Model provenance: 'toml' rows were seeded from models.toml, 'runtime'
        // rows were added via the dashboard / Homeostasis node. The boot sync is
        // insert-only, so existing rows of either origin survive a redeploy.
        assert!(col_exists(&conn, "models", "origin"));
        // Configurable embeddings provider settings are seeded.
        assert!(setting(&conn, "embedder.base_url").is_some());
        assert!(setting(&conn, "embedder.model").is_some());
        assert!(setting(&conn, "embedder.api_key").is_some());

        // Seeds + normalization: parallel-tool default is the lowered 3, and the
        // new quality-check mode is present.
        assert_eq!(
            setting(&conn, "agent.max_parallel_tools").as_deref(),
            Some("3")
        );
        assert_eq!(
            setting(&conn, "agent.quality_check_mode").as_deref(),
            Some("mutating")
        );
        // System prompt was seeded and patched with the native-tool-calling note.
        let sp = setting(&conn, "agent.system_prompt").unwrap();
        assert!(sp.contains("native JSON tool calling"));
        // The worldview directive is present exactly once — the seed carries it,
        // so the normalize.sql append must not double it up.
        assert_eq!(sp.matches("SPIRITUAL & BIBLICAL QUESTIONS").count(), 1);
    }

    #[test]
    fn upgrades_legacy_database_in_place() {
        // An OLD database: a couple of tables predate later columns, and the
        // operator never changed the parallel-tool default (stored as "5").
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE watchers (id TEXT PRIMARY KEY, service TEXT NOT NULL);
             CREATE TABLE settings (
                 key TEXT PRIMARY KEY, value TEXT NOT NULL, value_type TEXT NOT NULL,
                 description TEXT, category TEXT, updated_at TEXT NOT NULL DEFAULT (datetime('now')));
             INSERT INTO settings (key, value, value_type) VALUES ('agent.max_parallel_tools','5','int');",
        )
        .unwrap();

        init(&conn).unwrap();

        // The additive migration backfilled the missing column on the old table.
        assert!(col_exists(&conn, "watchers", "trigger_condition"));
        // Normalization lowered the untouched 5 -> 3.
        assert_eq!(
            setting(&conn, "agent.max_parallel_tools").as_deref(),
            Some("3")
        );
    }

    #[test]
    fn operator_customized_value_is_preserved() {
        let conn = Connection::open_in_memory().unwrap();
        // Pretend the operator deliberately set 8 parallel tools before upgrade.
        conn.execute_batch(
            "CREATE TABLE settings (
                 key TEXT PRIMARY KEY, value TEXT NOT NULL, value_type TEXT NOT NULL,
                 description TEXT, category TEXT, updated_at TEXT NOT NULL DEFAULT (datetime('now')));
             INSERT INTO settings (key, value, value_type) VALUES ('agent.max_parallel_tools','8','int');",
        )
        .unwrap();
        init(&conn).unwrap();
        // Only the default-5 is normalized; an explicit 8 is left alone.
        assert_eq!(
            setting(&conn, "agent.max_parallel_tools").as_deref(),
            Some("8")
        );
    }
}
