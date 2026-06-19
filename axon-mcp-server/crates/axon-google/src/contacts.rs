use crate::auth::access_token;
use anyhow::Result;
use axon_core::AppState;
use serde_json::{json, Value};

const BASE: &str = "https://people.googleapis.com/v1";

// ── Contacts ──────────────────────────────────────────────────────────────────

/// List the user's contacts (connections).
pub async fn list_contacts(state: &AppState, max_results: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/people/me/connections"))
        .bearer_auth(&tok)
        .query(&[
            ("pageSize", max_results.to_string()),
            (
                "personFields",
                "names,emailAddresses,phoneNumbers,photos,organizations".to_string(),
            ),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Get a single contact by its resource name (e.g. "people/c12345").
pub async fn get_contact(state: &AppState, name: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/{name}"))
        .bearer_auth(&tok)
        .query(&[(
            "personFields",
            "names,emailAddresses,phoneNumbers,photos,organizations,metadata",
        )])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Create a new contact.
pub async fn create_contact(
    state: &AppState,
    given_name: &str,
    family_name: Option<&str>,
    email: Option<&str>,
    phone: Option<&str>,
    notes: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;

    let mut person = json!({
        "names": [{
            "givenName": given_name,
        }]
    });

    if let Some(fam) = family_name {
        person["names"][0]["familyName"] = json!(fam);
    }
    if let Some(e) = email {
        person["emailAddresses"] = json!([{"value": e}]);
    }
    if let Some(p) = phone {
        person["phoneNumbers"] = json!([{"value": p}]);
    }
    if let Some(n) = notes {
        person["biographies"] = json!([{"value": n}]);
    }

    let resp: Value = state
        .client
        .post(format!("{BASE}/people:createContact"))
        .bearer_auth(&tok)
        .json(&person)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Update an existing contact.
pub async fn update_contact(
    state: &AppState,
    name: &str,
    given_name: Option<&str>,
    family_name: Option<&str>,
    email: Option<&str>,
    phone: Option<&str>,
    notes: Option<&str>,
) -> Result<Value> {
    let tok = access_token(state).await?;

    // First, we need to get the etag for the person
    let current: Value = get_contact(state, name).await?;
    let etag = current["etag"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing etag for contact update"))?;

    let mut person = json!({
        "etag": etag,
    });

    let mut update_fields = Vec::new();

    if given_name.is_some() || family_name.is_some() {
        // Always fall back to the existing value for whichever name part was NOT provided.
        // Without this, passing `family_name: None` into json!() serialises as JSON null
        // and the API will silently erase the existing family name (and vice-versa).
        let current_gn = current["names"][0]["givenName"].as_str().unwrap_or("");
        let current_fn = current["names"][0]["familyName"].as_str().unwrap_or("");
        person["names"] = json!([{
            "givenName":  given_name.unwrap_or(current_gn),
            "familyName": family_name.unwrap_or(current_fn),
        }]);
        update_fields.push("names");
    }

    if let Some(e) = email {
        person["emailAddresses"] = json!([{"value": e}]);
        update_fields.push("emailAddresses");
    }
    if let Some(p) = phone {
        person["phoneNumbers"] = json!([{"value": p}]);
        update_fields.push("phoneNumbers");
    }
    if let Some(n) = notes {
        person["biographies"] = json!([{"value": n}]);
        update_fields.push("biographies");
    }

    if update_fields.is_empty() {
        return Ok(current);
    }

    let fields_str = update_fields.join(",");
    let resp: Value = state
        .client
        .patch(format!("{BASE}/{name}:updateContact"))
        .bearer_auth(&tok)
        .query(&[("updatePersonFields", fields_str)])
        .json(&person)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Search contacts by name, email, or phone number.
/// Returns the top `max_results` matches (default: 10).
/// Use this to resolve a human-readable name to a resource name (e.g. "people/c12345")
/// before calling get_contact, update_contact, or delete_contact.
pub async fn search_contacts(state: &AppState, query: &str, max_results: u32) -> Result<Value> {
    let tok = access_token(state).await?;
    let resp: Value = state
        .client
        .get(format!("{BASE}/people:searchContacts"))
        .bearer_auth(&tok)
        .query(&[
            ("query", query.to_string()),
            ("pageSize", max_results.to_string()),
            (
                "readMask",
                "names,emailAddresses,phoneNumbers,photos,organizations".to_string(),
            ),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp)
}

/// Delete a contact.
pub async fn delete_contact(state: &AppState, name: &str) -> Result<Value> {
    let tok = access_token(state).await?;
    state
        .client
        .delete(format!("{BASE}/{name}:deleteContact"))
        .bearer_auth(&tok)
        .send()
        .await?
        .error_for_status()?;
    Ok(json!({ "success": true, "resourceName": name }))
}
