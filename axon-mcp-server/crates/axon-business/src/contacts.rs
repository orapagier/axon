use anyhow::{Context, Result};
use axon_core::storage::data_dir;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{fs, path::PathBuf};
use uuid::Uuid;

fn contacts_path() -> PathBuf {
    data_dir().join("contacts.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub company: Option<String>,
    pub title: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn load() -> Result<Vec<Contact>> {
    let p = contacts_path();
    if !p.exists() {
        return Ok(vec![]);
    }
    Ok(
        serde_json::from_str(&fs::read_to_string(&p).context("reading contacts.json")?)
            .unwrap_or_default(),
    )
}

fn save(contacts: &[Contact]) -> Result<()> {
    fs::create_dir_all(contacts_path().parent().unwrap())?;
    fs::write(contacts_path(), serde_json::to_string_pretty(contacts)?)
        .context("writing contacts.json")
}

fn str_field<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

pub async fn create(args: &Map<String, Value>) -> Result<Value> {
    let name =
        str_field(args, "name").ok_or_else(|| anyhow::anyhow!("missing required param 'name'"))?;
    let mut contacts = load()?;
    let now = Utc::now().to_rfc3339();
    let c = Contact {
        id: Uuid::new_v4().to_string(),
        name: name.to_owned(),
        email: str_field(args, "email").map(str::to_owned),
        phone: str_field(args, "phone").map(str::to_owned),
        company: str_field(args, "company").map(str::to_owned),
        title: str_field(args, "title").map(str::to_owned),
        notes: str_field(args, "notes").map(str::to_owned),
        tags: args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        created_at: now.clone(),
        updated_at: now,
    };
    let id = c.id.clone();
    contacts.push(c);
    save(&contacts)?;
    Ok(serde_json::json!({ "success": true, "id": id, "name": name }))
}

pub async fn list(tag: Option<&str>) -> Result<Value> {
    let contacts = load()?;
    let items: Vec<Value> = contacts
        .iter()
        .filter(|c| tag.map_or(true, |t| c.tags.iter().any(|ct| ct == t)))
        .map(|c| {
            serde_json::json!({
                "id": c.id, "name": c.name, "email": c.email,
                "phone": c.phone, "company": c.company, "title": c.title, "tags": c.tags,
            })
        })
        .collect();
    Ok(serde_json::json!({ "contacts": items, "count": items.len() }))
}

pub async fn get(id: &str) -> Result<Value> {
    let contacts = load()?;
    contacts
        .iter()
        .find(|c| c.id == id)
        .map(|c| Ok(serde_json::to_value(c)?))
        .unwrap_or_else(|| Err(anyhow::anyhow!("Contact '{id}' not found.")))
}

pub async fn search(query: &str) -> Result<Value> {
    let q = query.to_lowercase();
    let contacts = load()?;
    let results: Vec<Value> = contacts
        .iter()
        .filter(|c| {
            c.name.to_lowercase().contains(&q)
                || c.email.as_deref().unwrap_or("").to_lowercase().contains(&q)
                || c.company
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&q)
                || c.phone.as_deref().unwrap_or("").contains(&q)
        })
        .map(|c| {
            serde_json::json!({
                "id": c.id, "name": c.name, "email": c.email,
                "phone": c.phone, "company": c.company, "title": c.title,
            })
        })
        .collect();
    Ok(serde_json::json!({ "results": results, "count": results.len() }))
}

pub async fn update(id: &str, args: &Map<String, Value>) -> Result<Value> {
    let mut contacts = load()?;
    let c = contacts
        .iter_mut()
        .find(|c| c.id == id)
        .ok_or_else(|| anyhow::anyhow!("Contact '{id}' not found."))?;
    if let Some(v) = str_field(args, "name") {
        c.name = v.to_owned();
    }
    if let Some(v) = str_field(args, "email") {
        c.email = Some(v.to_owned());
    }
    if let Some(v) = str_field(args, "phone") {
        c.phone = Some(v.to_owned());
    }
    if let Some(v) = str_field(args, "company") {
        c.company = Some(v.to_owned());
    }
    if let Some(v) = str_field(args, "title") {
        c.title = Some(v.to_owned());
    }
    if let Some(v) = str_field(args, "notes") {
        c.notes = Some(v.to_owned());
    }
    c.updated_at = Utc::now().to_rfc3339();
    save(&contacts)?;
    Ok(serde_json::json!({ "success": true, "id": id }))
}

pub async fn delete(id: &str) -> Result<Value> {
    let mut contacts = load()?;
    let before = contacts.len();
    contacts.retain(|c| c.id != id);
    if contacts.len() == before {
        return Err(anyhow::anyhow!("Contact '{id}' not found."));
    }
    save(&contacts)?;
    Ok(serde_json::json!({ "success": true, "deleted_id": id }))
}
