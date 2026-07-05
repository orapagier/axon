//! Universal SQL Database workflow node.
//!
//! One node that talks to Postgres, MySQL, MariaDB and SQLite. MariaDB speaks the
//! MySQL wire protocol, so it shares the MySQL driver. All four run through sqlx's
//! runtime-generic `Any` backend, so there is a single query/decoding code path
//! instead of one per engine.
//!
//! Connection details arrive in `config` after `interpolate_config` merges the
//! selected credential (service "database") in — the same path Slack/Discord/GitHub
//! use for their tokens. The credential holds `host`, `port`, `user`, `password`,
//! `database` (or a single `connection_string` escape hatch). For SQLite the
//! `database` field is a file path.
//!
//! Safety: values are ALWAYS sent as bound parameters (never string-concatenated),
//! and table/column identifiers in the CRUD builders are validated + dialect-quoted.
//! The `where` clause of Select/Update/Delete is raw author-written SQL — the same
//! trust level as the Execute Query action, which is available anyway — so callers
//! who need parameterized filters should use Execute Query with `params`.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Map, Value};
use sqlx::any::{AnyArguments, AnyPoolOptions, AnyRow};
use sqlx::query::Query;
use sqlx::{Any, AnyPool, Column, Row, ValueRef};
use std::time::Duration;

// ── Dialect ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dialect {
    Postgres,
    MySql,
    Sqlite,
}

impl Dialect {
    fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" => Ok(Dialect::Postgres),
            "mysql" | "mariadb" => Ok(Dialect::MySql),
            "sqlite" | "sqlite3" => Ok(Dialect::Sqlite),
            other => Err(format!(
                "Unsupported db_type '{other}' (use postgres, mysql, mariadb, or sqlite)"
            )),
        }
    }

    fn quote_char(self) -> char {
        match self {
            Dialect::MySql => '`',
            Dialect::Postgres | Dialect::Sqlite => '"',
        }
    }

    /// Positional placeholder for the Nth (1-based) bind. Postgres numbers them
    /// (`$1`); MySQL and SQLite use anonymous `?`.
    fn placeholder(self, idx: usize) -> String {
        match self {
            Dialect::Postgres => format!("${idx}"),
            Dialect::MySql | Dialect::Sqlite => "?".to_string(),
        }
    }
}

// ── Config helpers (mirrors github.rs) ────────────────────────────────────────

fn str_val(config: &Value, key: &str) -> Option<String> {
    config.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        Value::Object(_) | Value::Array(_) => {
            let s = serde_json::to_string(v).unwrap_or_default();
            (!s.is_empty()).then_some(s)
        }
    })
}

fn require(config: &Value, key: &str) -> Result<String, String> {
    match str_val(config, key) {
        Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        _ => Err(format!("Missing required field '{key}' in Database config")),
    }
}

fn confirm_no_where(cfg: &Value) -> bool {
    matches!(cfg.get("allow_no_where"), Some(Value::Bool(true)))
        || str_val(cfg, "allow_no_where")
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false)
}

// ── Identifier validation & quoting ───────────────────────────────────────────

