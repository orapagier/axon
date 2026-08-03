use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Clone)]
pub struct Config {
    pub tunnel_token: String,
    pub api_secret: String,
    pub port: u16,
    /// Base URL for downloadable file links (e.g. "https://windows.canchowlung.com")
    #[serde(default)]
    pub public_url: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::config_path();

        if !path.exists() {
            // Write an example config so the user knows what to fill in
            std::fs::write(&path, include_str!("../config.example.toml"))
                .context("Failed to write default config")?;

            anyhow::bail!(
                "Config file created at: {}\n\nPlease fill in your tunnel_token and api_secret, then restart.",
                path.display()
            );
        }

        let content = std::fs::read_to_string(&path).context("Failed to read config.toml")?;

        let config: Config = toml::from_str(&content).context("Failed to parse config.toml")?;

        if config.tunnel_token == "YOUR_CLOUDFLARE_TUNNEL_TOKEN_HERE" {
            anyhow::bail!("Please set your tunnel_token in config.toml");
        }
        if config.api_secret == "change-this-to-a-strong-random-secret" {
            anyhow::bail!("Please set a secure api_secret in config.toml");
        }

        Ok(config)
    }

    fn config_path() -> PathBuf {
        // Look next to the executable
        let mut path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
        path.pop();
        path.push("config.toml");
        path
    }
}
