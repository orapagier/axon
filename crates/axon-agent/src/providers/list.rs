//! Fetch the list of models a provider currently exposes. Used to populate the
//! ModelsPage "Model ID" dropdown. Pure HTTP + parsing — no DB, no caching here
//! (that lives in `crate::model_cache`, which calls this once a day).
//!
//! Each provider advertises its catalogue differently, so the dispatch mirrors
//! `providers::call`: native adapters for Anthropic / Google / Ollama /
//! Cloudflare, and a shared OpenAI-compatible `GET /models` for everything else
//! (openai, openrouter, nvidia, groq, cerebras, …). Cloudflare needs its own
//! adapter because Workers AI is OpenAI-compatible for chat but lists its models
//! at the native `/ai/models/search`, not `GET /v1/models`.

use super::types::{normalize_base_url_str, normalize_provider_name, provider_base_url};
use anyhow::Context;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

/// One selectable model id, plus an optional human label (display name) when the
/// provider gives one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ModelChoice {
    fn new(id: impl Into<String>, label: Option<String>) -> Self {
        let label = label.filter(|l| !l.trim().is_empty());
        ModelChoice {
            id: id.into(),
            label,
        }
    }
}

// A separate, short-timeout client from the streaming chat client: listing is a
// quick metadata call and should fail fast rather than hang a background sweep.
static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent("axon-agent/1.0")
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("build model-list HTTP client")
});