/// A single identifier segment: starts with a letter/underscore, then letters,
/// digits, underscores. No spaces, quotes, or punctuation — which is what keeps
/// table/column names from being an injection vector.
fn valid_ident_segment(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// Validate and dialect-quote a (possibly dotted, e.g. `schema.table`) identifier.
/// Each segment is validated then wrapped in the dialect's quote char so reserved
/// words and mixed case survive.
fn quote_ident(dialect: Dialect, ident: &str) -> Result<String, String> {
    let qc = dialect.quote_char();
    let mut quoted = Vec::new();
    for seg in ident.split('.') {
        let seg = seg.trim();
        if !valid_ident_segment(seg) {
            return Err(format!(
                "Invalid identifier '{ident}' — table/column names may contain only \
                 letters, digits and underscores"
            ));
        }
        let escaped = seg.replace(qc, &format!("{qc}{qc}"));
        quoted.push(format!("{qc}{escaped}{qc}"));
    }
    Ok(quoted.join("."))
}

// ── Connection URL ─────────────────────────────────────────────────────────────

fn build_url(dialect: Dialect, cfg: &Value) -> Result<String, String> {
    // Escape hatch: a full DSN on the credential wins over discrete fields.
    for k in ["connection_string", "url", "dsn"] {
        if let Some(s) = str_val(cfg, k).filter(|s| !s.trim().is_empty()) {
            return Ok(s.trim().to_string());
        }
    }

    match dialect {
        Dialect::Sqlite => {
            let raw = require(cfg, "database").map_err(|_| {
                "SQLite needs a 'database' field (a file path) on the credential".to_string()
            })?;
            if raw.starts_with("sqlite:") {
                return Ok(raw);
            }
            // Opaque `sqlite:` form (no `//`) avoids URL authority parsing, which is
            // what mangles Windows drive letters. `mode=rwc` opens-or-creates.
            if raw.contains('?') {
                Ok(format!("sqlite:{raw}"))
            } else {
                Ok(format!("sqlite:{raw}?mode=rwc"))
            }
        }
        Dialect::Postgres | Dialect::MySql => {
            let (scheme, default_port) = match dialect {
                Dialect::Postgres => ("postgres", 5432),
                _ => ("mysql", 3306),
            };
            let host = str_val(cfg, "host").unwrap_or_else(|| "localhost".into());
            let port = str_val(cfg, "port").unwrap_or_else(|| default_port.to_string());
            let user = str_val(cfg, "user")
                .or_else(|| str_val(cfg, "username"))
                .unwrap_or_default();
            let pass = str_val(cfg, "password").unwrap_or_default();
            let db = str_val(cfg, "database")
                .or_else(|| str_val(cfg, "dbname"))
                .unwrap_or_default();

            let auth = if user.is_empty() {
                String::new()
            } else if pass.is_empty() {
                urlencoding::encode(&user).into_owned()
            } else {
                format!(
                    "{}:{}",
                    urlencoding::encode(&user),
                    urlencoding::encode(&pass)
                )
            };
            let mut url = if auth.is_empty() {
                format!("{scheme}://{host}:{port}/{db}")
            } else {
                format!("{scheme}://{auth}@{host}:{port}/{db}")
            };
            if dialect == Dialect::Postgres {
                if let Some(ssl) = str_val(cfg, "sslmode").filter(|s| !s.trim().is_empty()) {
                    url.push_str(&format!("?sslmode={}", ssl.trim()));
                }
            }
            Ok(url)
        }
    }
}

// ── Row decoding ───────────────────────────────────────────────────────────────

/// Decode one column into JSON. sqlx's `Any` backend exposes a limited scalar set
/// (int/float/bool/text/bytes), so exotic types (uuid, json, numeric, timestamp)
/// arrive as text via the driver's own coercion — cast them to text in the query
/// if you need exact fidelity.
fn decode_col(row: &AnyRow, i: usize) -> Value {
    if let Ok(raw) = row.try_get_raw(i) {
        if raw.is_null() {
            return Value::Null;
        }
    }
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return json!(v);
    }
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return json!(v);
    }
    if let Ok(v) = row.try_get::<bool, _>(i) {
        return json!(v);
    }
    if let Ok(v) = row.try_get::<String, _>(i) {
        return json!(v);
    }
    if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
        return json!(STANDARD.encode(v));
    }
    Value::Null
}

fn any_row_to_json(row: &AnyRow) -> Value {
    let mut obj = Map::new();
    for col in row.columns() {
        obj.insert(col.name().to_string(), decode_col(row, col.ordinal()));
    }
    Value::Object(obj)
}

// ── Bind & run ─────────────────────────────────────────────────────────────────

fn bind_all<'q>(
    mut q: Query<'q, Any, AnyArguments<'q>>,
    values: Vec<Value>,
) -> Query<'q, Any, AnyArguments<'q>> {
    for v in values {
        q = match v {
            Value::Null => q.bind(Option::<String>::None),
            Value::Bool(b) => q.bind(b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    q.bind(i)
                } else {
                    q.bind(n.as_f64().unwrap_or(0.0))
                }
            }
            Value::String(s) => q.bind(s),
            other => q.bind(other.to_string()),
        };
    }
    q
}

fn fmt_err(e: sqlx::Error) -> String {
    format!("Database error: {e}")
}

/// Run a row-returning statement (SELECT, RETURNING, SHOW, …).
async fn run_fetch(pool: &AnyPool, sql: &str, binds: Vec<Value>) -> Result<Value, String> {
    let rows = bind_all(sqlx::query(sql), binds)
        .fetch_all(pool)
        .await
        .map_err(fmt_err)?;
    let arr: Vec<Value> = rows.iter().map(any_row_to_json).collect();
    let n = arr.len();
    Ok(json!({ "rows": arr, "row_count": n }))
}

