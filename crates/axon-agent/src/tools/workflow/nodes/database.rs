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
use std::path::PathBuf;
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

/// Read a boolean-ish value from any of the shapes a UI/expression can produce:
/// a real bool, the strings "true"/"1", or a non-zero number.
fn as_bool(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => {
            let s = s.trim();
            s.eq_ignore_ascii_case("true") || s == "1"
        }
        Some(Value::Number(n)) => n.as_i64().map(|i| i != 0).unwrap_or(false),
        _ => false,
    }
}

fn bool_cfg(cfg: &Value, key: &str) -> bool {
    as_bool(cfg.get(key))
}

fn confirm_no_where(cfg: &Value) -> bool {
    bool_cfg(cfg, "allow_no_where")
}

// ── SQLite database files ──────────────────────────────────────────────────────
//
// A SQLite "database" is just a file. To keep them predictable and listable we
// store bare-named databases in a managed `databases/` folder under the app data
// dir (honoring AXON_DATA_DIR). Advanced users can still pass an explicit path.

/// Managed directory holding user SQLite database files. Created on demand.
pub(crate) fn sqlite_dir() -> PathBuf {
    let dir = axon_core::data_dir().join("databases");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Resolve a user-supplied SQLite database name/path to a filesystem path.
/// A bare name (`sales`, `sales.db`) lands in the managed dir; anything that
/// looks like a path (contains a separator or a drive letter) is honored as-is.
fn resolve_sqlite_path(name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("SQLite: database name is empty".into());
    }
    // Reject path traversal outright, before the explicit-path escape hatch.
    if name.contains("..") {
        return Err("Invalid database name (path traversal not allowed)".into());
    }
    let looks_pathy =
        name.contains('/') || name.contains('\\') || (name.len() > 1 && name.as_bytes()[1] == b':');
    if looks_pathy {
        return Ok(PathBuf::from(name));
    }
    let mut fname = name.to_string();
    if !(fname.ends_with(".db") || fname.ends_with(".sqlite") || fname.ends_with(".sqlite3")) {
        fname.push_str(".db");
    }
    if !fname
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(
            "Database name may contain only letters, digits, underscores and hyphens".into(),
        );
    }
    Ok(sqlite_dir().join(fname))
}

/// List the SQLite database files in the managed directory (for the picker and
/// the List Databases action).
pub(crate) fn list_sqlite_databases() -> Value {
    let dir = sqlite_dir();
    let mut rows: Vec<(String, u64)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            let is_db = p
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| matches!(x, "db" | "sqlite" | "sqlite3"))
                .unwrap_or(false);
            if is_db {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                    rows.push((name.to_string(), size));
                }
            }
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let databases: Vec<Value> = rows
        .into_iter()
        .map(|(name, size)| json!({ "name": name, "size_bytes": size }))
        .collect();
    json!({ "databases": databases, "directory": dir.to_string_lossy() })
}

