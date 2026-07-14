//! Prefetched per-provider model lists for the ModelsPage dropdown.
//!
//! A daily background sweep (`refresh_all`, spawned in `main.rs`) asks each
//! distinct provider currently in the `models` table for its catalogue and
//! stores it in `provider_model_cache`. The dashboard's Model-ID dropdown reads
//! from that table (`read_cached`) so opening the add/edit modal never blocks on
//! a live provider call. Keyed by `(provider, base_url)`; an empty `base_url`
//! means "the provider's default endpoint".

use crate::config::RuntimeSettings;
use crate::providers::types::normalize_base_url_str;
use crate::providers::{list_available_models, ModelChoice};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection};
use std::sync::Arc;

type Db = Arc<Pool<SqliteConnectionManager>>;

/// Canonicalize a base URL into the cache key form: normalized, with
/// `None`/empty collapsing to `""` (the provider default). Must match on both
/// the store and read sides so a lookup finds what the sweep wrote.
pub fn cache_base_key(base_url: Option<&str>) -> String {
    base_url
        .map(normalize_base_url_str)
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

/// Read the cached model choices for a `(provider, base_url)` pair, ordered for
/// a stable dropdown (label when present, else id). Empty vec when nothing is
/// cached yet (e.g. before the first sweep, or the provider errored).
///
/// Fallback: when the caller asks with no explicit base (the common "add a new
/// model, leave Base URL blank" case) and nothing is cached under the default
/// key, return whatever is cached for that provider under *any* base_url. This
/// makes the dropdown work for a new Cerebras/NVIDIA/Ollama model even though
/// the existing rows of that provider were cached under their explicit host.
pub fn read_cached(conn: &Connection, provider: &str, base_url: Option<&str>) -> Vec<ModelChoice> {
    let provider = crate::providers::normalize_provider_name(provider);
    let bkey = cache_base_key(base_url);
    let exact = query_choices(conn, &provider, Some(&bkey));
    if !exact.is_empty() || !bkey.is_empty() {
        // Exact hit, or a specific base was requested (don't guess across hosts).
        return exact;
    }
    // Default key missed → any base for this provider (deduped, usually one host).
    query_choices(conn, &provider, None)
}

/// Query cached choices for a provider, optionally scoped to one base_url key
/// (`None` = any base). De-duplicated by model_id, ordered for the dropdown.
fn query_choices(conn: &Connection, provider: &str, base_key: Option<&str>) -> Vec<ModelChoice> {
    let order = "ORDER BY COALESCE(NULLIF(label,''), model_id) COLLATE NOCASE";
    let (sql, bound): (String, Vec<String>) = match base_key {
        Some(b) => (
            format!(
                "SELECT model_id, label FROM provider_model_cache
                 WHERE provider=?1 AND base_url=?2 {order}"
            ),
            vec![provider.to_string(), b.to_string()],
        ),
        None => (
            format!(
                "SELECT model_id, MIN(label) FROM provider_model_cache
                 WHERE provider=?1 GROUP BY model_id {order}"
            ),
            vec![provider.to_string()],
        ),
    };
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map(rusqlite::params_from_iter(bound), |r| {
        Ok(ModelChoice {
            id: r.get::<_, String>(0)?,
            label: r.get::<_, Option<String>>(1)?.filter(|l| !l.is_empty()),
        })
    });
    match rows {
        Ok(rows) => rows.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    }
}

/// Replace the cached choices for one `(provider, base_url)` pair.
pub fn store(
    conn: &Connection,
    provider: &str,
    base_url: Option<&str>,
    choices: &[ModelChoice],
) -> rusqlite::Result<()> {
    let provider = crate::providers::normalize_provider_name(provider);
    let bkey = cache_base_key(base_url);
    conn.execute(
        "DELETE FROM provider_model_cache WHERE provider=?1 AND base_url=?2",
        params![provider, bkey],
    )?;
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO provider_model_cache (provider, base_url, model_id, label)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for c in choices {
        stmt.execute(params![provider, bkey, c.id, c.label])?;
    }
    Ok(())
}

/// One background sweep: for every distinct `(provider, base_url)` among the
/// enabled models, resolve a usable API key and fetch + store that provider's
/// catalogue. Per-provider failures are logged and skipped — one dead key never
/// aborts the sweep. Returns a short human summary for the caller to log.
pub async fn refresh_all(db: Db, settings: Arc<RuntimeSettings>) -> String {
    // (provider, base_url) -> first usable resolved key. Reading the models and
    // resolving keys is quick, blocking SQLite work; the HTTP fetches below are
    // the slow part and run off this borrowed connection.
    let groups: Vec<(String, Option<String>, String)> = {
        let conn = match db.get() {
            Ok(c) => c,
            Err(e) => return format!("model list refresh skipped: DB unavailable ({})", e),
        };
        let models = crate::config::load_models_from_db(&conn).unwrap_or_default();
        let mut seen: std::collections::HashMap<(String, String), String> =
            std::collections::HashMap::new();
        for m in models {
            if !m.enabled {
                continue;
            }
            let bkey = cache_base_key(m.base_url.as_deref());
            let entry = seen.entry((m.provider.clone(), bkey)).or_default();
            if entry.is_empty() {
                let resolved = settings.resolve(&m.api_key);
                if is_usable_key(&resolved) {
                    *entry = resolved;
                }
            }
        }
        seen.into_iter()
            .map(|((p, b), k)| (p, if b.is_empty() { None } else { Some(b) }, k))
            .collect()
    };

    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut cached = 0usize;
    for (provider, base_url, api_key) in groups {
        if !is_usable_key(&api_key) {
            // No resolvable key for this provider — leave any prior cache in
            // place and move on silently (avoids log spam for unconfigured keys).
            continue;
        }
        match list_available_models(&provider, base_url.as_deref(), &api_key).await {
            Ok(choices) => {
                cached += choices.len();
                ok += 1;
                if let Ok(conn) = db.get() {
                    if let Err(e) = store(&conn, &provider, base_url.as_deref(), &choices) {
                        tracing::warn!("model_cache: store for '{}' failed: {}", provider, e);
                    }
                }
            }
            Err(e) => {
                failed += 1;
                tracing::warn!(
                    "model_cache: list refresh for '{}' failed: {:#}",
                    provider,
                    e
                );
            }
        }
    }
    format!(
        "{} provider(s) refreshed, {} failed, {} model ids cached",
        ok, failed, cached
    )
}

/// A key that can actually authenticate a live call: non-empty and not an
/// unresolved `${VAR}` placeholder.
fn is_usable_key(key: &str) -> bool {
    let k = key.trim();
    !k.is_empty() && !(k.starts_with("${") && k.ends_with("}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init(&conn).unwrap();
        conn
    }

    #[test]
    fn base_key_normalizes_and_collapses_empty() {
        assert_eq!(cache_base_key(None), "");
        assert_eq!(cache_base_key(Some("")), "");
        assert_eq!(cache_base_key(Some("  ")), "");
        assert_eq!(
            cache_base_key(Some("https://api.cerebras.ai/v1/")),
            "https://api.cerebras.ai/v1"
        );
    }

    #[test]
    fn store_then_read_round_trips_and_replaces() {
        let conn = mem_db();
        store(
            &conn,
            "google",
            None,
            &[
                ModelChoice {
                    id: "gemini-3.1-flash-lite".into(),
                    label: Some("Flash Lite".into()),
                },
                ModelChoice {
                    id: "gemini-3.1-pro".into(),
                    label: None,
                },
            ],
        )
        .unwrap();
        // Alias for provider is normalized on write and read.
        let got = read_cached(&conn, "gemini", None);
        assert_eq!(got.len(), 2);
        // Ordered by label-or-id: "Flash Lite" < "gemini-3.1-pro".
        assert_eq!(got[0].id, "gemini-3.1-flash-lite");

        // A second store fully replaces the prior list for that key.
        store(
            &conn,
            "google",
            None,
            &[ModelChoice {
                id: "only-one".into(),
                label: None,
            }],
        )
        .unwrap();
        let got = read_cached(&conn, "google", None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "only-one");
    }

    #[test]
    fn blank_base_falls_back_to_provider_rows_under_any_host() {
        // Existing models cached under an explicit host (as the daily sweep does).
        let conn = mem_db();
        store(
            &conn,
            "cerebras",
            Some("https://api.cerebras.ai/v1"),
            &[ModelChoice {
                id: "gpt-oss-120b".into(),
                label: None,
            }],
        )
        .unwrap();
        // Adding a NEW cerebras model with Base URL left blank still lists them.
        let got = read_cached(&conn, "cerebras", None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "gpt-oss-120b");
    }

    #[test]
    fn read_is_scoped_by_base_url() {
        let conn = mem_db();
        store(
            &conn,
            "openai",
            None,
            &[ModelChoice {
                id: "default-host".into(),
                label: None,
            }],
        )
        .unwrap();
        store(
            &conn,
            "openai",
            Some("https://custom.example/v1"),
            &[ModelChoice {
                id: "custom-host".into(),
                label: None,
            }],
        )
        .unwrap();
        assert_eq!(read_cached(&conn, "openai", None)[0].id, "default-host");
        assert_eq!(
            read_cached(&conn, "openai", Some("https://custom.example/v1/"))[0].id,
            "custom-host"
        );
    }
}