/// Run a write/DDL statement and report affected rows.
async fn run_exec(pool: &AnyPool, sql: &str, binds: Vec<Value>) -> Result<Value, String> {
    let res = bind_all(sqlx::query(sql), binds)
        .execute(pool)
        .await
        .map_err(fmt_err)?;
    Ok(json!({ "success": true, "rows_affected": res.rows_affected() }))
}

// ── Config parsing for structured fields ──────────────────────────────────────

/// Parse the `params` field (a JSON array string) for Execute Query.
fn parse_params(cfg: &Value) -> Result<Vec<Value>, String> {
    match str_val(cfg, "params") {
        Some(s) if !s.trim().is_empty() => match serde_json::from_str::<Value>(&s) {
            Ok(Value::Array(a)) => Ok(a),
            Ok(_) => Err("'params' must be a JSON array, e.g. [1, \"foo\"]".into()),
            Err(e) => Err(format!("'params' is not valid JSON: {e}")),
        },
        _ => Ok(vec![]),
    }
}

/// Parse a JSON-object field (`data` for Insert/Update).
fn parse_object(cfg: &Value, key: &str) -> Result<Map<String, Value>, String> {
    // Already an object (rare — most UIs send a string).
    if let Some(Value::Object(m)) = cfg.get(key) {
        return Ok(m.clone());
    }
    let s = require(cfg, key)?;
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(format!(
            "'{key}' must be a JSON object, e.g. {{\"name\": \"Ann\"}}"
        )),
        Err(e) => Err(format!("'{key}' is not valid JSON: {e}")),
    }
}

// ── Operations ─────────────────────────────────────────────────────────────────

async fn op_execute_query(pool: &AnyPool, cfg: &Value) -> Result<Value, String> {
    let sql = require(cfg, "query")?;
    let params = parse_params(cfg)?;
    let head = sql.trim_start().to_ascii_uppercase();
    let returns_rows = ["SELECT", "WITH", "SHOW", "EXPLAIN", "PRAGMA", "VALUES", "DESCRIBE", "DESC"]
        .iter()
        .any(|k| head.starts_with(k))
        || head.contains(" RETURNING ");
    if returns_rows {
        run_fetch(pool, &sql, params).await
    } else {
        run_exec(pool, &sql, params).await
    }
}

async fn op_select(pool: &AnyPool, dialect: Dialect, cfg: &Value) -> Result<Value, String> {
    let table = quote_ident(dialect, &require(cfg, "table")?)?;
    let cols_raw = str_val(cfg, "columns").unwrap_or_default();
    let cols = if cols_raw.trim().is_empty() || cols_raw.trim() == "*" {
        "*".to_string()
    } else {
        let mut parts = Vec::new();
        for c in cols_raw.split(',') {
            let c = c.trim();
            if !c.is_empty() {
                parts.push(quote_ident(dialect, c)?);
            }
        }
        if parts.is_empty() {
            "*".into()
        } else {
            parts.join(", ")
        }
    };
    let mut sql = format!("SELECT {cols} FROM {table}");
    if let Some(w) = str_val(cfg, "where").filter(|s| !s.trim().is_empty()) {
        sql.push_str(&format!(" WHERE {}", w.trim()));
    }
    if let Some(l) = str_val(cfg, "limit").filter(|s| !s.trim().is_empty()) {
        let n: i64 = l
            .trim()
            .parse()
            .map_err(|_| "'limit' must be a whole number".to_string())?;
        sql.push_str(&format!(" LIMIT {n}"));
    }
    run_fetch(pool, &sql, vec![]).await
}

async fn op_insert(pool: &AnyPool, dialect: Dialect, cfg: &Value) -> Result<Value, String> {
    let table = quote_ident(dialect, &require(cfg, "table")?)?;
    let data = parse_object(cfg, "data")?;
    if data.is_empty() {
        return Err("Insert: 'data' object is empty".into());
    }
    let mut cols = Vec::new();
    let mut phs = Vec::new();
    let mut vals = Vec::new();
    for (i, (k, v)) in data.iter().enumerate() {
        cols.push(quote_ident(dialect, k)?);
        phs.push(dialect.placeholder(i + 1));
        vals.push(v.clone());
    }
    let mut sql = format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        cols.join(", "),
        phs.join(", ")
    );
    // Postgres can hand back the inserted row; other engines just report the count.
    if dialect == Dialect::Postgres {
        sql.push_str(" RETURNING *");
        run_fetch(pool, &sql, vals).await
    } else {
        run_exec(pool, &sql, vals).await
    }
}

