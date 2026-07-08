pub mod contacts;
pub mod datetime;
pub mod notes;
pub mod tasks;
pub mod text;
pub mod web;

use anyhow::Result;
use axon_core::{err_json, ok_json, schema, AppState};
use rmcp::model::{CallToolResult, Tool};
use serde_json::{Map, Value};
use std::sync::Arc;

pub struct BusinessService(pub Arc<AppState>);

impl BusinessService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self(state)
    }

    pub fn tool_list() -> Vec<Tool> {
        vec![
            // ── Notes (local Markdown file store) ─────────────────────────
            Tool::new("note_create", "Create a local Markdown note with a title and content. Returns the note ID.", schema!({"title":{"type":"string"},"content":{"type":"string"},"tags":{"type":"array","items":{"type":"string"}}}, ["title","content"])),
            Tool::new("note_list", "List all local notes (id, title, created, tags).", schema!({"tag":{"type":"string","description":"Filter by tag"}}, [])),
            Tool::new("note_get", "Get a note by ID.", schema!({"id":{"type":"string"}}, ["id"])),
            Tool::new("note_update", "Update a note's title or content.", schema!({"id":{"type":"string"},"title":{"type":"string"},"content":{"type":"string"},"tags":{"type":"array","items":{"type":"string"}}}, ["id"])),
            Tool::new("note_delete", "Delete a note by ID.", schema!({"id":{"type":"string"}}, ["id"])),
            Tool::new("note_search", "Search notes by keyword in title or content.", schema!({"query":{"type":"string"}}, ["query"])),
            Tool::new("note_export", "Export a note as a Markdown string.", schema!({"id":{"type":"string"}}, ["id"])),

            // ── Tasks ─────────────────────────────────────────────────────
            Tool::new("task_create", "Create a local task with title, optional due date and priority.", schema!({"title":{"type":"string"},"description":{"type":"string"},"due_date":{"type":"string","description":"YYYY-MM-DD"},"priority":{"type":"string","enum":["low","medium","high"],"default":"medium"},"tags":{"type":"array","items":{"type":"string"}}}, ["title"])),
            Tool::new("task_list", "List tasks. Filter by status (open|done|all) and/or priority.", schema!({"status":{"type":"string","enum":["open","done","all"],"default":"open"},"priority":{"type":"string"},"tag":{"type":"string"}}, [])),
            Tool::new("task_get", "Get a task by ID.", schema!({"id":{"type":"string"}}, ["id"])),
            Tool::new("task_complete", "Mark a task as completed.", schema!({"id":{"type":"string"}}, ["id"])),
            Tool::new("task_update", "Update a task's fields.", schema!({"id":{"type":"string"},"title":{"type":"string"},"description":{"type":"string"},"due_date":{"type":"string"},"priority":{"type":"string","enum":["low","medium","high"]}}, ["id"])),
            Tool::new("task_delete", "Delete a task by ID.", schema!({"id":{"type":"string"}}, ["id"])),
            Tool::new("task_overdue", "List all overdue (past due date, still open) tasks.", schema!({}, [])),

            // ── Contacts (local address book) ─────────────────────────────
            Tool::new("contact_create", "Add a contact to the local address book.", schema!({"name":{"type":"string"},"email":{"type":"string"},"phone":{"type":"string"},"company":{"type":"string"},"title":{"type":"string"},"notes":{"type":"string"},"tags":{"type":"array","items":{"type":"string"}}}, ["name"])),
            Tool::new("contact_list", "List local contacts.", schema!({"tag":{"type":"string"}}, [])),
            Tool::new("contact_get", "Get a contact by ID.", schema!({"id":{"type":"string"}}, ["id"])),
            Tool::new("contact_search", "Search contacts by name, email, company, or phone.", schema!({"query":{"type":"string"}}, ["query"])),
            Tool::new("contact_update", "Update a contact's details.", schema!({"id":{"type":"string"},"name":{"type":"string"},"email":{"type":"string"},"phone":{"type":"string"},"company":{"type":"string"},"title":{"type":"string"},"notes":{"type":"string"}}, ["id"])),
            Tool::new("contact_delete", "Delete a contact.", schema!({"id":{"type":"string"}}, ["id"])),

            // ── Date / Time ───────────────────────────────────────────────
            Tool::new("datetime_now", "Get the current date and time in a given timezone (e.g. 'Asia/Manila', 'UTC'). Defaults to 'Asia/Manila'.", schema!({"timezone":{"type":"string","default":"Asia/Manila"}}, [])),
            Tool::new("datetime_convert", "Convert a datetime string from one timezone to another.", schema!({"datetime":{"type":"string","description":"ISO 8601"},"from_tz":{"type":"string"},"to_tz":{"type":"string"}}, ["datetime","from_tz","to_tz"])),
            Tool::new("datetime_diff", "Calculate the difference between two ISO 8601 datetimes in days, hours, and minutes.", schema!({"start":{"type":"string"},"end":{"type":"string"}}, ["start","end"])),
            Tool::new("datetime_add", "Add or subtract time from a date. unit: days|hours|minutes|weeks|months.", schema!({"datetime":{"type":"string","description":"ISO 8601 or YYYY-MM-DD"},"amount":{"type":"integer"},"unit":{"type":"string","enum":["minutes","hours","days","weeks","months"]}}, ["datetime","amount","unit"])),
            Tool::new("datetime_format", "Format a datetime string into a human-readable format.", schema!({"datetime":{"type":"string"},"format":{"type":"string","description":"strftime pattern, e.g. %B %d %Y or use 'human'"}}, ["datetime"])),

            // ── Text Utilities ────────────────────────────────────────────
            Tool::new("text_word_count", "Count words, characters, sentences, and paragraphs in text.", schema!({"text":{"type":"string"}}, ["text"])),
            Tool::new("text_summarize_lines", "Extract the first N lines or sentences from text as a quick summary.", schema!({"text":{"type":"string"},"lines":{"type":"integer","default":5}}, ["text"])),
            Tool::new("text_extract_emails", "Extract all email addresses found in a block of text.", schema!({"text":{"type":"string"}}, ["text"])),
            Tool::new("text_extract_urls", "Extract all URLs found in a block of text.", schema!({"text":{"type":"string"}}, ["text"])),
            Tool::new("text_slugify", "Convert text to a URL-safe slug (e.g. 'Hello World!' → 'hello-world').", schema!({"text":{"type":"string"}}, ["text"])),
            Tool::new("text_template", "Fill a template string with key-value substitutions. Template vars use {{key}} syntax.", schema!({"template":{"type":"string"},"vars":{"type":"object","description":"Key-value pairs to substitute"}}, ["template","vars"])),

            // ── Web ───────────────────────────────────────────────────────
            Tool::new("web_request", "Make an HTTP/HTTPS request to an external API or website. Similar to n8n HTTP Request node.", schema!({
                    "url": {"type":"string", "description":"The full URL to request"},
                    "method": {"type":"string", "enum":["GET","POST","PUT","DELETE","PATCH","HEAD"], "default":"GET"},
                    "headers": {"type":"object", "description":"Custom HTTP headers"},
                    "query": {"type":"object", "description":"Query parameters to append to the URL"},
                    "body": {"type":"any", "description":"JSON body for POST/PUT requests"}
                }, ["url"])),
        ]
    }

    pub async fn call(&self, name: &str, args: Map<String, Value>) -> Result<CallToolResult> {
        let a = &args;
        let s = str!(a);
        let n = num!(a);

        let result: Result<Value> = match name {
            "note_create" => {
                notes::create(s("title")?, s("content")?, json_arr_opt(a, "tags")).await
            }
            "note_list" => notes::list(a.get("tag").and_then(|v| v.as_str())).await,
            "note_get" => notes::get(s("id")?).await,
            "note_update" => {
                notes::update(
                    s("id")?,
                    a.get("title").and_then(|v| v.as_str()),
                    a.get("content").and_then(|v| v.as_str()),
                    json_arr_opt(a, "tags"),
                )
                .await
            }
            "note_delete" => notes::delete(s("id")?).await,
            "note_search" => notes::search(s("query")?).await,
            "note_export" => notes::export(s("id")?).await,

            "task_create" => {
                tasks::create(
                    s("title")?,
                    a.get("description").and_then(|v| v.as_str()),
                    a.get("due_date").and_then(|v| v.as_str()),
                    a.get("priority")
                        .and_then(|v| v.as_str())
                        .unwrap_or("medium"),
                    json_arr_opt(a, "tags"),
                )
                .await
            }
            "task_list" => {
                tasks::list(
                    a.get("status").and_then(|v| v.as_str()).unwrap_or("open"),
                    a.get("priority").and_then(|v| v.as_str()),
                    a.get("tag").and_then(|v| v.as_str()),
                )
                .await
            }
            "task_get" => tasks::get(s("id")?).await,
            "task_complete" => tasks::complete(s("id")?).await,
            "task_update" => {
                tasks::update(
                    s("id")?,
                    a.get("title").and_then(|v| v.as_str()),
                    a.get("description").and_then(|v| v.as_str()),
                    a.get("due_date").and_then(|v| v.as_str()),
                    a.get("priority").and_then(|v| v.as_str()),
                )
                .await
            }
            "task_delete" => tasks::delete(s("id")?).await,
            "task_overdue" => tasks::overdue().await,

            "contact_create" => contacts::create(a).await,
            "contact_list" => contacts::list(a.get("tag").and_then(|v| v.as_str())).await,
            "contact_get" => contacts::get(s("id")?).await,
            "contact_search" => contacts::search(s("query")?).await,
            "contact_update" => contacts::update(s("id")?, a).await,
            "contact_delete" => contacts::delete(s("id")?).await,

            "datetime_now" => datetime::now(
                a.get("timezone")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Asia/Manila"),
            ),
            "datetime_convert" => datetime::convert(s("datetime")?, s("from_tz")?, s("to_tz")?),
            "datetime_diff" => datetime::diff(s("start")?, s("end")?),
            "datetime_add" => datetime::add(s("datetime")?, n("amount", 0.0) as i64, s("unit")?),
            "datetime_format" => datetime::format_dt(
                s("datetime")?,
                a.get("format").and_then(|v| v.as_str()).unwrap_or("human"),
            ),

            "text_word_count" => text::word_count(s("text")?),
            "text_summarize_lines" => text::summarize_lines(s("text")?, n("lines", 5.0) as usize),
            "text_extract_emails" => text::extract_emails(s("text")?),
            "text_extract_urls" => text::extract_urls(s("text")?),
            "text_slugify" => text::slugify(s("text")?),
            "text_template" => text::render_template(
                s("template")?,
                a.get("vars")
                    .and_then(|v| v.as_object())
                    .ok_or_else(|| anyhow::anyhow!("missing 'vars' object"))?,
            ),

            "web_request" => {
                web::request(
                    &self.0,
                    s("url")?,
                    a.get("method").and_then(|v| v.as_str()).unwrap_or("GET"),
                    a.get("headers").and_then(|v| v.as_object()),
                    a.get("query").and_then(|v| v.as_object()),
                    a.get("body"),
                )
                .await
            }

            other => Err(anyhow::anyhow!("Unknown Business tool: {other}")),
        };

        Ok(match result {
            Ok(v) => ok_json(v),
            Err(e) => err_json(e),
        })
    }
}

macro_rules! str {
    ($args:expr) => {
        |key: &str| -> Result<&str> {
            $args
                .get(key)
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing required param '{key}'"))
        }
    };
}
macro_rules! num {
    ($args:expr) => {
        |key: &str, default: f64| -> f64 {
            $args.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
        }
    };
}
use num;
use str;

fn json_arr_opt<'a>(args: &'a Map<String, Value>, key: &str) -> Option<Vec<&'a str>> {
    args.get(key)?
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
}
