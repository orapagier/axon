use anyhow::{Context, Result};
use axon_core::storage::data_dir;
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
use uuid::Uuid;

fn tasks_path() -> PathBuf {
    data_dir().join("tasks.json")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    Medium,
    High,
}

impl Priority {
    fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "low" => Self::Low,
            "high" => Self::High,
            _ => Self::Medium,
        }
    }
    fn order(&self) -> u8 {
        match self {
            Self::High => 0,
            Self::Medium => 1,
            Self::Low => 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Priority,
    pub tags: Vec<String>,
    pub due_date: Option<String>,
    pub done: bool,
    pub created_at: String,
    pub updated_at: String,
    pub done_at: Option<String>,
}

fn load() -> Result<Vec<Task>> {
    let p = tasks_path();
    if !p.exists() {
        return Ok(vec![]);
    }
    let raw = fs::read_to_string(&p).context("reading tasks.json")?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn save(tasks: &[Task]) -> Result<()> {
    fs::create_dir_all(tasks_path().parent().unwrap())?;
    fs::write(tasks_path(), serde_json::to_string_pretty(tasks)?).context("writing tasks.json")
}

pub async fn create(
    title: &str,
    description: Option<&str>,
    due_date: Option<&str>,
    priority: &str,
    tags: Option<Vec<&str>>,
) -> Result<Value> {
    let mut tasks = load()?;
    let now = Utc::now().to_rfc3339();
    let task = Task {
        id: Uuid::new_v4().to_string(),
        title: title.to_owned(),
        description: description.map(str::to_owned),
        priority: Priority::from_str(priority),
        tags: tags
            .unwrap_or_default()
            .iter()
            .map(|s| s.to_string())
            .collect(),
        due_date: due_date.map(str::to_owned),
        done: false,
        created_at: now.clone(),
        updated_at: now,
        done_at: None,
    };
    let id = task.id.clone();
    tasks.push(task);
    save(&tasks)?;
    Ok(json!({ "success": true, "id": id }))
}

pub async fn list(status: &str, priority: Option<&str>, tag: Option<&str>) -> Result<Value> {
    let mut tasks = load()?;
    tasks.sort_by_key(|t| (t.priority.order(), t.due_date.clone().unwrap_or_default()));

    let filtered: Vec<Value> = tasks
        .iter()
        .filter(|t| match status {
            "open" => !t.done,
            "done" => t.done,
            _ => true,
        })
        .filter(|t| {
            priority.map_or(true, |p| {
                serde_json::to_string(&t.priority)
                    .ok()
                    .map_or(false, |s| s.contains(p))
            })
        })
        .filter(|t| tag.map_or(true, |tg| t.tags.iter().any(|tt| tt == tg)))
        .map(|t| {
            json!({
                "id": t.id, "title": t.title, "priority": t.priority,
                "due_date": t.due_date, "done": t.done,
                "tags": t.tags, "description": t.description,
            })
        })
        .collect();

    Ok(json!({ "tasks": filtered, "count": filtered.len() }))
}

pub async fn get(id: &str) -> Result<Value> {
    let tasks = load()?;
    tasks
        .iter()
        .find(|t| t.id == id)
        .map(|t| Ok(serde_json::to_value(t)?))
        .unwrap_or_else(|| Err(anyhow::anyhow!("Task '{id}' not found.")))
}

pub async fn complete(id: &str) -> Result<Value> {
    let mut tasks = load()?;
    let task = tasks
        .iter_mut()
        .find(|t| t.id == id)
        .ok_or_else(|| anyhow::anyhow!("Task '{id}' not found."))?;
    task.done = true;
    task.done_at = Some(Utc::now().to_rfc3339());
    task.updated_at = Utc::now().to_rfc3339();
    let title = task.title.clone();
    save(&tasks)?;
    Ok(json!({ "success": true, "id": id, "message": format!("Task '{}' marked done.", title) }))
}

pub async fn update(
    id: &str,
    title: Option<&str>,
    description: Option<&str>,
    due_date: Option<&str>,
    priority: Option<&str>,
) -> Result<Value> {
    let mut tasks = load()?;
    let task = tasks
        .iter_mut()
        .find(|t| t.id == id)
        .ok_or_else(|| anyhow::anyhow!("Task '{id}' not found."))?;
    if let Some(t) = title {
        task.title = t.to_owned();
    }
    if let Some(d) = description {
        task.description = Some(d.to_owned());
    }
    if let Some(dd) = due_date {
        task.due_date = Some(dd.to_owned());
    }
    if let Some(p) = priority {
        task.priority = Priority::from_str(p);
    }
    task.updated_at = Utc::now().to_rfc3339();
    save(&tasks)?;
    Ok(json!({ "success": true, "id": id }))
}

pub async fn delete(id: &str) -> Result<Value> {
    let mut tasks = load()?;
    let before = tasks.len();
    tasks.retain(|t| t.id != id);
    if tasks.len() == before {
        return Err(anyhow::anyhow!("Task '{id}' not found."));
    }
    save(&tasks)?;
    Ok(json!({ "success": true, "deleted_id": id }))
}

pub async fn overdue() -> Result<Value> {
    let tasks = load()?;
    // Use Asia/Manila (+8:00) for "today" to align with user's local day
    let today = Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive();
    let result: Vec<Value> = tasks
        .iter()
        .filter(|t| !t.done)
        .filter(|t| {
            t.due_date.as_ref().map_or(false, |d| {
                NaiveDate::parse_from_str(d, "%Y-%m-%d").map_or(false, |nd| nd < today)
            })
        })
        .map(|t| {
            json!({
                "id": t.id, "title": t.title, "due_date": t.due_date, "priority": t.priority,
            })
        })
        .collect();
    Ok(json!({ "overdue_tasks": result, "count": result.len() }))
}
