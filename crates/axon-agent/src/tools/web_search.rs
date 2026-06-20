// web_search.rs
// Web search tool using Tavily Search API
// Dashboard-configurable: add unlimited Tavily accounts via web_search_accounts table
// Auto-fallback: rotates through accounts in priority order, disables exhausted ones

use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

const TAVILY_API_URL: &str = "https://api.tavily.com/search";

// ─────────────────────────────────────────────
// Shared result types
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub account_used: String,
    pub results: Vec<SearchResult>,
}

// ─────────────────────────────────────────────
// Tavily account from database
// ─────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SearchAccount {
    id: String,
    name: String,
    api_key: String,
    #[allow(dead_code)]
    queries_this_month: i64,
    #[allow(dead_code)]
    priority: i64,
}

// ─────────────────────────────────────────────
// Web Search Tool
// ─────────────────────────────────────────────

pub struct WebSearchTool {
    db: Arc<Pool<SqliteConnectionManager>>,
    client: reqwest::Client,
}

impl WebSearchTool {
    pub fn new(db: Arc<Pool<SqliteConnectionManager>>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        Self { db, client }
    }

    /// Load all enabled accounts ordered by priority then least used first
    fn load_accounts(&self) -> Result<Vec<SearchAccount>> {
        let conn = self.db.get().context("DB pool")?;
        let mut stmt = conn.prepare(
            "SELECT id, name, api_key, queries_this_month, priority \
             FROM web_search_accounts \
             WHERE enabled = 1 \
             ORDER BY priority ASC, queries_this_month ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(SearchAccount {
                id: row.get(0)?,
                name: row.get(1)?,
                api_key: row.get(2)?,
                queries_this_month: row.get(3)?,
                priority: row.get(4)?,
            })
        })?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Disable an account when its monthly quota is exhausted.
    /// Re-enable manually from the dashboard at the start of next billing cycle.
    fn disable_account(&self, account_id: &str, reason: &str) -> Result<()> {
        let conn = self.db.get().context("DB pool")?;
        conn.execute(
            "UPDATE web_search_accounts SET enabled = 0 WHERE id = ?1",
            rusqlite::params![account_id],
        )?;
        tracing::warn!(
            "WebSearch: Account {} disabled — {}. Re-enable from dashboard next billing cycle.",
            account_id,
            reason
        );
        Ok(())
    }

    /// Increment monthly query counter for a successful request
    fn increment_usage(&self, account_id: &str) -> Result<()> {
        let conn = self.db.get().context("DB pool")?;
        conn.execute(
            "UPDATE web_search_accounts \
             SET queries_this_month = queries_this_month + 1 \
             WHERE id = ?1",
            rusqlite::params![account_id],
        )?;
        Ok(())
    }

    /// Read search_depth from settings ("basic" or "advanced").
    /// Defaults to "basic" (1 credit/query) if not set or invalid.
    fn get_search_depth(&self) -> String {
        let conn = match self.db.get() {
            Ok(c) => c,
            Err(_) => return "basic".to_string(),
        };
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'websearch.search_depth'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .filter(|v| v == "basic" || v == "advanced")
        .unwrap_or_else(|| "basic".to_string())
    }

    /// Main search — tries all enabled accounts until one succeeds
    pub async fn search(&self, query: &str, top_n: u8) -> Result<SearchResponse> {
        let top_n = top_n.min(10);
        let accounts = self.load_accounts()?;

        if accounts.is_empty() {
            anyhow::bail!(
                "No enabled Tavily accounts. Add an account in the dashboard \
                 or re-enable an account if the monthly quota has reset."
            );
        }

        let search_depth = self.get_search_depth();
        let mut last_error = None;

        for account in accounts {
            match self
                .query_tavily(&account, query, top_n, &search_depth)
                .await
            {
                Ok(results) => {
                    let _ = self.increment_usage(&account.id);
                    return Ok(SearchResponse {
                        query: query.to_string(),
                        account_used: account.name,
                        results,
                    });
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("429") || err_str.contains("quota") {
                        // Monthly quota exhausted — disable and try next account
                        let _ = self.disable_account(&account.id, "monthly quota exhausted (429)");
                    } else if err_str.contains("401") || err_str.contains("unauthorized") {
                        // Bad API key — disable immediately
                        let _ = self.disable_account(&account.id, "invalid API key (401)");
                    } else {
                        tracing::warn!("WebSearch: Account {} error: {}", account.name, err_str);
                    }
                    last_error = Some(err_str);
                }
            }
        }

        anyhow::bail!(
            "All Tavily accounts failed. Last error: {}. Check dashboard for account status.",
            last_error.unwrap_or_else(|| "Unknown".to_string())
        )
    }

    /// Get usage status for all accounts — for dashboard display
    /// Returns (name, queries_this_month, enabled)
    pub fn quota_status(&self) -> Result<Vec<(String, i64, bool)>> {
        let conn = self.db.get().context("DB pool")?;
        let mut stmt = conn.prepare(
            "SELECT name, queries_this_month, enabled \
             FROM web_search_accounts \
             ORDER BY priority ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)? == 1,
            ))
        })?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Run once at startup — idempotent, safe to call on every restart.
    ///
    /// Migration strategy for the cx column:
    ///   The old CSE schema had `cx TEXT NOT NULL`. SQLite's ALTER TABLE DROP COLUMN
    ///   only works on 3.35.0+ and silently fails on older versions, leaving the NOT NULL
    ///   constraint intact and breaking all Tavily inserts with:
    ///     "NOT NULL constraint failed: web_search_accounts.cx"
    ///
    ///   The fix is the standard SQLite table-recreation pattern — works on ALL versions:
    ///     1. Create new table with correct Tavily schema
    ///     2. Copy existing rows (api_key, name, etc — drop cx)
    ///     3. Drop old table
    ///     4. Rename new table
    pub fn migrate(db: &Pool<SqliteConnectionManager>) -> Result<()> {
        let conn = db.get().context("DB pool")?;

        // ── Step 1: settings table (always safe) ─────────────────────────
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS settings (
                key         TEXT PRIMARY KEY,
                value       TEXT NOT NULL,
                value_type  TEXT NOT NULL,
                description TEXT,
                category    TEXT,
                updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- 'basic'    = 1 credit/query  (default, recommended for free tier)
            -- 'advanced' = 2 credits/query (richer results, burns quota faster)
            INSERT OR IGNORE INTO settings (key, value, value_type, description, category)
            VALUES (
                'websearch.search_depth',
                'basic',
                'string',
                'Tavily search depth: basic (1 credit) or advanced (2 credits)',
                'websearch'
            );
            ",
        )?;

        // ── Step 2: Check current columns Procedurally (Compatible with ALL SQLite versions) ──
        let mut cx_exists = false;
        let mut qtm_exists = false;
        {
            let mut stmt = conn.prepare("PRAGMA table_info(web_search_accounts)")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let name: String = row.get(1)?;
                if name == "cx" {
                    cx_exists = true;
                }
                if name == "queries_this_month" {
                    qtm_exists = true;
                }
            }
        }

        // ── Step 3: Check if the table exists at all ──
        let table_exists = {
            let mut stmt = conn.prepare(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='web_search_accounts'",
            )?;
            let count: i64 = stmt.query_row([], |r| r.get(0)).unwrap_or(0);
            count > 0
        };

        if !table_exists {
            // Fresh install — create directly with the correct schema
            conn.execute_batch(
                "
                CREATE TABLE web_search_accounts (
                    id                  TEXT    PRIMARY KEY,
                    name                TEXT    NOT NULL,
                    api_key             TEXT    NOT NULL,
                    priority            INTEGER NOT NULL DEFAULT 1,
                    enabled             INTEGER NOT NULL DEFAULT 1,
                    queries_this_month  INTEGER NOT NULL DEFAULT 0
                );
                ",
            )?;
            tracing::info!("WebSearch: Created web_search_accounts table (fresh install).");
        } else if cx_exists || !qtm_exists {
            // Old CSE schema detected (has cx column, or missing queries_this_month).
            // Use table-recreation — the only SQLite-version-safe way to change schema.
            tracing::info!(
                "WebSearch: Migrating web_search_accounts from CSE schema to Tavily schema \
                 (cx_exists={}, qtm_exists={}).",
                cx_exists,
                qtm_exists
            );

            conn.execute_batch(
                "
                -- 1. New table with correct Tavily schema
                CREATE TABLE IF NOT EXISTS web_search_accounts_new (
                    id                  TEXT    PRIMARY KEY,
                    name                TEXT    NOT NULL,
                    api_key             TEXT    NOT NULL,
                    priority            INTEGER NOT NULL DEFAULT 1,
                    enabled             INTEGER NOT NULL DEFAULT 1,
                    queries_this_month  INTEGER NOT NULL DEFAULT 0
                );

                -- 2. Copy rows — cx is intentionally dropped.
                --    queries_this_month resets to 0: the old column was a daily CSE
                --    counter and does not map to Tavily's monthly credit model.
                INSERT OR IGNORE INTO web_search_accounts_new
                    (id, name, api_key, priority, enabled, queries_this_month)
                SELECT id, name, api_key, priority, enabled, 0
                FROM web_search_accounts;

                -- 3. Swap tables
                DROP TABLE web_search_accounts;
                ALTER TABLE web_search_accounts_new RENAME TO web_search_accounts;
                ",
            )?;
            tracing::info!("WebSearch: Schema migration complete. cx column removed.");
        }
        // else: schema is already correct, nothing to do

        tracing::info!("WebSearch: DB migration complete.");
        Ok(())
    }

    // ── Core Tavily HTTP call ─────────────────────────────────────────────
    async fn query_tavily(
        &self,
        account: &SearchAccount,
        query: &str,
        max_results: u8,
        search_depth: &str,
    ) -> Result<Vec<SearchResult>> {
        let payload = serde_json::json!({
            "api_key": account.api_key,
            "query": query,
            "search_depth": search_depth,
            "max_results": max_results,
            "include_images": false,
            "include_answer": false,
            "include_raw_content": false
        });

        let response = self
            .client
            .post(TAVILY_API_URL)
            .json(&payload)
            .send()
            .await
            .context("HTTP request failed")?;

        let status = response.status();

        if status == 401 {
            anyhow::bail!("401 unauthorized - invalid Tavily API key");
        }
        if status == 403 {
            anyhow::bail!("403 forbidden - account suspended or plan issue");
        }
        if status == 429 {
            anyhow::bail!("429 quota exceeded - monthly limit reached");
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status, body);
        }

        let parsed: TavilyResponse = response.json().await.context("JSON parse error")?;

        Ok(parsed
            .results
            .into_iter()
            .map(|item| SearchResult {
                title: item.title,
                url: item.url,
                snippet: item.content,
                score: item.score,
            })
            .collect())
    }
}