async fn op_update(pool: &AnyPool, dialect: Dialect, cfg: &Value) -> Result<Value, String> {
    let table = quote_ident(dialect, &require(cfg, "table")?)?;
    let data = parse_object(cfg, "data")?;
    if data.is_empty() {
        return Err("Update: 'data' object is empty".into());
    }
    let where_clause = str_val(cfg, "where").map(|s| s.trim().to_string()).unwrap_or_default();
    if where_clause.is_empty() && !confirm_no_where(cfg) {
        return Err("Update without a WHERE clause would modify EVERY row. Set a 'where' \
                    clause, or enable 'Allow no WHERE' to proceed."
            .into());
    }
    let mut sets = Vec::new();
    let mut vals = Vec::new();
    for (i, (k, v)) in data.iter().enumerate() {
        sets.push(format!(
            "{} = {}",
            quote_ident(dialect, k)?,
            dialect.placeholder(i + 1)
        ));
        vals.push(v.clone());
    }
    let mut sql = format!("UPDATE {table} SET {}", sets.join(", "));
    if !where_clause.is_empty() {
        sql.push_str(&format!(" WHERE {where_clause}"));
    }
    run_exec(pool, &sql, vals).await
}

async fn op_delete(pool: &AnyPool, dialect: Dialect, cfg: &Value) -> Result<Value, String> {
    let table = quote_ident(dialect, &require(cfg, "table")?)?;
    let where_clause = str_val(cfg, "where").map(|s| s.trim().to_string()).unwrap_or_default();
    if where_clause.is_empty() && !confirm_no_where(cfg) {
        return Err("Delete without a WHERE clause would remove EVERY row. Set a 'where' \
                    clause, or enable 'Allow no WHERE' to proceed."
            .into());
    }
    let mut sql = format!("DELETE FROM {table}");
    if !where_clause.is_empty() {
        sql.push_str(&format!(" WHERE {where_clause}"));
    }
    run_exec(pool, &sql, vec![]).await
}

// ── Public executor ────────────────────────────────────────────────────────────

/// Register sqlx's default drivers (postgres/mysql/sqlite) with the `Any` URL
/// router exactly once for the whole process.
fn ensure_drivers() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(sqlx::any::install_default_drivers);
}

