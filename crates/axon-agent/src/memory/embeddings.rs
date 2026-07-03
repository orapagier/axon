use crate::config::RuntimeSettings;
use anyhow::Context;

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

pub fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}
pub fn bytes_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// OpenAI-compatible embeddings client. One code path covers every provider
/// that speaks the `/embeddings` shape, so switching providers is a settings
/// change, not a code change:
///   * Google  — https://generativelanguage.googleapis.com/v1beta/openai + gemini-embedding-001
///   * Ollama  — http://localhost:11434/v1 + all-minilm (no API key)
///   * Voyage  — https://api.voyageai.com/v1 + voyage-4
#[derive(Clone)]
pub struct Embedder {
    base_url: String,
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl Embedder {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        Embedder {
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            api_key: api_key.trim().to_string(),
            model: model.trim().to_string(),
            client: reqwest::Client::builder()
                // Generous enough for a local Ollama cold-loading the model,
                // short enough not to stall a routing turn forever.
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Build from the `embedder.*` runtime settings. When no base URL is
    /// configured, falls back to Voyage via the legacy VOYAGE_API_KEY env var
    /// (the pre-settings behavior); returns `None` when nothing is configured —
    /// the semantic router tier and memory embeddings then no-op.
    pub fn from_settings(settings: &RuntimeSettings) -> Option<Self> {
        let base_url = settings.resolve(&settings.get_str("embedder.base_url", ""));
        let base_url = base_url.trim();
        if !base_url.is_empty() {
            let model = settings.resolve(&settings.get_str("embedder.model", ""));
            let model = model.trim();
            if model.is_empty() {
                tracing::warn!(
                    "embedder.base_url is set but embedder.model is empty — embeddings disabled"
                );
                return None;
            }
            let api_key = settings.resolve(&settings.get_str("embedder.api_key", ""));
            return Some(Embedder::new(base_url, &api_key, model));
        }
        std::env::var("VOYAGE_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .map(|k| Embedder::new("https://api.voyageai.com/v1", &k, "voyage-4"))
    }

    /// Identity of the vector space this embedder produces. Persisted next to
    /// stored embeddings so vectors from a previous provider/model are never
    /// cosine-compared against fresh ones.
    pub fn model_id(&self) -> &str {
        &self.model
    }

    pub async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut req = self.client.post(format!("{}/embeddings", self.base_url));
        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }
        let resp = req
            .json(&serde_json::json!({"input":texts,"model":self.model}))
            .send()
            .await
            .context("embeddings request")?;
        if !resp.status().is_success() {
            anyhow::bail!(
                "embeddings ({}): {}",
                self.model,
                resp.text().await.unwrap_or_default()
            );
        }
        #[derive(serde::Deserialize)]
        struct R {
            data: Vec<D>,
        }
        #[derive(serde::Deserialize)]
        struct D {
            embedding: Vec<f32>,
        }
        Ok(resp
            .json::<R>()
            .await
            .context("parse embeddings response")?
            .data
            .into_iter()
            .map(|d| d.embedding)
            .collect())
    }
    pub async fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.embed(&[text]).await?.pop().context("empty embedding")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_normalizes_base_url() {
        let e = Embedder::new(" http://localhost:11434/v1/ ", "", " all-minilm ");
        assert_eq!(e.base_url, "http://localhost:11434/v1");
        assert_eq!(e.model_id(), "all-minilm");
        assert!(e.api_key.is_empty());
    }
}
