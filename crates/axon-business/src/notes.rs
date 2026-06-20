use anyhow::{Context, Result};
use axon_core::storage::data_dir;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
use uuid::Uuid;

fn notes_path() -> PathBuf {
    data_dir().join("notes.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn load() -> Result<Vec<Note>> {
    let p = notes_path();
    if !p.exists() {
        return Ok(vec![]);
    }
    let raw = fs::read_to_string(&p).context("reading notes.json")?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn save(notes: &[Note]) -> Result<()> {
    fs::create_dir_all(notes_path().parent().unwrap())?;
    fs::write(notes_path(), serde_json::to_string_pretty(notes)?).context("writing notes.json")
}

pub async fn create(title: &str, content: &str, tags: Option<Vec<&str>>) -> Result<Value> {
    let mut notes = load()?;
    let now = Utc::now().to_rfc3339();
    let note = Note {
        id: Uuid::new_v4().to_string(),
        title: title.to_owned(),
        content: content.to_owned(),
        tags: tags
            .unwrap_or_default()
            .iter()
            .map(|s| s.to_string())
            .collect(),
        created_at: now.clone(),
        updated_at: now,
    };
    let id = note.id.clone();
    notes.push(note);
    save(&notes)?;
    Ok(json!({ "success": true, "id": id, "message": format!("Note '{}' created.", title) }))
}

pub async fn list(tag: Option<&str>) -> Result<Value> {
    let notes = load()?;
    let filtered: Vec<Value> = notes
        .iter()
        .filter(|n| tag.map_or(true, |t| n.tags.iter().any(|nt| nt == t)))
        .map(|n| {
            json!({
                "id": n.id, "title": n.title,
                "tags": n.tags, "created_at": n.created_at, "updated_at": n.updated_at,
                "preview": n.content.chars().take(100).collect::<String>(),
            })
        })
        .collect();
    Ok(json!({ "notes": filtered, "count": filtered.len() }))
}

pub async fn get(id: &str) -> Result<Value> {
    let notes = load()?;
    notes
        .iter()
        .find(|n| n.id == id)
        .map(|n| Ok(serde_json::to_value(n)?))
        .unwrap_or_else(|| Err(anyhow::anyhow!("Note '{id}' not found.")))
}

pub async fn update(
    id: &str,
    title: Option<&str>,
    content: Option<&str>,
    tags: Option<Vec<&str>>,
) -> Result<Value> {
    let mut notes = load()?;
    let note = notes
        .iter_mut()
        .find(|n| n.id == id)
        .ok_or_else(|| anyhow::anyhow!("Note '{id}' not found."))?;
    if let Some(t) = title {
        note.title = t.to_owned();
    }
    if let Some(c) = content {
        note.content = c.to_owned();
    }
    if let Some(t) = tags {
        note.tags = t.iter().map(|s| s.to_string()).collect();
    }
    note.updated_at = Utc::now().to_rfc3339();
    save(&notes)?;
    Ok(json!({ "success": true, "id": id }))
}

pub async fn delete(id: &str) -> Result<Value> {
    let mut notes = load()?;
    let before = notes.len();
    notes.retain(|n| n.id != id);
    if notes.len() == before {
        return Err(anyhow::anyhow!("Note '{id}' not found."));
    }
    save(&notes)?;
    Ok(json!({ "success": true, "deleted_id": id }))
}

pub async fn search(query: &str) -> Result<Value> {
    let q = query.to_lowercase();
    let notes = load()?;
    let results: Vec<Value> = notes
        .iter()
        .filter(|n| n.title.to_lowercase().contains(&q) || n.content.to_lowercase().contains(&q))
        .map(|n| {
            json!({
                "id": n.id, "title": n.title,
                "tags": n.tags, "updated_at": n.updated_at,
                "preview": n.content.chars().take(150).collect::<String>(),
            })
        })
        .collect();
    Ok(json!({ "results": results, "count": results.len(), "query": query }))
}

pub async fn export(id: &str) -> Result<Value> {
    let notes = load()?;
    let note = notes
        .iter()
        .find(|n| n.id == id)
        .ok_or_else(|| anyhow::anyhow!("Note '{id}' not found."))?;
    let tags_line = if note.tags.is_empty() {
        String::new()
    } else {
        format!("\ntags: {}", note.tags.join(", "))
    };
    let md = format!(
        "# {}\n\n_Created: {} | Updated: {}{}_\n\n---\n\n{}",
        note.title, note.created_at, note.updated_at, tags_line, note.content
    );
    Ok(json!({ "markdown": md }))
}