pub(crate) async fn execute(config: &Value) -> Result<Value, String> {
    ensure_drivers();
    let dialect = Dialect::parse(&str_val(config, "db_type").unwrap_or_else(|| "postgres".into()))?;
    let url = build_url(dialect, config)?;

    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&url)
        .await
        .map_err(|e| format!("Database connection failed: {e}"))?;

    let operation = str_val(config, "operation").unwrap_or_else(|| "executeQuery".to_string());
    let result = match operation.as_str() {
        "executeQuery" => op_execute_query(&pool, config).await,
        "select" => op_select(&pool, dialect, config).await,
        "insert" => op_insert(&pool, dialect, config).await,
        "update" => op_update(&pool, dialect, config).await,
        "delete" => op_delete(&pool, dialect, config).await,
        other => Err(format!("Unsupported database operation '{other}'")),
    };

    pool.close().await;
    result
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_parse_aliases() {
        assert_eq!(Dialect::parse("Postgres").unwrap(), Dialect::Postgres);
        assert_eq!(Dialect::parse("postgresql").unwrap(), Dialect::Postgres);
        assert_eq!(Dialect::parse("mariadb").unwrap(), Dialect::MySql);
        assert_eq!(Dialect::parse("MySQL").unwrap(), Dialect::MySql);
        assert_eq!(Dialect::parse("sqlite").unwrap(), Dialect::Sqlite);
        assert!(Dialect::parse("oracle").is_err());
    }

    #[test]
    fn placeholders_are_dialect_specific() {
        assert_eq!(Dialect::Postgres.placeholder(3), "$3");
        assert_eq!(Dialect::MySql.placeholder(3), "?");
        assert_eq!(Dialect::Sqlite.placeholder(1), "?");
    }

    #[test]
    fn identifier_quoting_and_rejection() {
        assert_eq!(quote_ident(Dialect::Postgres, "users").unwrap(), "\"users\"");
        assert_eq!(quote_ident(Dialect::MySql, "users").unwrap(), "`users`");
        assert_eq!(
            quote_ident(Dialect::Postgres, "public.users").unwrap(),
            "\"public\".\"users\""
        );
        // Injection attempts are rejected outright.
        assert!(quote_ident(Dialect::Postgres, "users; DROP TABLE x").is_err());
        assert!(quote_ident(Dialect::Postgres, "1col").is_err());
        assert!(quote_ident(Dialect::Postgres, "a b").is_err());
    }

    #[test]
    fn postgres_url_encodes_password() {
        let cfg = json!({
            "host": "db.example.com", "port": "6432",
            "user": "admin", "password": "p@ss:word/!", "database": "app",
            "sslmode": "require"
        });
        let url = build_url(Dialect::Postgres, &cfg).unwrap();
        assert!(url.starts_with("postgres://admin:"), "got {url}");
        assert!(url.contains("p%40ss%3Aword%2F%21"), "password not encoded: {url}");
        assert!(url.contains("@db.example.com:6432/app"));
        assert!(url.ends_with("?sslmode=require"));
    }

    #[test]
    fn connection_string_overrides_discrete_fields() {
        let cfg = json!({ "connection_string": "mysql://u:p@h/db", "host": "ignored" });
        assert_eq!(build_url(Dialect::MySql, &cfg).unwrap(), "mysql://u:p@h/db");
    }

    #[test]
    fn update_without_where_is_blocked() {
        // Build a config with no where + no override; op requires a pool but we can
        // check the guard fires before any connection by calling the guard logic.
        let cfg = json!({ "table": "t", "data": "{\"x\":1}" });
        assert!(!confirm_no_where(&cfg));
        let cfg2 = json!({ "table": "t", "data": "{\"x\":1}", "allow_no_where": true });
        assert!(confirm_no_where(&cfg2));
    }

    /// End-to-end against a throwaway SQLite file: create → insert → select,
    /// exercising connect → bind → row→JSON.
    #[tokio::test]
    async fn sqlite_roundtrip() {
        let path = std::env::temp_dir()
            .join(format!("axon_db_test_{}.sqlite", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .replace('\\', "/");

        let base = |op: &str| {
            json!({ "db_type": "sqlite", "database": path.clone(), "operation": op })
        };

        // CREATE
        let mut c = base("executeQuery");
        c["query"] = json!("CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)");
        execute(&c).await.expect("create");

        // INSERT (builder)
        let mut c = base("insert");
        c["table"] = json!("people");
        c["data"] = json!("{\"name\": \"Ann\", \"age\": 30}");
        let ins = execute(&c).await.expect("insert");
        assert_eq!(ins["rows_affected"], 1);

        // INSERT (parameterized raw query)
        let mut c = base("executeQuery");
        c["query"] = json!("INSERT INTO people (name, age) VALUES (?, ?)");
        c["params"] = json!("[\"Bob\", 25]");
        execute(&c).await.expect("param insert");

        // SELECT (builder, with where + limit)
        let mut c = base("select");
        c["table"] = json!("people");
        c["columns"] = json!("name, age");
        c["where"] = json!("age >= 30");
        let out = execute(&c).await.expect("select");
        assert_eq!(out["row_count"], 1);
        assert_eq!(out["rows"][0]["name"], "Ann");
        assert_eq!(out["rows"][0]["age"], 30);

        // UPDATE
        let mut c = base("update");
        c["table"] = json!("people");
        c["data"] = json!("{\"age\": 31}");
        c["where"] = json!("name = 'Ann'");
        let upd = execute(&c).await.expect("update");
        assert_eq!(upd["rows_affected"], 1);

        // DELETE without where is blocked
        let mut c = base("delete");
        c["table"] = json!("people");
        assert!(execute(&c).await.is_err(), "empty-where delete must be blocked");

        // DELETE with where
        let mut c = base("delete");
        c["table"] = json!("people");
        c["where"] = json!("name = 'Bob'");
        let del = execute(&c).await.expect("delete");
        assert_eq!(del["rows_affected"], 1);

        let _ = std::fs::remove_file(path.replace('/', std::path::MAIN_SEPARATOR_STR));
    }
}
