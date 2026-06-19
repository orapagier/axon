use anyhow::Result;
use axon_core::{oauth, AppState};
use serde_json::{json, Value};

const SCOPES: &[&str] = &[
    "offline_access",
    "https://graph.microsoft.com/Mail.ReadWrite",
    "https://graph.microsoft.com/Mail.Send",
    "https://graph.microsoft.com/Calendars.ReadWrite",
    "https://graph.microsoft.com/Files.ReadWrite",
    "https://graph.microsoft.com/Team.ReadBasic.All",
    "https://graph.microsoft.com/Channel.ReadBasic.All",
    "https://graph.microsoft.com/ChannelMessage.Send",
    "https://graph.microsoft.com/Chat.ReadWrite",
    "https://graph.microsoft.com/User.Read",
];

pub async fn auth_url(state: &AppState) -> Result<Value> {
    let storage = state.storage.read().await;
    let creds = storage.microsoft_creds()?;
    let tenant = &creds.tenant_id;
    let scope = SCOPES.join(" ");
    let redir = oauth::callback_uri("microsoft");

    let url = format!(
        "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize?\
         client_id={}&redirect_uri={}&response_type=code&scope={}&response_mode=query",
        urlenc(&creds.client_id),
        urlenc(&redir),
        urlenc(&scope),
    );
    Ok(json!({
        "login_url": url,
        "instructions": "Click the login_url above to sign in with your Microsoft account."
    }))
}

pub async fn exchange_code(state: &AppState, code: &str) -> Result<Value> {
    let (client_id, client_secret, tenant) = {
        let s = state.storage.read().await;
        let c = s.microsoft_creds()?;
        (
            c.client_id.clone(),
            c.client_secret.clone(),
            c.tenant_id.clone(),
        )
    };
    let token_url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token");
    let scope = SCOPES.join(" ");

    let tok = oauth::exchange_code(
        &state.client,
        &token_url,
        &client_id,
        &client_secret,
        &oauth::callback_uri("microsoft"),
        code,
        &[("scope", &scope)],
    )
    .await?;

    state.storage.write().await.set_microsoft_token(tok)?;
    Ok(json!({ "success": true, "message": "Microsoft authenticated!" }))
}

pub async fn auth_status(state: &AppState) -> Result<Value> {
    let _ = state.storage.write().await.reload_tokens();
    let s = state.storage.read().await;
    match &s.tokens.microsoft {
        None => Ok(json!({ "authenticated": false })),
        Some(t) => Ok(json!({ "authenticated": true, "expired": t.is_expired() })),
    }
}

pub async fn revoke(state: &AppState) -> Result<Value> {
    state.storage.write().await.tokens.microsoft = None;
    state.storage.read().await.save_tokens()?;
    Ok(json!({ "success": true }))
}

pub async fn access_token(state: &AppState) -> Result<String> {
    let _ = state.storage.write().await.reload_tokens();
    {
        let s = state.storage.read().await;
        let t = s.tokens.microsoft.as_ref().ok_or_else(|| anyhow::anyhow!(
            "Microsoft not authenticated. Call microsoft_auth_url → sign in → microsoft_exchange_code."
        ))?;
        if !t.is_expired() {
            return Ok(t.access_token.clone());
        }
    }

    let (client_id, client_secret, tenant, refresh) = {
        let s = state.storage.read().await;
        let c = s.microsoft_creds()?;
        let t = s.tokens.microsoft.as_ref().unwrap();
        let r = t
            .refresh_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No refresh token."))?;
        (
            c.client_id.clone(),
            c.client_secret.clone(),
            c.tenant_id.clone(),
            r,
        )
    };

    let token_url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token");
    let scope = SCOPES.join(" ");
    let new_tok = oauth::refresh_token(
        &state.client,
        &token_url,
        &client_id,
        &client_secret,
        &refresh,
        &[("scope", &scope)],
    )
    .await?;

    let access = new_tok.access_token.clone();
    state.storage.write().await.set_microsoft_token(new_tok)?;
    Ok(access)
}

fn urlenc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
