use anyhow::Result;
use axon_core::{oauth, AppState};
use serde_json::{json, Value};

const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.modify",
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/drive",
    "https://www.googleapis.com/auth/documents",
    "https://www.googleapis.com/auth/spreadsheets",
    "https://www.googleapis.com/auth/contacts",
    "https://www.googleapis.com/auth/youtube",
    "https://www.googleapis.com/auth/youtube.force-ssl",
    "https://www.googleapis.com/auth/youtube.upload",
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
];

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Build and return the Google OAuth URL.
pub async fn auth_url(state: &AppState) -> Result<Value> {
    let storage = state.storage.read().await;
    let creds = storage.google_creds()?;

    let scope = SCOPES.join(" ");
    let redir_uri = oauth::callback_uri("google");

    // login_hint pre-selects the desired account on Google's consent screen
    let login_hint = std::env::var("GOOGLE_LOGIN_HINT").unwrap_or_default();
    let hint_param = if login_hint.is_empty() {
        String::new()
    } else {
        format!("&login_hint={}", urlenc(&login_hint))
    };

    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?\
         client_id={}&redirect_uri={}&response_type=code&\
         scope={}&access_type=offline&prompt=consent{}",
        urlenc(&creds.client_id),
        urlenc(&redir_uri),
        urlenc(&scope),
        hint_param,
    );
    Ok(json!({
        "login_url": url,
        "instructions": "Click the login_url above to sign in with Google. Your Axon server will automatically receive the tokens once you complete the sign-in."
    }))
}

/// Exchange code → tokens.
pub async fn exchange_code(state: &AppState, code: &str) -> Result<Value> {
    let (client_id, client_secret) = {
        let storage = state.storage.read().await;
        let creds = storage.google_creds()?;
        (creds.client_id.clone(), creds.client_secret.clone())
    };

    let token = oauth::exchange_code(
        &state.client,
        TOKEN_URL,
        &client_id,
        &client_secret,
        &oauth::callback_uri("google"),
        code,
        &[],
    )
    .await?;

    state.storage.write().await.set_google_token(token)?;
    Ok(json!({ "success": true, "message": "Google authenticated!" }))
}

pub async fn auth_status(state: &AppState) -> Result<Value> {
    let _ = state.storage.write().await.reload_tokens();
    let storage = state.storage.read().await;
    match &storage.tokens.google {
        None => Ok(json!({ "authenticated": false })),
        Some(t) => Ok(json!({
            "authenticated": true,
            "expired": t.is_expired(),
            "expires_at": t.expires_at,
        })),
    }
}

pub async fn revoke(state: &AppState) -> Result<Value> {
    let token = {
        let s = state.storage.read().await;
        s.tokens.google.as_ref().map(|t| t.access_token.clone())
    };
    if let Some(t) = token {
        let _ = state
            .client
            .post(format!("https://oauth2.googleapis.com/revoke?token={t}"))
            .send()
            .await;
    }
    state.storage.write().await.tokens.google = None;
    state.storage.read().await.save_tokens()?;
    Ok(json!({ "success": true, "message": "Google tokens revoked." }))
}

/// Ensure a valid access token, refreshing if necessary.
pub async fn access_token(state: &AppState) -> Result<String> {
    let _ = state.storage.write().await.reload_tokens();
    // Fast-path: not expired
    {
        let storage = state.storage.read().await;
        let tok = storage.tokens.google.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Google not authenticated. Call google_auth_url → sign in → google_exchange_code."
            )
        })?;
        if !tok.is_expired() {
            return Ok(tok.access_token.clone());
        }
    }

    // Refresh
    let (client_id, client_secret, refresh) = {
        let storage = state.storage.read().await;
        let creds = storage.google_creds()?;
        // Re-check under this lock: the fast-path's guard was released, and a
        // concurrent revoke/reload can have cleared the token in between.
        let tok = storage.tokens.google.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Google token removed while refreshing. Re-authenticate Google.")
        })?;
        let refresh = tok
            .refresh_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No refresh token. Re-authenticate Google."))?;
        (
            creds.client_id.clone(),
            creds.client_secret.clone(),
            refresh,
        )
    };

    let new_tok = oauth::refresh_token(
        &state.client,
        TOKEN_URL,
        &client_id,
        &client_secret,
        &refresh,
        &[],
    )
    .await?;

    let access = new_tok.access_token.clone();
    state.storage.write().await.set_google_token(new_tok)?;
    Ok(access)
}

fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