/// Delete a SQLite database file.
fn drop_sqlite_database(cfg: &Value) -> Result<Value, String> {
    let name = require(cfg, "database")?;
    let path = resolve_sqlite_path(&name)?;
    if !path.exists() {
        return Err(format!("Database '{name}' does not exist"));
    }
    std::fs::remove_file(&path).map_err(|e| format!("Failed to delete database '{name}': {e}"))?;
    Ok(json!({ "dropped": true, "database": name }))
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

/// Coerce a user-typed name into a valid identifier so a stray space or symbol
/// doesn't make the operation error. Applied per dotted segment (so `schema.table`
/// still works): every character that isn't a letter, digit or underscore becomes
/// `_`, and a leading digit gets an `_` prefix (identifiers may not start with a
/// digit). Already-valid names pass through completely unchanged. An empty or
/// all-symbol segment is left alone so the strict `quote_ident` below still rejects
/// it with a helpful message rather than silently inventing a name.
fn sanitize_ident(ident: &str) -> String {
    ident
        .split('.')
        .map(|seg| {
            let seg = seg.trim();
            if seg.is_empty() {
                return String::new();
            }
            let mut out: String = seg
                .chars()
                .map(|c| {
                    if c == '_' || c.is_ascii_alphanumeric() {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                out.insert(0, '_');
            }
            out
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The identifier path for every name a workflow author types (tables, columns,
/// databases): sanitize spaces/symbols to underscores, then run through the strict
/// validator+quoter. `quote_ident` stays strict on its own so it remains the
/// injection boundary and can still reject anything sanitizing can't rescue.
fn quote_user_ident(dialect: Dialect, ident: &str) -> Result<String, String> {
    quote_ident(dialect, &sanitize_ident(ident))
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
            let raw = require(cfg, "database")
                .map_err(|_| "SQLite: choose or name a database first".to_string())?;
            if raw.starts_with("sqlite:") {
                return Ok(raw);
            }
            let path = resolve_sqlite_path(&raw)?;
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Opaque `sqlite:` form (no `//`) avoids URL authority parsing, which is
            // what mangles Windows drive letters. `mode=rwc` opens-or-creates.
            let p = path.to_string_lossy().replace('\\', "/");
            Ok(format!("sqlite:{p}?mode=rwc"))
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

/// Resolve the column → value map for Insert/Update from whichever input the
/// user filled in: the beginner-friendly `data_fields` row editor (default) or
/// the advanced raw-JSON `data` box. `data_mode` records the pick; legacy nodes
/// have no `data_mode` and only a `data` string, so those still parse as JSON.
fn resolve_data(cfg: &Value) -> Result<Map<String, Value>, String> {
    let mode = str_val(cfg, "data_mode").unwrap_or_default();
    let has_json = matches!(cfg.get("data"), Some(Value::Object(_)))
        || str_val(cfg, "data")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
    if mode == "json" || (mode.is_empty() && has_json) {
        return parse_object(cfg, "data");
    }
    parse_data_fields(cfg)
}

/// Build a column → value map from the `data_fields` fixedCollection rows,
/// shaped `{ "parameters": [ { "column": "name", "value": "Ann" }, … ] }`.
/// Rows with a blank column are skipped; values are plain text, lightly coerced
/// by [`coerce_text`] so number/boolean columns aren't stored as strings.
fn parse_data_fields(cfg: &Value) -> Result<Map<String, Value>, String> {
    let rows = cfg
        .get("data_fields")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    let mut out = Map::new();
    for row in rows {
        let col = row
            .get("column")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if col.is_empty() {
            continue;
        }
        out.insert(col.to_string(), coerce_text(row.get("value")));
    }
    if out.is_empty() {
        return Err("Add at least one Data field — a column name and a value.".into());
    }
    Ok(out)
}

/// Turn a plain-text row value into the most natural JSON scalar so number and
/// boolean columns bind as numbers/bools rather than text. Anything that isn't
/// unambiguously a number/bool/null stays a string — and codes that would lose
/// meaning as a number (leading zeros like `007`, a leading `+`) stay strings.
fn coerce_text(v: Option<&Value>) -> Value {
    let s = match v {
        Some(Value::String(s)) => s.clone(),
        // Already typed by an upstream expression, or absent → empty string.
        Some(Value::Null) | None => return Value::String(String::new()),
        Some(other) => return other.clone(),
    };
    let t = s.trim();
    match t {
        "" => Value::String(String::new()),
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "null" => Value::Null,
        _ => {
            if let Ok(i) = t.parse::<i64>() {
                if t == i.to_string() {
                    return Value::Number(i.into());
                }
            } else if let Ok(f) = t.parse::<f64>() {
                if let Some(n) = serde_json::Number::from_f64(f) {
                    return Value::Number(n);
                }
            }
            Value::String(s)
        }
    }
}

// ── Operations ─────────────────────────────────────────────────────────────────

async fn op_execute_query(pool: &AnyPool, cfg: &Value) -> Result<Value, String> {
    let sql = require(cfg, "query")?;
    let params = parse_params(cfg)?;
    let head = sql.trim_start().to_ascii_uppercase();
    let returns_rows = [
        "SELECT", "WITH", "SHOW", "EXPLAIN", "PRAGMA", "VALUES", "DESCRIBE", "DESC",
    ]
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
    let table = quote_user_ident(dialect, &require(cfg, "table")?)?;
    let cols_raw = str_val(cfg, "columns").unwrap_or_default();
    let cols = if cols_raw.trim().is_empty() || cols_raw.trim() == "*" {
        "*".to_string()
    } else {
        let mut parts = Vec::new();
        for c in cols_raw.split(',') {
            let c = c.trim();
            if !c.is_empty() {
                parts.push(quote_user_ident(dialect, c)?);
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
    let table = quote_user_ident(dialect, &require(cfg, "table")?)?;
    let data = resolve_data(cfg)?;
    if data.is_empty() {
        return Err("Insert: 'data' object is empty".into());
    }
    let mut cols = Vec::new();
    let mut phs = Vec::new();
    let mut vals = Vec::new();
    for (i, (k, v)) in data.iter().enumerate() {
        cols.push(quote_user_ident(dialect, k)?);
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
    let table = quote_user_ident(dialect, &require(cfg, "table")?)?;
    let data = resolve_data(cfg)?;
    if data.is_empty() {
        return Err("Update: 'data' object is empty".into());
    }
    let where_clause = str_val(cfg, "where")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if where_clause.is_empty() && !confirm_no_where(cfg) {
        return Err(
            "Update without a WHERE clause would modify EVERY row. Set a 'where' \
                    clause, or enable 'Allow no WHERE' to proceed."
                .into(),
        );
    }
    let mut sets = Vec::new();
    let mut vals = Vec::new();
    for (i, (k, v)) in data.iter().enumerate() {
        sets.push(format!(
            "{} = {}",
            quote_user_ident(dialect, k)?,
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
    let table = quote_user_ident(dialect, &require(cfg, "table")?)?;
    let where_clause = str_val(cfg, "where")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if where_clause.is_empty() && !confirm_no_where(cfg) {
        return Err(
            "Delete without a WHERE clause would remove EVERY row. Set a 'where' \
                    clause, or enable 'Allow no WHERE' to proceed."
                .into(),
        );
    }
    let mut sql = format!("DELETE FROM {table}");
    if !where_clause.is_empty() {
        sql.push_str(&format!(" WHERE {where_clause}"));
    }
    run_exec(pool, &sql, vec![]).await
}

// ── Table management (Create / Drop / Describe) ────────────────────────────────

/// A raw SQL type the author typed in the Type box (e.g. `VARCHAR(100)`,
/// `DECIMAL(10,2)`). Allow only letters/digits/space/parens/commas so the type
/// can't smuggle SQL into the CREATE statement, and require an alphabetic start.
fn valid_raw_type(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && s.chars()
            .next()
            .map(|c| c.is_ascii_alphabetic())
            .unwrap_or(false)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '(' | ')' | ','))
}

/// Map a beginner-friendly logical type to the concrete SQL type for `dialect`.
/// Anything not in the known set is treated as a raw SQL type and validated.
fn map_column_type(dialect: Dialect, logical: &str) -> Result<String, String> {
    let mapped = match logical.trim().to_ascii_lowercase().as_str() {
        "" | "text" | "string" => match dialect {
            Dialect::MySql => "VARCHAR(255)",
            _ => "TEXT",
        },
        "integer" | "int" => match dialect {
            Dialect::MySql => "INT",
            _ => "INTEGER",
        },
        "number" | "float" | "double" | "real" | "decimal" => match dialect {
            Dialect::Postgres => "DOUBLE PRECISION",
            Dialect::MySql => "DOUBLE",
            Dialect::Sqlite => "REAL",
        },
        "boolean" | "bool" => match dialect {
            Dialect::Postgres => "BOOLEAN",
            Dialect::MySql => "TINYINT(1)",
            Dialect::Sqlite => "BOOLEAN",
        },
        "date" => "DATE",
        "datetime" | "timestamp" => match dialect {
            Dialect::MySql => "DATETIME",
            _ => "TIMESTAMP",
        },
        "json" => match dialect {
            Dialect::Postgres => "JSONB",
            Dialect::MySql => "JSON",
            Dialect::Sqlite => "TEXT",
        },
        _ => {
            if valid_raw_type(logical) {
                return Ok(logical.trim().to_string());
            }
            return Err(format!(
                "Unsupported column type '{logical}'. Use text, integer, number, \
                 boolean, date, datetime or json — or a simple SQL type like VARCHAR(100)."
            ));
        }
    };
    Ok(mapped.to_string())
}

/// Render a DEFAULT value literal safely. Numbers and a small keyword allowlist
/// (booleans / NULL / CURRENT_*) embed raw; everything else becomes a quoted
/// string literal with single quotes doubled — so a default can't inject SQL.
fn format_default_literal(raw: &str) -> String {
    let t = raw.trim();
    if t.parse::<i64>().is_ok() || t.parse::<f64>().is_ok() {
        return t.to_string();
    }
    let up = t.to_ascii_uppercase();
    if matches!(
        up.as_str(),
        "TRUE" | "FALSE" | "NULL" | "CURRENT_TIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIME"
    ) {
        return up;
    }
    format!("'{}'", t.replace('\'', "''"))
}

/// Build one column's DDL from a `columns_def` row. `sole_pk` is true when this
/// row is the ONLY primary-key column, which lets a single integer PK become an
/// auto-incrementing id (SERIAL / AUTO_INCREMENT / AUTOINCREMENT per dialect).
fn build_column_def(dialect: Dialect, row: &Value, sole_pk: bool) -> Result<String, String> {
    let name = row
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let col = quote_user_ident(dialect, name)?;
    let logical = row.get("type").and_then(|v| v.as_str()).unwrap_or("text");
    let is_integer = matches!(
        logical.trim().to_ascii_lowercase().as_str(),
        "integer" | "int"
    );
    let is_pk = row_bool(row, "primary_key");
    let auto_pk = sole_pk && is_pk && is_integer;

    let mut parts = vec![col];
    // A single integer PK on Postgres uses SERIAL (which is the type + sequence).
    if auto_pk && dialect == Dialect::Postgres {
        parts.push("SERIAL".to_string());
    } else {
        parts.push(map_column_type(dialect, logical)?);
    }

    if sole_pk && is_pk {
        match dialect {
            Dialect::Sqlite if is_integer => parts.push("PRIMARY KEY AUTOINCREMENT".into()),
            Dialect::MySql if is_integer => parts.push("AUTO_INCREMENT PRIMARY KEY".into()),
            _ => parts.push("PRIMARY KEY".into()),
        }
    } else {
        // PRIMARY KEY already implies NOT NULL + uniqueness, so only apply these
        // to non-sole-PK columns.
        if row_bool(row, "not_null") {
            parts.push("NOT NULL".into());
        }
        if row_bool(row, "unique") {
            parts.push("UNIQUE".into());
        }
    }

    // A DEFAULT is meaningless on an auto-increment id — skip it there only.
    if !auto_pk {
        if let Some(d) = row
            .get("default")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(format!("DEFAULT {}", format_default_literal(d)));
        }
    }
    Ok(parts.join(" "))
}

/// Read a boolean sub-field from a fixedCollection row.
fn row_bool(row: &Value, key: &str) -> bool {
    as_bool(row.get(key))
}

async fn op_create_table(pool: &AnyPool, dialect: Dialect, cfg: &Value) -> Result<Value, String> {
    // The name the author typed may have spaces/symbols; sanitize once so both the
    // SQL and the value we report back reflect the actual table that was created.
    let name = sanitize_ident(&require(cfg, "table")?);
    let table = quote_ident(dialect, &name)?;
    let rows = cfg
        .get("columns_def")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    // Keep only rows with a column name — blank rows the editor leaves behind.
    let cols: Vec<&Value> = rows
        .iter()
        .filter(|r| {
            r.get("name")
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        })
        .collect();
    if cols.is_empty() {
        return Err("Create Table: add at least one column — a name and a type.".into());
    }

    let pk_names: Vec<&str> = cols
        .iter()
        .filter(|r| row_bool(r, "primary_key"))
        .filter_map(|r| r.get("name").and_then(|v| v.as_str()))
        .map(str::trim)
        .collect();
    let sole_pk = pk_names.len() == 1;

    let mut defs = Vec::new();
    for r in &cols {
        defs.push(build_column_def(dialect, r, sole_pk)?);
    }
    // More than one PK column → a table-level composite PRIMARY KEY constraint.
    if pk_names.len() > 1 {
        let quoted = pk_names
            .iter()
            .map(|n| quote_user_ident(dialect, n))
            .collect::<Result<Vec<_>, _>>()?;
        defs.push(format!("PRIMARY KEY ({})", quoted.join(", ")));
    }

    let if_not_exists = if bool_cfg(cfg, "if_not_exists") {
        "IF NOT EXISTS "
    } else {
        ""
    };
    let sql = format!("CREATE TABLE {if_not_exists}{table} ({})", defs.join(", "));
    bind_all(sqlx::query(&sql), vec![])
        .execute(pool)
        .await
        .map_err(fmt_err)?;
    Ok(json!({ "success": true, "created": true, "table": name }))
}

async fn op_drop_table(pool: &AnyPool, dialect: Dialect, cfg: &Value) -> Result<Value, String> {
    let name = sanitize_ident(&require(cfg, "table")?);
    let table = quote_ident(dialect, &name)?;
    let if_exists = if bool_cfg(cfg, "if_exists") {
        "IF EXISTS "
    } else {
        ""
    };
    let sql = format!("DROP TABLE {if_exists}{table}");
    bind_all(sqlx::query(&sql), vec![])
        .execute(pool)
        .await
        .map_err(fmt_err)?;
    Ok(json!({ "success": true, "dropped": true, "table": name }))
}

/// List a table's columns (name, type, nullability, default) — a schema peek so
/// authors can see what to Insert/Select without leaving the node.
async fn op_describe_table(pool: &AnyPool, dialect: Dialect, cfg: &Value) -> Result<Value, String> {
    // Sanitize so describing "user profile" finds the "user_profile" that Create
    // Table made from the same typed name.
    let name_raw = sanitize_ident(&require(cfg, "table")?);
    match dialect {
        Dialect::Sqlite => {
            // PRAGMA takes no bound params; the name is validated + quoted.
            let t = quote_ident(dialect, &name_raw)?;
            run_fetch(pool, &format!("PRAGMA table_info({t})"), vec![]).await
        }
        Dialect::MySql => {
            let t = quote_ident(dialect, &name_raw)?;
            run_fetch(pool, &format!("SHOW COLUMNS FROM {t}"), vec![]).await
        }
        Dialect::Postgres => {
            // Split an optional schema qualifier; both parts bind as parameters.
            let (schema, tbl) = match name_raw.rsplit_once('.') {
                Some((s, t)) => (Some(s.trim().to_string()), t.trim().to_string()),
                None => (None, name_raw.clone()),
            };
            let mut sql = String::from(
                "SELECT column_name, data_type, is_nullable, column_default \
                 FROM information_schema.columns WHERE table_name = $1",
            );
            let mut binds = vec![Value::String(tbl)];
            if let Some(s) = schema {
                sql.push_str(" AND table_schema = $2");
                binds.push(Value::String(s));
            }
            sql.push_str(" ORDER BY ordinal_position");
            run_fetch(pool, &sql, binds).await
        }
    }
}

// ── Database & schema management ──────────────────────────────────────────────

async fn op_create_database(
    pool: &AnyPool,
    dialect: Dialect,
    cfg: &Value,
) -> Result<Value, String> {
    match dialect {
        // For SQLite the file is created by connecting with mode=rwc (build_url),
        // so by the time we get here it already exists — just report it.
        Dialect::Sqlite => {
            let name = require(cfg, "database")?;
            Ok(json!({ "created": true, "database": name }))
        }
        _ => {
            let name = sanitize_ident(&require(cfg, "new_database")?);
            let ident = quote_ident(dialect, &name)?;
            run_exec(pool, &format!("CREATE DATABASE {ident}"), vec![]).await?;
            Ok(json!({ "created": true, "database": name }))
        }
    }
}

async fn op_drop_database_server(
    pool: &AnyPool,
    dialect: Dialect,
    cfg: &Value,
) -> Result<Value, String> {
    let name = sanitize_ident(&require(cfg, "new_database").or_else(|_| require(cfg, "database"))?);
    let ident = quote_ident(dialect, &name)?;
    run_exec(pool, &format!("DROP DATABASE {ident}"), vec![]).await?;
    Ok(json!({ "dropped": true, "database": name }))
}

async fn op_list_databases_server(pool: &AnyPool, dialect: Dialect) -> Result<Value, String> {
    let sql = match dialect {
        Dialect::Postgres => {
            "SELECT datname AS name FROM pg_database WHERE datistemplate = false ORDER BY datname"
        }
        Dialect::MySql => "SHOW DATABASES",
        Dialect::Sqlite => unreachable!("SQLite listDatabases handled before connect"),
    };
    let rows = bind_all(sqlx::query(sql), vec![])
        .fetch_all(pool)
        .await
        .map_err(fmt_err)?;
    let databases: Vec<Value> = rows.iter().map(|r| decode_col(r, 0)).collect();
    let n = databases.len();
    Ok(json!({ "databases": databases, "database_count": n }))
}

async fn op_list_tables(pool: &AnyPool, dialect: Dialect) -> Result<Value, String> {
    let sql = match dialect {
        Dialect::Sqlite => {
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        }
        Dialect::Postgres => {
            "SELECT tablename AS name FROM pg_tables WHERE schemaname NOT IN ('pg_catalog','information_schema') ORDER BY tablename"
        }
        Dialect::MySql => "SHOW TABLES",
    };
    let rows = bind_all(sqlx::query(sql), vec![])
        .fetch_all(pool)
        .await
        .map_err(fmt_err)?;
    let tables: Vec<Value> = rows.iter().map(|r| decode_col(r, 0)).collect();
    let n = tables.len();
    Ok(json!({ "tables": tables, "table_count": n }))
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
    let operation = str_val(config, "operation").unwrap_or_else(|| "executeQuery".to_string());

    // SQLite database management is pure filesystem work — no connection to a
    // target file (dropDatabase must not open the file, and listDatabases just
    // reads the folder).
    if dialect == Dialect::Sqlite {
        match operation.as_str() {
            "listDatabases" => return Ok(list_sqlite_databases()),
            "dropDatabase" => return drop_sqlite_database(config),
            _ => {}
        }
    }

    let url = build_url(dialect, config)?;
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&url)
        .await
        .map_err(|e| format!("Database connection failed: {e}"))?;

    let result = match operation.as_str() {
        "executeQuery" => op_execute_query(&pool, config).await,
        "select" => op_select(&pool, dialect, config).await,
        "insert" => op_insert(&pool, dialect, config).await,
        "update" => op_update(&pool, dialect, config).await,
        "delete" => op_delete(&pool, dialect, config).await,
        "createTable" => op_create_table(&pool, dialect, config).await,
        "dropTable" => op_drop_table(&pool, dialect, config).await,
        "describeTable" => op_describe_table(&pool, dialect, config).await,
        "createDatabase" => op_create_database(&pool, dialect, config).await,
        "dropDatabase" => op_drop_database_server(&pool, dialect, config).await,
        "listDatabases" => op_list_databases_server(&pool, dialect).await,
        "listTables" => op_list_tables(&pool, dialect).await,
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
        assert_eq!(
            quote_ident(Dialect::Postgres, "users").unwrap(),
            "\"users\""
        );
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
    fn sanitize_ident_fixes_names_without_erroring() {
        // Spaces and symbols become underscores; a leading digit is prefixed.
        assert_eq!(sanitize_ident("user profile"), "user_profile");
        assert_eq!(sanitize_ident("  full name  "), "full_name");
        assert_eq!(sanitize_ident("email@address!"), "email_address_");
        assert_eq!(sanitize_ident("1st_col"), "_1st_col");
        // Valid names pass through completely unchanged (including real underscores).
        assert_eq!(sanitize_ident("first_name"), "first_name");
        assert_eq!(sanitize_ident("schema.my table"), "schema.my_table");
        // Sanitized names sail through the strict quoter that raw ones would fail.
        assert!(quote_ident(Dialect::Postgres, "a b").is_err());
        assert_eq!(
            quote_user_ident(Dialect::Postgres, "a b").unwrap(),
            "\"a_b\""
        );
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
        assert!(
            url.contains("p%40ss%3Aword%2F%21"),
            "password not encoded: {url}"
        );
        assert!(url.contains("@db.example.com:6432/app"));
        assert!(url.ends_with("?sslmode=require"));
    }

    #[test]
    fn connection_string_overrides_discrete_fields() {
        let cfg = json!({ "connection_string": "mysql://u:p@h/db", "host": "ignored" });
        assert_eq!(build_url(Dialect::MySql, &cfg).unwrap(), "mysql://u:p@h/db");
    }

    #[test]
    fn coerce_text_picks_natural_scalar() {
        assert_eq!(coerce_text(Some(&json!("30"))), json!(30));
        assert_eq!(coerce_text(Some(&json!("-5"))), json!(-5));
        assert_eq!(coerce_text(Some(&json!("3.14"))), json!(3.14));
        assert_eq!(coerce_text(Some(&json!("true"))), json!(true));
        assert_eq!(coerce_text(Some(&json!("false"))), json!(false));
        assert_eq!(coerce_text(Some(&json!("null"))), json!(null));
        assert_eq!(coerce_text(Some(&json!("Ann"))), json!("Ann"));
        // Codes that must not silently become numbers stay text.
        assert_eq!(coerce_text(Some(&json!("007"))), json!("007"));
        assert_eq!(
            coerce_text(Some(&json!("+639171234567"))),
            json!("+639171234567")
        );
        // Absent / empty → empty string, not NULL.
        assert_eq!(coerce_text(None), json!(""));
        assert_eq!(coerce_text(Some(&json!("  "))), json!(""));
        // Already-typed values from an upstream expression pass through.
        assert_eq!(coerce_text(Some(&json!(42))), json!(42));
    }

    #[test]
    fn resolve_data_prefers_mode_then_falls_back() {
        // Fields mode: build from the row editor, skipping blank-column rows.
        let cfg = json!({
            "data_mode": "fields",
            "data_fields": { "parameters": [
                { "column": "name", "value": "Ann" },
                { "column": "age", "value": "30" },
                { "column": "", "value": "ignored" },
            ] },
        });
        let m = resolve_data(&cfg).unwrap();
        assert_eq!(m.get("name"), Some(&json!("Ann")));
        assert_eq!(m.get("age"), Some(&json!(30)));
        assert_eq!(m.len(), 2);

        // JSON mode: parse the raw box even if stale rows linger.
        let cfg = json!({
            "data_mode": "json",
            "data": "{\"x\": 1}",
            "data_fields": { "parameters": [{ "column": "y", "value": "2" }] },
        });
        assert_eq!(resolve_data(&cfg).unwrap().get("x"), Some(&json!(1)));

        // Legacy node (no data_mode, only a data string) still parses as JSON.
        let cfg = json!({ "data": "{\"x\": 1}" });
        assert_eq!(resolve_data(&cfg).unwrap().get("x"), Some(&json!(1)));

        // Fields mode with all-blank rows gives a friendly error, not a panic.
        let cfg = json!({ "data_mode": "fields", "data_fields": { "parameters": [{ "column": "", "value": "" }] } });
        assert!(resolve_data(&cfg).is_err());
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

        let base =
            |op: &str| json!({ "db_type": "sqlite", "database": path.clone(), "operation": op });

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

        // INSERT (beginner row editor — plain-text values, age coerced to a number)
        let mut c = base("insert");
        c["table"] = json!("people");
        c["data_mode"] = json!("fields");
        c["data_fields"] = json!({ "parameters": [
            { "column": "name", "value": "Cara" },
            { "column": "age", "value": "20" },
        ] });
        let ins = execute(&c).await.expect("row-editor insert");
        assert_eq!(ins["rows_affected"], 1);

        // The plain-text "20" was coerced to a number and lands in the INTEGER column.
        let mut c = base("select");
        c["table"] = json!("people");
        c["where"] = json!("name = 'Cara'");
        let cara = execute(&c).await.expect("select cara");
        assert_eq!(cara["rows"][0]["age"], 20);

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
        assert!(
            execute(&c).await.is_err(),
            "empty-where delete must be blocked"
        );

        // DELETE with where
        let mut c = base("delete");
        c["table"] = json!("people");
        c["where"] = json!("name = 'Bob'");
        let del = execute(&c).await.expect("delete");
        assert_eq!(del["rows_affected"], 1);

        let _ = std::fs::remove_file(path.replace('/', std::path::MAIN_SEPARATOR_STR));
    }

    #[test]
    fn resolve_sqlite_path_rules() {
        // Bare name → managed dir, .db appended.
        let p = resolve_sqlite_path("sales").unwrap();
        assert_eq!(p.file_name().unwrap(), "sales.db");
        assert_eq!(p.parent().unwrap(), sqlite_dir());
        // Existing extension kept.
        assert_eq!(
            resolve_sqlite_path("x.sqlite")
                .unwrap()
                .file_name()
                .unwrap(),
            "x.sqlite"
        );
        // Explicit path honored as-is.
        assert_eq!(
            resolve_sqlite_path("/tmp/a.db").unwrap(),
            PathBuf::from("/tmp/a.db")
        );
        // Traversal / bad chars rejected.
        assert!(resolve_sqlite_path("../secret").is_err());
        assert!(resolve_sqlite_path("bad name!").is_err());
    }

    #[test]
    fn column_types_map_per_dialect() {
        assert_eq!(map_column_type(Dialect::Postgres, "text").unwrap(), "TEXT");
        assert_eq!(
            map_column_type(Dialect::MySql, "text").unwrap(),
            "VARCHAR(255)"
        );
        assert_eq!(map_column_type(Dialect::MySql, "integer").unwrap(), "INT");
        assert_eq!(
            map_column_type(Dialect::Sqlite, "integer").unwrap(),
            "INTEGER"
        );
        assert_eq!(
            map_column_type(Dialect::Postgres, "number").unwrap(),
            "DOUBLE PRECISION"
        );
        assert_eq!(map_column_type(Dialect::Sqlite, "json").unwrap(), "TEXT");
        assert_eq!(map_column_type(Dialect::Postgres, "json").unwrap(), "JSONB");
        // A safe raw type passes through; an injection attempt is rejected.
        assert_eq!(
            map_column_type(Dialect::MySql, "VARCHAR(100)").unwrap(),
            "VARCHAR(100)"
        );
        assert!(map_column_type(Dialect::Postgres, "TEXT); DROP TABLE x;--").is_err());
    }

    #[test]
    fn default_literals_are_safe() {
        assert_eq!(format_default_literal("0"), "0");
        assert_eq!(format_default_literal("3.5"), "3.5");
        assert_eq!(format_default_literal("true"), "TRUE");
        assert_eq!(
            format_default_literal("CURRENT_TIMESTAMP"),
            "CURRENT_TIMESTAMP"
        );
        assert_eq!(format_default_literal("active"), "'active'");
        // Quotes are doubled so a string default can't break out.
        assert_eq!(format_default_literal("O'Brien"), "'O''Brien'");
    }

    #[test]
    fn column_def_auto_increment_pk() {
        let int_pk = json!({ "name": "id", "type": "integer", "primary_key": true });
        assert_eq!(
            build_column_def(Dialect::Postgres, &int_pk, true).unwrap(),
            "\"id\" SERIAL PRIMARY KEY"
        );
        assert_eq!(
            build_column_def(Dialect::MySql, &int_pk, true).unwrap(),
            "`id` INT AUTO_INCREMENT PRIMARY KEY"
        );
        assert_eq!(
            build_column_def(Dialect::Sqlite, &int_pk, true).unwrap(),
            "\"id\" INTEGER PRIMARY KEY AUTOINCREMENT"
        );
        // NOT NULL / UNIQUE / DEFAULT on a plain column.
        let col = json!({ "name": "email", "type": "text", "not_null": true, "unique": true, "default": "x" });
        assert_eq!(
            build_column_def(Dialect::Postgres, &col, false).unwrap(),
            "\"email\" TEXT NOT NULL UNIQUE DEFAULT 'x'"
        );
    }

    /// Create Table via the row editor, then Describe and Drop it on SQLite.
    #[tokio::test]
    async fn sqlite_create_describe_drop_table() {
        let path = std::env::temp_dir()
            .join(format!("axon_ddl_{}.sqlite", uuid::Uuid::new_v4().simple()))
            .to_string_lossy()
            .replace('\\', "/");
        let base =
            |op: &str| json!({ "db_type": "sqlite", "database": path.clone(), "operation": op });

        // Create a table with an auto-increment id + two columns.
        let mut c = base("createTable");
        c["table"] = json!("contacts");
        c["if_not_exists"] = json!(true);
        c["columns_def"] = json!({ "parameters": [
            { "name": "id", "type": "integer", "primary_key": true },
            { "name": "name", "type": "text", "not_null": true },
            { "name": "score", "type": "number", "default": "0" },
            { "name": "", "type": "text" },
        ] });
        let created = execute(&c).await.expect("createTable");
        assert_eq!(created["created"], true);

        // Re-running with IF NOT EXISTS is a safe no-op (would error otherwise).
        execute(&c).await.expect("createTable again");

        // The auto-increment id fills itself in on insert.
        let mut ins = base("insert");
        ins["table"] = json!("contacts");
        ins["data_mode"] = json!("fields");
        ins["data_fields"] = json!({ "parameters": [{ "column": "name", "value": "Ann" }] });
        execute(&ins).await.expect("insert");

        let mut sel = base("select");
        sel["table"] = json!("contacts");
        let out = execute(&sel).await.expect("select");
        assert_eq!(out["rows"][0]["id"], 1);
        // `score` is a REAL column with DEFAULT 0, so it comes back as 0.0.
        assert_eq!(out["rows"][0]["score"], 0.0);

        // Describe returns one row per column.
        let mut d = base("describeTable");
        d["table"] = json!("contacts");
        let desc = execute(&d).await.expect("describeTable");
        assert_eq!(desc["row_count"], 3);

        // Drop it, then it's gone from listTables.
        let mut dr = base("dropTable");
        dr["table"] = json!("contacts");
        execute(&dr).await.expect("dropTable");
        let t = execute(&base("listTables")).await.expect("listTables");
        assert_eq!(t["table_count"], 0);

        let _ = std::fs::remove_file(path.replace('/', std::path::MAIN_SEPARATOR_STR));
    }

    /// Full lifecycle in the MANAGED dir: create a named database, see it in the
    /// list, create a table, list tables, then drop the database.
    #[tokio::test]
    async fn sqlite_managed_lifecycle() {
        let name = format!("axon_life_{}", uuid::Uuid::new_v4().simple());
        let base =
            |op: &str| json!({ "db_type": "sqlite", "database": name.clone(), "operation": op });

        // Create the database (file in the managed dir).
        let created = execute(&base("createDatabase"))
            .await
            .expect("createDatabase");
        assert_eq!(created["created"], true);
        assert!(resolve_sqlite_path(&name).unwrap().exists());

        // It shows up in listDatabases.
        let list = execute(&base("listDatabases"))
            .await
            .expect("listDatabases");
        let names: Vec<String> = list["databases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .collect();
        assert!(
            names.contains(&format!("{name}.db")),
            "listing missing new db: {names:?}"
        );

        // Empty database → no tables.
        let t0 = execute(&base("listTables"))
            .await
            .expect("listTables empty");
        assert_eq!(t0["table_count"], 0);

        // Create a table, then it appears in listTables.
        let mut c = base("executeQuery");
        c["query"] = json!("CREATE TABLE orders (id INTEGER PRIMARY KEY, total REAL)");
        execute(&c).await.expect("create table");
        let t1 = execute(&base("listTables")).await.expect("listTables");
        assert_eq!(t1["table_count"], 1);
        assert_eq!(t1["tables"][0], "orders");

        // Drop the database file.
        let dropped = execute(&base("dropDatabase")).await.expect("dropDatabase");
        assert_eq!(dropped["dropped"], true);
        assert!(!resolve_sqlite_path(&name).unwrap().exists());
    }
}