/// Dispatch a "list models" request to the right provider adapter. `base_url`
/// is the model's configured base (may be `None`/empty → provider default);
/// `api_key` must already be resolved (no `${VAR}` placeholders).
pub async fn list_available_models(
    provider: &str,
    base_url: Option<&str>,
    api_key: &str,
) -> anyhow::Result<Vec<ModelChoice>> {
    let provider = normalize_provider_name(provider);
    let base = base_url
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let mut out = match provider.as_str() {
        "anthropic" => list_anthropic(base.as_deref(), api_key).await?,
        "google" => list_google(base.as_deref(), api_key).await?,
        "ollama" => list_ollama(base.as_deref(), api_key).await?,
        "cloudflare" => list_cloudflare(base.as_deref(), api_key).await?,
        _ => list_openai_compat(&provider, base.as_deref(), api_key).await?,
    };
    // Stable, de-duplicated ordering so the dropdown is predictable run to run.
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

// ── Anthropic: GET /v1/models (x-api-key + anthropic-version) ────────────────
#[derive(Deserialize)]
struct AnthList {
    data: Vec<AnthListItem>,
}
#[derive(Deserialize)]
struct AnthListItem {
    id: String,
    display_name: Option<String>,
}

async fn list_anthropic(base_url: Option<&str>, api_key: &str) -> anyhow::Result<Vec<ModelChoice>> {
    let base = base_url
        .map(normalize_base_url_str)
        .unwrap_or_else(|| "https://api.anthropic.com/v1".to_string());
    let url = format!("{}/models?limit=1000", base);
    let resp = HTTP_CLIENT
        .get(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .with_context(|| format!("HTTP to {}", url))?;
    let body = read_ok(resp, &url).await?;
    let parsed: AnthList = serde_json::from_str(&body).context("parse anthropic model list")?;
    Ok(parsed
        .data
        .into_iter()
        .map(|m| ModelChoice::new(m.id, m.display_name))
        .collect())
}

// ── Google Gemini: GET /v1beta/models?key= (paginated; generateContent only) ─
#[derive(Deserialize)]
struct GeminiList {
    #[serde(default)]
    models: Vec<GeminiListItem>,
    #[serde(rename = "nextPageToken", default)]
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
struct GeminiListItem {
    name: String,
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
    #[serde(rename = "supportedGenerationMethods", default)]
    supported_generation_methods: Vec<String>,
}

async fn list_google(base_url: Option<&str>, api_key: &str) -> anyhow::Result<Vec<ModelChoice>> {
    let base = base_url
        .map(normalize_base_url_str)
        // Runtime rows may carry the old OpenAI-compat shim suffix; strip it so
        // the native ListModels URL is correct (mirrors providers::google::call).
        .map(|b| b.strip_suffix("/openai").unwrap_or(&b).to_string())
        .or_else(|| provider_base_url("google").map(str::to_string))
        .unwrap_or_else(|| "https://generativelanguage.googleapis.com/v1beta".to_string());

    let mut out = Vec::new();
    let mut page_token: Option<String> = None;
    // Bound the pagination loop; Gemini's catalogue is well under this.
    for _ in 0..20 {
        let mut url = format!("{}/models?key={}&pageSize=1000", base, api_key);
        if let Some(tok) = &page_token {
            url.push_str(&format!("&pageToken={}", tok));
        }
        let resp = HTTP_CLIENT
            .get(&url)
            .send()
            .await
            .with_context(|| format!("HTTP to {}/models", base))?;
        // Redact the key from any error surfaced to logs/UI.
        let body = read_ok(resp, &format!("{}/models", base)).await?;
        let parsed: GeminiList = serde_json::from_str(&body).context("parse gemini model list")?;
        for m in parsed.models {
            // Only models usable by our generateContent path — skip embedding /
            // Live-API-only / vision-generation-only entries.
            if !m
                .supported_generation_methods
                .iter()
                .any(|s| s == "generateContent")
            {
                continue;
            }
            // Strip the "models/" prefix so the id matches what goes in model_id.
            let id = m
                .name
                .strip_prefix("models/")
                .unwrap_or(&m.name)
                .to_string();
            out.push(ModelChoice::new(id, m.display_name));
        }
        match parsed.next_page_token.filter(|t| !t.is_empty()) {
            Some(tok) => page_token = Some(tok),
            None => break,
        }
    }
    Ok(out)
}

// ── Ollama: GET /api/tags (native). Cloud (ollama.com) is the default host. ──
#[derive(Deserialize)]
struct OllamaTags {
    #[serde(default)]
    models: Vec<OllamaTagItem>,
}
#[derive(Deserialize)]
struct OllamaTagItem {
    name: String,
}

async fn list_ollama(base_url: Option<&str>, api_key: &str) -> anyhow::Result<Vec<ModelChoice>> {
    // /api/tags lives at the host root, not under /v1 — strip an OpenAI-compat
    // suffix if present. Default to the cloud host (ollama.com), per this
    // project's Ollama usage (cloud, not a local daemon).
    let base = base_url
        .map(normalize_base_url_str)
        .map(|b| b.trim_end_matches("/v1").trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "https://ollama.com".to_string());
    let url = format!("{}/api/tags", base);
    let mut req = HTTP_CLIENT.get(&url);
    // Cloud requires a key; a local daemon ignores the header.
    if !api_key.trim().is_empty() {
        req = req.header("Authorization", format!("Bearer {}", api_key));
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("HTTP to {}", url))?;
    let body = read_ok(resp, &url).await?;
    let parsed: OllamaTags = serde_json::from_str(&body).context("parse ollama tags")?;
    Ok(parsed
        .models
        .into_iter()
        .map(|m| ModelChoice::new(m.name, None))
        .collect())
}

// ── OpenAI-compatible: GET /models (Bearer). openai/openrouter/nvidia/groq/… ─
#[derive(Deserialize)]
struct OaiList {
    #[serde(default)]
    data: Vec<OaiListItem>,
}
#[derive(Deserialize)]
struct OaiListItem {
    id: String,
    // OpenRouter (and some hosts) include a friendly name; OpenAI does not.
    name: Option<String>,
}

async fn list_openai_compat(
    provider: &str,
    base_url: Option<&str>,
    api_key: &str,
) -> anyhow::Result<Vec<ModelChoice>> {
    let base = base_url
        .map(normalize_base_url_str)
        .or_else(|| provider_base_url(provider).map(str::to_string))
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let url = format!("{}/models", base);
    let mut req = HTTP_CLIENT.get(&url);
    // Some hosts (OpenRouter) expose /models publicly; sending an empty
    // `Bearer ` can 401 there, so only attach auth when we actually have a key.
    if !api_key.trim().is_empty() {
        req = req.header("Authorization", format!("Bearer {}", api_key));
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("HTTP to {}", url))?;
    let body = read_ok(resp, &url).await?;
    let parsed: OaiList =
        serde_json::from_str(&body).context("parse OpenAI-compatible model list")?;
    Ok(parsed
        .data
        .into_iter()
        .map(|m| ModelChoice::new(m.id, m.name))
        .collect())
}

// ── Cloudflare Workers AI: GET /accounts/{id}/ai/models/search (Bearer) ──────
//
// Workers AI is OpenAI-compatible for *chat* (so `providers::call` routes it
// through openai_compat), but it does NOT expose OpenAI's `GET /v1/models`.
// Its catalogue lives at the native `/accounts/{id}/ai/models/search`, returning
// a `{result:[…]}` envelope whose items carry the model id in `name` (which may
// be `@cf/…` or `@hf/…`), a `task` object, and a `properties` list.
//
// The account id is embedded in the model's own base_url
// (…/accounts/{id}/ai/v1), so the search URL is derived from that — nothing is
// hardcoded per account, matching how every other adapter reads its base.
#[derive(Deserialize)]
struct CfModelList {
    #[serde(default)]
    result: Vec<CfModelItem>,
}
#[derive(Deserialize)]
struct CfModelItem {
    name: String,
    #[serde(default)]
    task: Option<CfTask>,
    #[serde(default)]
    properties: Vec<CfProperty>,
}
#[derive(Deserialize)]
struct CfTask {
    #[serde(default)]
    name: Option<String>,
}
#[derive(Deserialize)]
struct CfProperty {
    #[serde(default)]
    property_id: Option<String>,
    #[serde(default)]
    value: serde_json::Value,
}

async fn list_cloudflare(
    base_url: Option<&str>,
    api_key: &str,
) -> anyhow::Result<Vec<ModelChoice>> {
    // Derive `…/accounts/{id}/ai` from the model's chat base_url by stripping the
    // OpenAI-compat `/v1` suffix, then hit `/models/search` off it.
    let base = base_url
        .map(normalize_base_url_str)
        .map(|b| b.trim_end_matches("/v1").trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .context(
            "Cloudflare model listing needs the model's Base URL \
             (https://api.cloudflare.com/client/v4/accounts/<ACCOUNT_ID>/ai/v1)",
        )?;
    if api_key.trim().is_empty() {
        anyhow::bail!("Cloudflare model listing requires an API token");
    }

    let mut out = Vec::new();
    // Terminate on an empty page (robust to whatever per_page the API enforces);
    // the loop cap bounds it well past Workers AI's ~150-model catalogue.
    for page in 1..=20 {
        let url = format!(
            "{}/models/search?task=Text%20Generation&hide_experimental=false&per_page=50&page={}",
            base, page
        );
        let resp = HTTP_CLIENT
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .send()
            .await
            .with_context(|| format!("HTTP to {}/models/search", base))?;
        let body = read_ok(resp, &format!("{}/models/search", base)).await?;
        let parsed: CfModelList =
            serde_json::from_str(&body).context("parse cloudflare model list")?;
        if parsed.result.is_empty() {
            break;
        }
        for m in &parsed.result {
            // The server already filters to Text Generation; re-check client-side
            // in case the filter is ignored, keeping models whose task is absent
            // (fail open) so a shape change never silently empties the dropdown.
            let is_text = m
                .task
                .as_ref()
                .and_then(|t| t.name.as_deref())
                .map(|n| n.to_ascii_lowercase().contains("text generation"))
                .unwrap_or(true);
            if !is_text {
                continue;
            }
            // Surface tool support in the label — Axon leans on function calling,
            // so this steers the user toward a model that will actually work.
            let has_tools = m.properties.iter().any(|p| {
                p.property_id.as_deref() == Some("function_calling")
                    && (p.value == serde_json::json!(true) || p.value == serde_json::json!("true"))
            });
            let label = has_tools.then(|| "function calling".to_string());
            out.push(ModelChoice::new(m.name.clone(), label));
        }
    }
    Ok(out)
}

/// Return the response body text on success, or a bail! carrying the status and
/// a truncated body on failure. `url` is included for diagnosis but any query
/// string (which may hold a key) is dropped first.
async fn read_ok(resp: reqwest::Response, url: &str) -> anyhow::Result<String> {
    let safe_url = url.split('?').next().unwrap_or(url);
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let body: String = body.chars().take(400).collect();
        anyhow::bail!("model list error {} at {}: {}", status, safe_url, body);
    }
    resp.text().await.context("read model list body")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_filters_to_generate_content_and_strips_prefix() {
        let body = r#"{
            "models": [
                {"name":"models/gemini-3.1-flash-lite","displayName":"Flash Lite","supportedGenerationMethods":["generateContent","countTokens"]},
                {"name":"models/text-embedding-004","supportedGenerationMethods":["embedContent"]},
                {"name":"models/gemini-live-2.5","supportedGenerationMethods":["bidiGenerateContent"]}
            ]
        }"#;
        let parsed: GeminiList = serde_json::from_str(body).unwrap();
        let choices: Vec<ModelChoice> = parsed
            .models
            .into_iter()
            .filter(|m| {
                m.supported_generation_methods
                    .iter()
                    .any(|s| s == "generateContent")
            })
            .map(|m| {
                let id = m
                    .name
                    .strip_prefix("models/")
                    .unwrap_or(&m.name)
                    .to_string();
                ModelChoice::new(id, m.display_name)
            })
            .collect();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].id, "gemini-3.1-flash-lite");
        assert_eq!(choices[0].label.as_deref(), Some("Flash Lite"));
    }

    #[test]
    fn openai_list_uses_id_and_optional_name() {
        let body = r#"{"data":[{"id":"gpt-4o"},{"id":"x/y:free","name":"Y Free"}]}"#;
        let parsed: OaiList = serde_json::from_str(body).unwrap();
        let choices: Vec<ModelChoice> = parsed
            .data
            .into_iter()
            .map(|m| ModelChoice::new(m.id, m.name))
            .collect();
        assert_eq!(
            choices[0],
            ModelChoice {
                id: "gpt-4o".into(),
                label: None
            }
        );
        assert_eq!(choices[1].label.as_deref(), Some("Y Free"));
    }

    // Network-gated: verifies the real OpenRouter path (public /models, no key)
    // populates the dropdown. Run with `cargo test -p axon -- --ignored openrouter`.
    #[tokio::test]
    #[ignore = "hits the live OpenRouter endpoint"]
    async fn openrouter_lists_models_without_a_key() {
        let choices = list_available_models("openrouter", None, "")
            .await
            .expect("openrouter list should succeed unauthenticated");
        assert!(
            choices.len() > 50,
            "expected a large catalogue, got {}",
            choices.len()
        );
        assert!(choices.iter().all(|c| !c.id.is_empty()));
    }

    #[test]
    fn cloudflare_filters_to_text_models_and_flags_tool_support() {
        // Mirrors the CF `/ai/models/search` envelope: `@cf/`+`@hf/` ids, a
        // `task` object, and a `properties` list whose `function_calling` flag
        // arrives as either a bool or the string "true".
        let body = r#"{
            "success": true,
            "result": [
                {"name":"@cf/meta/llama-3.3-70b-instruct-fp8-fast","task":{"name":"Text Generation"},"properties":[{"property_id":"function_calling","value":"true"}]},
                {"name":"@cf/baai/bge-base-en-v1.5","task":{"name":"Text Embeddings"},"properties":[]},
                {"name":"@hf/nousresearch/hermes-2-pro-mistral-7b","task":{"name":"Text Generation"},"properties":[{"property_id":"function_calling","value":true}]},
                {"name":"@cf/mistral/mistral-7b-instruct","task":{"name":"Text Generation"},"properties":[]}
            ]
        }"#;
        let parsed: CfModelList = serde_json::from_str(body).unwrap();
        // Same filter + label logic as `list_cloudflare`.
        let choices: Vec<ModelChoice> = parsed
            .result
            .iter()
            .filter_map(|m| {
                let is_text = m
                    .task
                    .as_ref()
                    .and_then(|t| t.name.as_deref())
                    .map(|n| n.to_ascii_lowercase().contains("text generation"))
                    .unwrap_or(true);
                if !is_text {
                    return None;
                }
                let has_tools = m.properties.iter().any(|p| {
                    p.property_id.as_deref() == Some("function_calling")
                        && (p.value == serde_json::json!(true)
                            || p.value == serde_json::json!("true"))
                });
                Some(ModelChoice::new(
                    m.name.clone(),
                    has_tools.then(|| "function calling".to_string()),
                ))
            })
            .collect();

        // Embedding model dropped; both flag encodings honored; plain model unlabeled.
        assert_eq!(choices.len(), 3);
        assert_eq!(choices[0].id, "@cf/meta/llama-3.3-70b-instruct-fp8-fast");
        assert_eq!(choices[0].label.as_deref(), Some("function calling"));
        assert_eq!(choices[1].id, "@hf/nousresearch/hermes-2-pro-mistral-7b");
        assert_eq!(choices[1].label.as_deref(), Some("function calling"));
        assert_eq!(choices[2].id, "@cf/mistral/mistral-7b-instruct");
        assert_eq!(choices[2].label, None);
    }

    #[test]
    fn anthropic_list_maps_display_name() {
        let body = r#"{"data":[{"id":"claude-haiku-4-5","display_name":"Claude Haiku 4.5"}]}"#;
        let parsed: AnthList = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.data[0].id, "claude-haiku-4-5");
        assert_eq!(
            parsed.data[0].display_name.as_deref(),
            Some("Claude Haiku 4.5")
        );
    }
}
