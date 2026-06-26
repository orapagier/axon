//! Engram — persistent key-value store.
//!
//! Long-term memory that survives across workflow runs, backed by the agent's
//! shared SQLite pool (`state.db`). Values are stored as text; anything that
//! round-trips as JSON is parsed back into a real JSON value on read. The table
//! is created lazily on first use so no separate migration is required.

use crate::state::AppState;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

const INIT: &str = "CREATE TABLE IF NOT EXISTS engram_store (
    scope TEXT NOT NULL,
    key   TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (scope, key)
)";

const UPSERT: &str = "INSERT INTO engram_store (scope, key, value, updated_at)
     VALUES (?1, ?2, ?3, datetime('now'))
     ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')";

pub(crate) async fn execute(config: &Value, state: &AppState) -> Result<Value, String> {
    let op = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("get");
    let scope = config
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .trim();
    let scope = if scope.is_empty() { "default" } else { scope };
    let key = config
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let conn = state.db.get().map_err(|e| format!("DB pool: {e}"))?;
    conn.execute(INIT, [])
        .map_err(|e| format!("engram init: {e}"))?;

    match op {
        "set" => {
            if key.is_empty() {
                return Err("Engram Set requires a Key".into());
            }
            let stored = value_to_text(config.get("value"));
            conn.execute(UPSERT, rusqlite::params![scope, key, stored])
                .map_err(|e| format!("engram set: {e}"))?;
            Ok(json!({ "operation": "set", "scope": scope, "key": key, "value": parse_maybe(&stored) }))
        }
        "get" => {
            if key.is_empty() {
                return Err("Engram Get requires a Key".into());
            }
            let found: Option<String> = conn
                .query_row(
                    "SELECT value FROM engram_store WHERE scope = ?1 AND key = ?2",
                    rusqlite::params![scope, key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| format!("engram get: {e}"))?;
            match found {
                Some(s) => Ok(
                    json!({ "operation": "get", "scope": scope, "key": key, "value": parse_maybe(&s), "found": true }),
                ),
                None => {
                    let default = match config.get("defaultValue") {
                        Some(Value::String(s)) if !s.is_empty() => parse_maybe(s),
                        Some(v) if !v.is_null() => v.clone(),
                        _ => Value::Null,
                    };
                    Ok(
                        json!({ "operation": "get", "scope": scope, "key": key, "value": default, "found": false }),
                    )
                }
            }
        }
        "delete" => {
            if key.is_empty() {
                return Err("Engram Delete requires a Key".into());
            }
            let deleted = conn
                .execute(
                    "DELETE FROM engram_store WHERE scope = ?1 AND key = ?2",
                    rusqlite::params![scope, key],
                )
                .map_err(|e| format!("engram delete: {e}"))?;
            Ok(json!({ "operation": "delete", "scope": scope, "key": key, "deleted": deleted }))
        }
        "increment" => {
            if key.is_empty() {
                return Err("Engram Increment requires a Key".into());
            }
            let by = config
                .get("incrementBy")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0);
            let current: Option<String> = conn
                .query_row(
                    "SELECT value FROM engram_store WHERE scope = ?1 AND key = ?2",
                    rusqlite::params![scope, key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| format!("engram increment: {e}"))?;
            let base = current
                .as_deref()
                .and_then(|s| s.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            let next = base + by;
            // Keep whole numbers as integers so reads come back as ints, not 5.0.
            let stored = if next.fract() == 0.0 {
                format!("{}", next as i64)
            } else {
                next.to_string()
            };
            conn.execute(UPSERT, rusqlite::params![scope, key, stored])
                .map_err(|e| format!("engram increment: {e}"))?;
            Ok(json!({ "operation": "increment", "scope": scope, "key": key, "value": parse_maybe(&stored) }))
        }
        "list" => {
            let mut stmt = conn
                .prepare("SELECT key, value FROM engram_store WHERE scope = ?1 ORDER BY key")
                .map_err(|e| format!("engram list: {e}"))?;
            let rows = stmt
                .query_map(rusqlite::params![scope], |r| {
                    let k: String = r.get(0)?;
                    let v: String = r.get(1)?;
                    Ok((k, v))
                })
                .map_err(|e| format!("engram list: {e}"))?;
            let mut keys = Vec::new();
            let mut items = serde_json::Map::new();
            for row in rows {
                let (k, v) = row.map_err(|e| format!("engram list: {e}"))?;
                items.insert(k.clone(), parse_maybe(&v));
                keys.push(k);
            }
            let count = keys.len();
            Ok(json!({ "operation": "list", "scope": scope, "keys": keys, "items": items, "count": count }))
        }
        other => Err(format!("Unknown Engram operation: {other}")),
    }
}

/// Serialize a config value for storage: strings are kept verbatim (so users can
/// store plain text), everything else becomes its JSON text.
fn value_to_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn parse_maybe(s: &str) -> Value {
    serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.to_string()))
}
