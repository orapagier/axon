use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

/// Placeholder values shipped in config.example.toml. Startup refuses to
/// continue while any of these are still in place — otherwise a fresh install
/// would silently run with the example credentials.
const PLACEHOLDER_TOKEN: &str = "YOUR_CLOUDFLARE_TUNNEL_TOKEN_HERE";
const PLACEHOLDER_SECRET: &str = "change-this-to-a-strong-random-secret";

/// Minimum length for `api_secret`. This is the only thing standing between the
/// public internet (via the tunnel) and full control of the machine, so a short
/// or guessable value is treated as a fatal misconfiguration rather than a warning.
const MIN_SECRET_LEN: usize = 32;

/// cloudflared's own first-choice metrics port, so an operator who has looked
/// at a running instance before sees the port they expect.
const DEFAULT_METRICS_PORT: u16 = 20241;

/// Accepted values for `tunnel_protocol`. Anything else is a typo, and silently
/// treating a typo as "auto" would hide a pin the operator meant to set.
const VALID_PROTOCOLS: [&str; 4] = ["auto", "quic", "http2", "h2"];

#[derive(Deserialize, Clone)]
pub struct Config {
    pub tunnel_token: String,
    pub api_secret: String,
    pub port: u16,
    /// Base URL for downloadable file links (e.g. "https://windows.example.com")
    #[serde(default)]
    pub public_url: String,
    /// Loopback port the desktop agent listens on. The service forwards
    /// screenshot/clipboard/input routes here. Never leaves 127.0.0.1 and is
    /// never exposed through the tunnel. Defaults to `port + 1`.
    #[serde(default)]
    pub session_port: Option<u16>,
    /// Transport cloudflared uses to reach the Cloudflare edge.
    ///
    /// * `"auto"` (default) — start on QUIC and fall back to HTTP/2 if QUIC
    ///   proves unstable on this network.
    /// * `"http2"` — pin HTTP/2 over TCP. The right answer on links that drop
    ///   long-lived UDP flows, which is most consumer NAT.
    /// * `"quic"` — pin QUIC even if it flaps.
    #[serde(default)]
    pub tunnel_protocol: Option<String>,
    /// Loopback port for cloudflared's metrics endpoint, which the supervisor
    /// polls to tell "tunnel up" from "process up". Never exposed through the
    /// tunnel. Defaults to 20241, cloudflared's own first choice.
    #[serde(default)]
    pub tunnel_metrics_port: Option<u16>,
}

impl Config {
    /// Resolved loopback port for the desktop agent.
    pub fn session_port(&self) -> u16 {
        self.session_port.unwrap_or(self.port.wrapping_add(1))
    }

    /// Resolved loopback port for cloudflared's metrics endpoint.
    pub fn tunnel_metrics_port(&self) -> u16 {
        self.tunnel_metrics_port.unwrap_or(DEFAULT_METRICS_PORT)
    }
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

        config.validate(&path)?;

