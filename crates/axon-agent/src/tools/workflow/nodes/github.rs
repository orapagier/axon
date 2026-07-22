//! GitHub workflow action node.
//!
//! Calls GitHub's REST API (https://api.github.com) with a Personal Access
//! Token. The token arrives in `config` as `access_token` (or `token`) after
//! `interpolate_config` merges the selected credential (service "github") in —
//! exactly the same path the Slack/Discord nodes use for their bot tokens.
//!
//! Auth is just `Authorization: Bearer <PAT>`, which works for both classic and
//! fine-grained tokens. GitHub additionally REQUIRES a `User-Agent` header and
//! rejects requests that omit it (HTTP 403), so every request sets one.
//!
//! Unlike Slack (which returns HTTP 200 with `ok:false` on failure), GitHub uses
//! standard HTTP status codes, so success is decided by the status. The error
//! body — usually `{"message": ..., "errors": [...]}` — is surfaced verbatim so
//! the editor shows GitHub's own reason.

use crate::tools::workflow::str_val;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

const API_BASE: &str = "https://api.github.com";
const UA: &str = "Axon-Workflow";
const API_VERSION: &str = "2022-11-28";

// ── Config helpers ──────────────────────────────────────────────────────────

fn require(config: &Value, key: &str) -> Result<String, String> {
    match str_val(config, key) {
        Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        _ => Err(format!("Missing required field '{key}' in GitHub config")),
    }
}

/// The owner/repo pair almost every operation needs.
fn owner_repo(config: &Value) -> Result<(String, String), String> {
    Ok((require(config, "owner")?, require(config, "repo")?))
}

/// Split a comma-separated field (labels, assignees) into a trimmed list,
/// dropping empties. Returns None when the field is absent/blank.
fn opt_csv(config: &Value, key: &str) -> Option<Vec<String>> {
    str_val(config, key).and_then(|s| {
        let v: Vec<String> = s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
        (!v.is_empty()).then_some(v)
    })
}

// ── Request plumbing ─────────────────────────────────────────────────────────

enum Method {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

async fn call(
    client: &reqwest::Client,
    token: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let url = format!("{API_BASE}{path}");
    let mut req = match method {
        Method::Get => client.get(&url),
        Method::Post => client.post(&url),
        Method::Patch => client.patch(&url),
        Method::Put => client.put(&url),
        Method::Delete => client.delete(&url),
    };
    req = req
        .bearer_auth(token)
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", API_VERSION);
    if let Some(b) = body {
        req = req.json(&b);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("GitHub request error: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("GitHub: failed to read response: {e}"))?;

    // A successful DELETE / merge can return 204 with an empty body.
    let parsed: Value = if text.trim().is_empty() {
        json!({ "success": true, "status": status.as_u16() })
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }))
    };

    if !status.is_success() {
        let msg = parsed
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("request failed");
        return Err(format!("GitHub API error {status}: {msg}"));
    }
    Ok(parsed)
}

// ── Issues ───────────────────────────────────────────────────────────────────

async fn create_issue(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let mut body = serde_json::Map::new();
    body.insert("title".into(), json!(require(cfg, "title")?));
    if let Some(b) = str_val(cfg, "body") {
        body.insert("body".into(), json!(b));
    }
    if let Some(labels) = opt_csv(cfg, "labels") {
        body.insert("labels".into(), json!(labels));
    }
    if let Some(assignees) = opt_csv(cfg, "assignees") {
        body.insert("assignees".into(), json!(assignees));
    }
    call(
        c,
        t,
        Method::Post,
        &format!("/repos/{owner}/{repo}/issues"),
        Some(Value::Object(body)),
    )
    .await
}

async fn get_issue(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let n = require(cfg, "issue_number")?;
    call(
        c,
        t,
        Method::Get,
        &format!("/repos/{owner}/{repo}/issues/{n}"),
        None,
    )
    .await
}