// ─────────────────────────────────────────────
// Tavily response shapes
// ─────────────────────────────────────────────

#[derive(Deserialize, Debug)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyItem>,
}

#[derive(Deserialize, Debug)]
struct TavilyItem {
    title: String,
    url: String,
    content: String,
    #[serde(default)]
    score: f64,
}

// ─────────────────────────────────────────────
// Format for LLM context injection
// ─────────────────────────────────────────────

pub fn format_for_llm(resp: &SearchResponse, top_n: usize) -> String {
    let mut out = format!("Web search results for: \"{}\"\n\n", resp.query);
    for (i, r) in resp.results.iter().take(top_n).enumerate() {
        out.push_str(&format!(
            "[{}] {}\nURL: {}\n{}\n\n",
            i + 1,
            r.title,
            r.url,
            r.snippet.trim(),
        ));
    }
    out
}

// ─────────────────────────────────────────────
// Tool wrapper for Axon's ToolRegistry
// ─────────────────────────────────────────────

use crate::tools::schema::{ToolDefinition, ToolSource};

pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".to_string(),
        description:
            "Search the web using Tavily. Automatically rotates through configured API accounts."
                .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "top_n": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10,
                    "default": 5,
                    "description": "Number of results (max 10)"
                }
            },
            "required": ["query"]
        }),
        required: vec!["query".to_string()],
        source: ToolSource::Internal,
        enabled: true,
        is_mutating: false,
    }
}
