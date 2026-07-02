use super::*;

pub async fn get_credentials(State(state): State<AppState>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let mut s = try_json!(
            conn.prepare("SELECT id, name, service FROM credentials ORDER BY created_at")
        );
        let creds: Vec<Value> = try_json!(s.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "service": r.get::<_, String>(2)?,
                "has_data": true
            }))
        }))
        .filter_map(|r| r.ok())
        .collect();
        return Json(json!({ "credentials": creds }));
    }
    Json(json!({ "credentials": [] }))
}

pub async fn upsert_credential(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| "");
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unnamed Credential");
    let service = payload
        .get("service")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let data = payload
        .get("data")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let id = if id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        id.to_string()
    };

    let data_str = serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_string());
    // Encrypt the secret blob at rest; the read seams decrypt symmetrically.
    let data_str = crate::crypto::encrypt_key(&data_str);

    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO credentials (id, name, service, data, created_at) VALUES (?1, ?2, ?3, ?4, datetime('now'))",
            rusqlite::params![id, name, service, data_str],
        );
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB Error"}))
}

pub async fn delete_credential(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "DELETE FROM credentials WHERE id = ?1",
            rusqlite::params![id],
        );
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB Error"}))
}

/// D1: cheap, service-specific credential validity check. Never returns secret
/// material — only `{ok, tested, message|error}`. For services without a known
/// probe it reports the credential is present/well-formed but `tested:false`,
/// rather than pretending to have validated it.
pub async fn test_credential(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    let loaded = state.db.get().ok().and_then(|conn| {
        conn.query_row(
            "SELECT service, data FROM credentials WHERE id = ?1",
            [id.as_str()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok()
    });
    let (service, data_str) = match loaded {
        Some(v) => v,
        None => {
            return Json(json!({"ok": false, "tested": false, "error": "Credential not found"}))
        }
    };

    // Blob is encrypted at rest; decrypt_key passes legacy plaintext through.
    let data_str = crate::crypto::decrypt_key(&data_str);
    let data: Value = serde_json::from_str(&data_str).unwrap_or_else(|_| json!({}));
    // First non-empty string field among the given keys.
    let pick = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| {
            data.get(*k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    match service.to_lowercase().as_str() {
        "facebook" | "instagram" => match pick(&["page_access_token", "access_token", "token"]) {
            Some(tok) => {
                probe_credential(
                    &client,
                    "https://graph.facebook.com/v19.0/me?fields=id,name",
                    &[("Authorization".into(), format!("Bearer {tok}"))],
                )
                .await
            }
            None => {
                Json(json!({"ok": false, "tested": true, "error": "No access token in credential"}))
            }
        },
        "google" | "gmail" | "gsheets" | "google_sheets" | "google_drive" => {
            match pick(&["access_token", "token", "api_key"]) {
                Some(tok) => {
                    probe_credential(
                        &client,
                        &format!(
                            "https://www.googleapis.com/oauth2/v3/tokeninfo?access_token={}",
                            urlencoding::encode(&tok)
                        ),
                        &[],
                    )
                    .await
                }
                None => Json(
                    json!({"ok": false, "tested": true, "error": "No access token in credential"}),
                ),
            }
        }
        _ => {
            // Generic: a bearer token against an explicit `test_url`, otherwise
            // report present-but-untested honestly.
            match (
                pick(&["test_url"]),
                pick(&["api_key", "token", "bearer_token", "access_token"]),
            ) {
                (Some(url), Some(tok)) => {
                    probe_credential(
                        &client,
                        &url,
                        &[("Authorization".into(), format!("Bearer {tok}"))],
                    )
                    .await
                }
                _ => {
                    let has_data = data.as_object().map(|o| !o.is_empty()).unwrap_or(false);
                    if has_data {
                        Json(json!({
                            "ok": true, "tested": false,
                            "message": format!("No automated test for service '{service}'. Credential is present and well-formed.")
                        }))
                    } else {
                        Json(
                            json!({"ok": false, "tested": false, "error": "Credential has no data"}),
                        )
                    }
                }
            }
        }
    }
}

/// Issue a cheap GET and map the HTTP status to a validity verdict.
async fn probe_credential(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
) -> Json<Value> {
    let mut req = client.get(url);
    for (k, v) in headers {
        req = req.header(k.as_str(), v);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                Json(
                    json!({"ok": true, "tested": true, "message": format!("Credential is valid (HTTP {})", status.as_u16())}),
                )
            } else {
                Json(
                    json!({"ok": false, "tested": true, "error": format!("Service rejected the credential (HTTP {})", status.as_u16())}),
                )
            }
        }
        Err(e) => {
            Json(json!({"ok": false, "tested": true, "error": format!("Request failed: {e}")}))
        }
    }
}