async fn update_issue(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let n = require(cfg, "issue_number")?;
    let mut body = serde_json::Map::new();
    if let Some(v) = str_val(cfg, "title") {
        body.insert("title".into(), json!(v));
    }
    if let Some(v) = str_val(cfg, "body") {
        body.insert("body".into(), json!(v));
    }
    // `state` toggles open/closed (the common "close issue" use).
    if let Some(v) = str_val(cfg, "state").filter(|s| !s.is_empty()) {
        body.insert("state".into(), json!(v));
    }
    if let Some(labels) = opt_csv(cfg, "labels") {
        body.insert("labels".into(), json!(labels));
    }
    if body.is_empty() {
        return Err("Update Issue: set at least one of title / body / state / labels".into());
    }
    call(
        c,
        t,
        Method::Patch,
        &format!("/repos/{owner}/{repo}/issues/{n}"),
        Some(Value::Object(body)),
    )
    .await
}

async fn create_comment(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let n = require(cfg, "issue_number")?;
    let body = json!({ "body": require(cfg, "body")? });
    call(
        c,
        t,
        Method::Post,
        &format!("/repos/{owner}/{repo}/issues/{n}/comments"),
        Some(body),
    )
    .await
}

async fn list_issues(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let state = str_val(cfg, "list_state").unwrap_or_else(|| "open".into());
    call(
        c,
        t,
        Method::Get,
        &format!("/repos/{owner}/{repo}/issues?state={state}&per_page=50"),
        None,
    )
    .await
}

// ── Pull Requests ─────────────────────────────────────────────────────────────

async fn create_pr(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let mut body = serde_json::Map::new();
    body.insert("title".into(), json!(require(cfg, "title")?));
    body.insert("head".into(), json!(require(cfg, "head")?));
    body.insert("base".into(), json!(require(cfg, "base")?));
    if let Some(b) = str_val(cfg, "body") {
        body.insert("body".into(), json!(b));
    }
    call(
        c,
        t,
        Method::Post,
        &format!("/repos/{owner}/{repo}/pulls"),
        Some(Value::Object(body)),
    )
    .await
}

async fn get_pr(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let n = require(cfg, "pull_number")?;
    call(
        c,
        t,
        Method::Get,
        &format!("/repos/{owner}/{repo}/pulls/{n}"),
        None,
    )
    .await
}

async fn list_prs(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let state = str_val(cfg, "list_state").unwrap_or_else(|| "open".into());
    call(
        c,
        t,
        Method::Get,
        &format!("/repos/{owner}/{repo}/pulls?state={state}&per_page=50"),
        None,
    )
    .await
}

async fn merge_pr(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let n = require(cfg, "pull_number")?;
    let mut body = serde_json::Map::new();
    // merge / squash / rebase — default to a plain merge commit.
    body.insert(
        "merge_method".into(),
        json!(str_val(cfg, "merge_method").unwrap_or_else(|| "merge".into())),
    );
    if let Some(v) = str_val(cfg, "commit_title") {
        body.insert("commit_title".into(), json!(v));
    }
    call(
        c,
        t,
        Method::Put,
        &format!("/repos/{owner}/{repo}/pulls/{n}/merge"),
        Some(Value::Object(body)),
    )
    .await
}

// ── File contents ─────────────────────────────────────────────────────────────

async fn get_file(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let path = require(cfg, "path")?;
    let mut url = format!("/repos/{owner}/{repo}/contents/{path}");
    if let Some(r) = str_val(cfg, "ref").filter(|s| !s.is_empty()) {
        url.push_str(&format!("?ref={r}"));
    }
    let mut resp = call(c, t, Method::Get, &url, None).await?;
    // Decode the base64 blob into `decoded_content` for convenience (GitHub
    // returns file bodies base64-encoded with embedded newlines).
    if let Some(b64) = resp.get("content").and_then(|v| v.as_str()) {
        let cleaned: String = b64.split_whitespace().collect();
        if let Ok(bytes) = STANDARD.decode(cleaned) {
            if let Ok(text) = String::from_utf8(bytes) {
                if let Value::Object(ref mut m) = resp {
                    m.insert("decoded_content".into(), json!(text));
                }
            }
        }
    }
    Ok(resp)
}

