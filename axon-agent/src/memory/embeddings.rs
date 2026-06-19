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

pub struct VoyageEmbedder {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl VoyageEmbedder {
    pub fn new(api_key: String) -> Self {
        VoyageEmbedder {
            api_key,
            model: "voyage-4".into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
        }
    }
    pub async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let resp = self
            .client
            .post("https://api.voyageai.com/v1/embeddings")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&serde_json::json!({"input":texts,"model":self.model}))
            .send()
            .await
            .context("Voyage AI request")?;
        if !resp.status().is_success() {
            anyhow::bail!("Voyage: {}", resp.text().await.unwrap_or_default());
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
            .context("parse voyage")?
            .data
            .into_iter()
            .map(|d| d.embedding)
            .collect())
    }
    pub async fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.embed(&[text]).await?.pop().context("empty embedding")
    }
}
