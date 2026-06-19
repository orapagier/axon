//! In-process MCP provider.
//!
//! Runs the integration services (Google, Microsoft, Facebook, Instagram,
//! Business, CRM) directly inside the agent instead of as a separate axon-mcp
//! process reached over SSE. Tool dispatch becomes a plain async function call:
//! no second process, no JSON-RPC/SSE framing, no localhost network hop.
//!
//! The provider owns the shared `axon_core::AppState` (OAuth storage, HTTP
//! client, temporary media registry). The same state instance backs the agent's
//! OAuth callback and media routes, so a token obtained via the dashboard is
//! immediately visible to every service.

use std::sync::Arc;

use serde_json::Value;

use crate::tools::schema::ToolDefinition;
use axon_business::BusinessService;
use axon_core::AppState as McpState;
use axon_crm::CrmService;
use axon_facebook::FacebookService;
use axon_google::GoogleService;
use axon_instagram::InstagramService;
use axon_microsoft::MicrosoftService;

pub struct InProcessMcp {
    state: Arc<McpState>,
    google: Arc<GoogleService>,
    microsoft: Arc<MicrosoftService>,
    facebook: Arc<FacebookService>,
    instagram: Arc<InstagramService>,
    business: Arc<BusinessService>,
    crm: Arc<CrmService>,
    tools_cache: Vec<ToolDefinition>,
}

impl InProcessMcp {
    pub async fn new(server_name: &str) -> anyhow::Result<Self> {
        let state = Arc::new(McpState::new().await?);
        let google = Arc::new(GoogleService::new(state.clone()));
        let microsoft = Arc::new(MicrosoftService::new(state.clone()));
        let facebook = Arc::new(FacebookService::new(state.clone()));
        let instagram = Arc::new(InstagramService::new(state.clone()));
        let business = Arc::new(BusinessService::new(state.clone()));
        let crm = Arc::new(CrmService::new(state.clone()).await?);

        let tools_cache = build_tool_defs(server_name);
        tracing::info!("In-process MCP '{}': {} tools", server_name, tools_cache.len());

        Ok(Self {
            state,
            google,
            microsoft,
            facebook,
            instagram,
            business,
            crm,
            tools_cache,
        })
    }

    /// Shared OAuth/media state, for the agent's `/auth` and `/media` routes.
    pub fn mcp_state(&self) -> Arc<McpState> {
        self.state.clone()
    }

    pub fn cached_tools(&self) -> Vec<ToolDefinition> {
        self.tools_cache.clone()
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> anyhow::Result<Value> {
        let map = match args {
            Value::Object(m) => m,
            Value::Null => serde_json::Map::new(),
            other => {
                let mut m = serde_json::Map::new();
                m.insert("input".to_string(), other);
                m
            }
        };
        let result = self.dispatch(name, map).await?;
        // Serialize the rmcp CallToolResult to the same wire shape the SSE path
        // produced, then reuse the existing normalizer.
        let v = serde_json::to_value(result)?;
        Ok(crate::mcp::client::normalize_mcp_output(v))
    }

    async fn dispatch(
        &self,
        name: &str,
        args: serde_json::Map<String, Value>,
    ) -> anyhow::Result<rmcp::model::CallToolResult> {
        // Same prefix routing as the former axon-mcp server. Order matters:
        // `facebook_instagram_auth_url` must reach the Facebook service.
        if is_google(name) {
            self.google.call(name, args).await
        } else if is_microsoft(name) {
            self.microsoft.call(name, args).await
        } else if name.starts_with("facebook_") || name.starts_with("fb_") {
            self.facebook.call(name, args).await
        } else if name.starts_with("instagram_") || name.starts_with("ig_") {
            self.instagram.call(name, args).await
        } else if name.starts_with("crm_") {
            self.crm.call(name, args).await
        } else {
            self.business.call(name, args).await
        }
    }
}

fn is_google(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "google_", "gmail_", "gcal_", "gdrive_", "gdocs_", "gsheets_", "gcon_", "gmeet_",
        "gtasks_", "gslides_", "gforms_", "gchat_",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

fn is_microsoft(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "microsoft_",
        "outlook_",
        "mscal_",
        "onedrive_",
        "teams_",
        "mscontacts_",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

fn build_tool_defs(server_name: &str) -> Vec<ToolDefinition> {
    let mut all = Vec::new();
    let lists = [
        GoogleService::tool_list(),
        MicrosoftService::tool_list(),
        FacebookService::tool_list(),
        InstagramService::tool_list(),
        BusinessService::tool_list(),
        CrmService::tool_list(),
    ];
    for list in lists {
        for tool in list {
            if let Ok(v) = serde_json::to_value(&tool) {
                if let Some(def) = crate::mcp::client::tool_def_from_json(&v, server_name) {
                    all.push(def);
                }
            }
        }
    }
    all
}