        Ok(config)
    }

    fn validate(&self, path: &std::path::Path) -> Result<()> {
        let where_ = path.display();

        if self.tunnel_token.trim().is_empty() || self.tunnel_token == PLACEHOLDER_TOKEN {
            anyhow::bail!("Please set your tunnel_token in {}", where_);
        }
        if self.api_secret.trim().is_empty() || self.api_secret == PLACEHOLDER_SECRET {
            anyhow::bail!("Please set a secure api_secret in {}", where_);
        }
        if self.api_secret.len() < MIN_SECRET_LEN {
            anyhow::bail!(
                "api_secret in {} is too short ({} chars, minimum {}). \
                 This secret is the only thing protecting this machine from the public \
                 internet — generate a strong one with: openssl rand -hex 32",
                where_,
                self.api_secret.len(),
                MIN_SECRET_LEN
            );
        }
        if !has_enough_variety(&self.api_secret) {
            anyhow::bail!(
                "api_secret in {} looks low-entropy (too few distinct characters). \
                 Generate a random one with: openssl rand -hex 32",
                where_
            );
        }
        if self.port == 0 {
            anyhow::bail!("port must be between 1 and 65535 in {}", where_);
        }
        if self.session_port() == 0 || self.session_port() == self.port {
            anyhow::bail!(
                "session_port in {} must be non-zero and different from port ({})",
                where_,
                self.port
            );
        }
        if let Some(p) = &self.tunnel_protocol {
            let p = p.trim().to_ascii_lowercase();
            if !VALID_PROTOCOLS.contains(&p.as_str()) {
                anyhow::bail!(
                    "tunnel_protocol in {} is \"{}\", which is not one of: {}. \
                     Leave it unset for \"auto\" (QUIC, falling back to HTTP/2 if it flaps).",
                    where_,
                    p,
                    VALID_PROTOCOLS.join(", ")
                );
            }
        }
        let metrics = self.tunnel_metrics_port();
        if metrics == 0 || metrics == self.port || metrics == self.session_port() {
            anyhow::bail!(
                "tunnel_metrics_port in {} must be non-zero and different from port ({}) \
                 and session_port ({})",
                where_,
                self.port,
                self.session_port()
            );
        }

        Ok(())
    }

    fn config_path() -> PathBuf {
        // Look next to the executable
        let mut path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
        path.pop();
        path.push("config.toml");
        path
    }
}

/// Cheap sanity check that rejects secrets built from a tiny alphabet — e.g. a
/// string of concatenated dates, or a repeated word. Not a real entropy
/// estimate, just enough to catch the obvious cases.
fn has_enough_variety(secret: &str) -> bool {
    let distinct: std::collections::HashSet<char> = secret.chars().collect();
    distinct.len() >= 10
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(secret: &str) -> Config {
        Config {
            tunnel_token: "a-real-looking-token".to_string(),
            api_secret: secret.to_string(),
            port: 8080,
            public_url: String::new(),
            session_port: None,
            tunnel_protocol: None,
            tunnel_metrics_port: None,
        }
    }

    fn validate(secret: &str) -> Result<()> {
        cfg(secret).validate(std::path::Path::new("config.toml"))
    }

    #[test]
    fn rejects_placeholder_secret() {
        assert!(validate(PLACEHOLDER_SECRET).is_err());
    }

    #[test]
    fn rejects_short_secret() {
        assert!(validate("abc123").is_err());
    }

    #[test]
    fn rejects_low_entropy_secret() {
        // Concatenated dates: 32 chars but only a handful of distinct digits.
        assert!(validate("05201991012319901223202008182025").is_err());
    }

    #[test]
    fn accepts_strong_secret() {
        assert!(validate("9f3b1c7ae2d84610bf59ca03e7d24b8f").is_ok());
    }

    #[test]
    fn rejects_unknown_tunnel_protocol() {
        let mut c = cfg("9f3b1c7ae2d84610bf59ca03e7d24b8f");
        c.tunnel_protocol = Some("tcp".into());
        assert!(c.validate(std::path::Path::new("config.toml")).is_err());
    }

    #[test]
    fn accepts_known_tunnel_protocols() {
        for p in ["auto", "QUIC", " http2 ", "h2"] {
            let mut c = cfg("9f3b1c7ae2d84610bf59ca03e7d24b8f");
            c.tunnel_protocol = Some(p.into());
            assert!(
                c.validate(std::path::Path::new("config.toml")).is_ok(),
                "{p} should be accepted"
            );
        }
    }

    #[test]
    fn rejects_metrics_port_colliding_with_api() {
        let mut c = cfg("9f3b1c7ae2d84610bf59ca03e7d24b8f");
        c.tunnel_metrics_port = Some(8080);
        assert!(c.validate(std::path::Path::new("config.toml")).is_err());
    }

    #[test]
    fn metrics_port_defaults_when_unset() {
        assert_eq!(cfg("x").tunnel_metrics_port(), DEFAULT_METRICS_PORT);
    }

    #[test]
    fn rejects_placeholder_token() {
        let mut c = cfg("9f3b1c7ae2d84610bf59ca03e7d24b8f");
        c.tunnel_token = PLACEHOLDER_TOKEN.to_string();
        assert!(c.validate(std::path::Path::new("config.toml")).is_err());
    }
}