async fn put_file(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let path = require(cfg, "path")?;
    let content = require(cfg, "content")?;
    let mut body = serde_json::Map::new();
    body.insert("message".into(), json!(require(cfg, "message")?));
    body.insert("content".into(), json!(STANDARD.encode(content.as_bytes())));
    if let Some(branch) = str_val(cfg, "branch").filter(|s| !s.is_empty()) {
        body.insert("branch".into(), json!(branch));
    }
    // `sha` is REQUIRED by GitHub to UPDATE an existing file; omit it to create.
    if let Some(sha) = str_val(cfg, "sha").filter(|s| !s.is_empty()) {
        body.insert("sha".into(), json!(sha));
    }
    call(
        c,
        t,
        Method::Put,
        &format!("/repos/{owner}/{repo}/contents/{path}"),
        Some(Value::Object(body)),
    )
    .await
}

async fn delete_file(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    let path = require(cfg, "path")?;
    let mut body = serde_json::Map::new();
    body.insert("message".into(), json!(require(cfg, "message")?));
    // Deleting a file requires its current blob sha.
    body.insert("sha".into(), json!(require(cfg, "sha")?));
    if let Some(branch) = str_val(cfg, "branch").filter(|s| !s.is_empty()) {
        body.insert("branch".into(), json!(branch));
    }
    call(
        c,
        t,
        Method::Delete,
        &format!("/repos/{owner}/{repo}/contents/{path}"),
        Some(Value::Object(body)),
    )
    .await
}

// ── Repository ────────────────────────────────────────────────────────────────

async fn get_repo(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    call(c, t, Method::Get, &format!("/repos/{owner}/{repo}"), None).await
}

async fn list_branches(c: &reqwest::Client, t: &str, cfg: &Value) -> Result<Value, String> {
    let (owner, repo) = owner_repo(cfg)?;
    call(
        c,
        t,
        Method::Get,
        &format!("/repos/{owner}/{repo}/branches?per_page=100"),
        None,
    )
    .await
}

// ── Public executor ───────────────────────────────────────────────────────────

pub(crate) async fn execute(config: &Value) -> Result<Value, String> {
    let token = str_val(config, "access_token")
        .or_else(|| str_val(config, "token"))
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| {
            "Missing GitHub token. Add a credential (service 'github') with an \
             'access_token' field holding a Personal Access Token."
                .to_string()
        })?;
    let token = token.trim().to_string();

    let client = crate::http::shared();
    let operation = str_val(config, "operation").unwrap_or_else(|| "createIssue".to_string());

    match operation.as_str() {
        "createIssue" => create_issue(&client, &token, config).await,
        "getIssue" => get_issue(&client, &token, config).await,
        "updateIssue" => update_issue(&client, &token, config).await,
        "createComment" => create_comment(&client, &token, config).await,
        "listIssues" => list_issues(&client, &token, config).await,
        "createPullRequest" => create_pr(&client, &token, config).await,
        "getPullRequest" => get_pr(&client, &token, config).await,
        "listPullRequests" => list_prs(&client, &token, config).await,
        "mergePullRequest" => merge_pr(&client, &token, config).await,
        "getFile" => get_file(&client, &token, config).await,
        "createOrUpdateFile" => put_file(&client, &token, config).await,
        "deleteFile" => delete_file(&client, &token, config).await,
        "getRepo" => get_repo(&client, &token, config).await,
        "listBranches" => list_branches(&client, &token, config).await,
        other => Err(format!("Unsupported GitHub operation '{other}'")),
    }
}
