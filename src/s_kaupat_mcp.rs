//! Read-only MCP surface for S-Kaupat catalogue and store discovery.
//!
//! Temporary validation surface while the providers are being composed into the main
//! server. The provider uses the existing Chrome lifecycle only to learn persisted-query
//! hashes; normal catalogue calls are direct HTTP.

use std::sync::Arc;

use anyhow::Result;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ContentBlock, Implementation, IntoContents, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router, transport::stdio};
use serde::Deserialize;

use crate::browser::{LaunchMode, Session, session::default_profile_dir};
use crate::providers::s_kaupat::{
    SKaupatClient, SKaupatProductSearchView, SKaupatStoreSearchView,
};

const LIMIT_DESC: &str = "How many results to return. Defaults to 10, capped at 50.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchSKaupatProductsArg {
    #[schemars(description = "Free-text product search, preferably in Finnish.")]
    pub query: String,
    #[schemars(
        description = "S-Kaupat store id from search_s_kaupat_stores. Product prices and \
                       assortment are store-specific."
    )]
    pub store_id: String,
    #[schemars(description = LIMIT_DESC)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchSKaupatStoresArg {
    #[schemars(
        description = "Optional place or store-name query, e.g. \"Jyväskylä\" or \"Seppälä\"."
    )]
    pub query: Option<String>,
}

#[derive(Clone)]
pub struct SKaupatServer {
    client: Arc<SKaupatClient>,
}

impl SKaupatServer {
    pub fn new(browser: Arc<Session>) -> Self {
        Self {
            client: Arc::new(SKaupatClient::new(browser)),
        }
    }
}

pub struct ToolFailure(String);

impl IntoContents for ToolFailure {
    fn into_contents(self) -> Vec<ContentBlock> {
        vec![ContentBlock::text(self.0)]
    }
}

#[tool_router]
impl SKaupatServer {
    #[tool(
        annotations(
            title = "Search S-Kaupat products",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Search products at a specific S-Kaupat store. Read-only. Use \
                       search_s_kaupat_stores first to resolve a store id."
    )]
    async fn search_s_kaupat_products(
        &self,
        Parameters(arg): Parameters<SearchSKaupatProductsArg>,
    ) -> Result<Json<SKaupatProductSearchView>, ToolFailure> {
        self.client
            .search_products(&arg.query, &arg.store_id, arg.limit)
            .await
            .map(Json)
            .map_err(|error| ToolFailure(error.to_string()))
    }

    #[tool(
        annotations(
            title = "Search S-Kaupat stores",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Search S-Kaupat stores by place or store name. Read-only. Returned \
                       store ids can be passed to search_s_kaupat_products."
    )]
    async fn search_s_kaupat_stores(
        &self,
        Parameters(arg): Parameters<SearchSKaupatStoresArg>,
    ) -> Result<Json<SKaupatStoreSearchView>, ToolFailure> {
        self.client
            .search_stores(arg.query.as_deref())
            .await
            .map(Json)
            .map_err(|error| ToolFailure(error.to_string()))
    }
}

#[tool_handler]
impl ServerHandler for SKaupatServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                format!("{}-s-kaupat", env!("CARGO_PKG_NAME")),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Read-only S-Kaupat catalogue access. Resolve a store with \
                 `search_s_kaupat_stores`, then search its catalogue with \
                 `search_s_kaupat_products`. This surface cannot place orders or spend money.",
            )
    }
}

pub async fn serve() -> Result<()> {
    let profile_dir = default_profile_dir()?;
    let browser = Arc::new(Session::new(profile_dir, LaunchMode::Headless)?);
    let service = SKaupatServer::new(Arc::clone(&browser)).serve(stdio()).await?;
    let outcome = service.waiting().await;
    browser.signal_shutdown();
    browser.close().await.ok();
    outcome?;
    Ok(())
}
