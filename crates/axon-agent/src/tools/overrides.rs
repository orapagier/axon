//! Persistence for ToolsPage Enable/Disable toggles (`tool_overrides` table).
//! `ToolRegistry::set_enabled` only mutates the in-memory registry; callers
//! that want a toggle to survive a restart must also persist it here.

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::collections::HashMap;

/// All persisted per-tool overrides, keyed by tool name.
pub fn load_all(db: &Pool<SqliteConnectionManager>) -> HashMap<String, bool> {
    let mut out = HashMap::new();
    let Ok(conn) = db.get() else {
        return out;
    };
    let Ok(mut stmt) = conn.prepare("SELECT name, enabled FROM tool_overrides") else {
        return out;
    };
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
    });
    if let Ok(rows) = rows {
        out.extend(rows.filter_map(|r| r.ok()));
    }
    out
}

/// Persist a single tool's enabled state so it survives a restart.
pub fn upsert(db: &Pool<SqliteConnectionManager>, name: &str, enabled: bool) -> anyhow::Result<()> {
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO tool_overrides (name, enabled, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(name) DO UPDATE SET enabled = excluded.enabled, updated_at = excluded.updated_at",
        rusqlite::params![name, enabled as i64],
    )?;
    Ok(())
}
