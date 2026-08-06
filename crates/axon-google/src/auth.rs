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
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v3/userinfo";

/// Marks the "connect an extra account as a credential" leg of the OAuth dance
/// so the shared callback can tell it apart from the Services-page sign-in and
/// avoid overwriting the global token. Mirrors Facebook's `fbcred`.
pub const CONNECT_STATE: &str = "gcred";

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

/// OAuth URL for the "connect an extra Google account as a credential" flow (the
/// Gmail node's Connect button). Same scopes as [`auth_url`], but carries
/// `state=gcred` so the callback saves the account as its own credential instead
/// of replacing the globally signed-in one.
///
/// `select_account` is added to the prompt because the whole point is to reach a
/// *different* inbox: without it Google silently reuses the session's current
/// account. No `login_hint` for the same reason. `consent` stays so Google keeps
/// issuing a refresh token — the credential is useless without one, since these
/// access tokens expire in an hour.
pub async fn connect_url(state: &AppState) -> Result<Value> {
    let storage = state.storage.read().await;
    let creds = storage.google_creds()?;

    let url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?\
         client_id={}&redirect_uri={}&response_type=code&\
         scope={}&access_type=offline&prompt={}&state={CONNECT_STATE}",
        urlenc(&creds.client_id),
        urlenc(&oauth::callback_uri("google")),
        urlenc(&SCOPES.join(" ")),
        urlenc("consent select_account"),
    );
    Ok(json!({
        "url": url,
        "instructions": "Open in browser and pick the Google account to add. It is saved as a credential you can select per node — the account signed in on the Credentials page stays the default."
    }))
}

/// Exchange an OAuth `code` for one extra account's tokens **without** touching
/// the global token, and identify which account it is.
///
/// Returns `{ email, name, access_token, refresh_token, expires_at }` for the
/// caller to persist as a credential. Fails when Google withholds a refresh
/// token: an access token alone dies in an hour and would leave a credential
/// that silently stops working, which is worse than refusing to save it.
pub async fn exchange_code_account(state: &AppState, code: &str) -> Result<Value> {
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

    let refresh_token = token.refresh_token.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "Google did not return a refresh token for this account. Remove Axon at \
             myaccount.google.com/permissions and connect again so the consent screen re-appears."
        )
    })?;

    // Identify the account. This is what names the credential, so a user with
    // several inboxes can tell them apart in the node dropdown.
    let profile: Value = state
        .get(&token.access_token, USERINFO_URL)
        .await
        .unwrap_or_else(|_| json!({}));
    let email = profile
        .get("email")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Could not read the account's email address. Make sure the \
                 userinfo.email scope was granted."
            )
        })?;

    Ok(json!({
        "email": email,
        "name": profile.get("name").and_then(|v| v.as_str()).unwrap_or(email),
        "access_token": token.access_token,
        "refresh_token": refresh_token,
        "expires_at": token.expires_at,
    }))
}

/// Refresh one credential-held account's access token. Unlike [`access_token`]
/// this touches no global state — the caller owns the returned token and is
/// responsible for writing it back to wherever the credential lives.
pub async fn refresh_account(
    state: &AppState,
    refresh_token: &str,
) -> Result<axon_core::OAuthToken> {
    let (client_id, client_secret) = {
        let storage = state.storage.read().await;
        let creds = storage.google_creds()?;
        (creds.client_id.clone(), creds.client_secret.clone())
    };
    oauth::refresh_token(
        &state.client,
        TOKEN_URL,
        &client_id,
        &client_secret,
        refresh_token,
        &[],
    )
    .await
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
///
/// Every Google helper in this crate authenticates through here, which makes it
/// the one place that decides *which* account a call runs as. A caller that
/// scoped the work with `axon_core::google_account::scoped` has already resolved
/// and refreshed a specific account's token, so it wins; everything else uses
/// the globally signed-in account from the Credentials page.
pub async fn access_token(state: &AppState) -> Result<String> {
    if let Some(token) = axon_core::google_account::current() {
        return Ok(token);
    }
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
