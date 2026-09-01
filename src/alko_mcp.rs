//! Read-only MCP surface for Alko catalogue and store discovery.
//!
//! This is an intermediate transport surface while the grocery providers are being
//! composed into one server. It deliberately has no account state and no purchasing
//! capability: Alko access here is guest-session catalogue lookup only.

use std::sync::Arc;

use anyhow::Result;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ContentBlock, Implementation, IntoContents, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router, transport::stdio};
use serde::Deserialize;

use crate::providers::alko::{AlkoClient, AlkoProductSearchView, AlkoStoreView};

const LIMIT_DESC: &str = "How many results to return. Defaults to 10, capped at 50.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchAlkoProductsArg {
    #[schemars(description = "Free-text product search, e.g. \"riesling\" or \"lonkero\".")]
    pub query: String,
    #[schemars(
        description = "Optional Alko store id. Omit it to search the national catalogue; \
                       pass a store id from search_alko_stores to scope results to a store."
    )]
    pub store_id: Option<String>,
    #[schemars(description = LIMIT_DESC)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchAlkoStoresArg {
    #[schemars(
        description = "Optional city filter, e.g. \"Jyväskylä\". Omit it to list every store."
    )]
    pub city: Option<String>,
}

#[derive(Clone)]
pub struct AlkoServer {
    client: Arc<AlkoClient>,
}

impl Default for AlkoServer {
    fn default() -> Self {
        Self::new()
    }
}

impl AlkoServer {
    pub fn new() -> Self {
        Self {
            client: Arc::new(AlkoClient::new()),
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
impl AlkoServer {
    #[tool(
        annotations(
            title = "Search Alko products",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "Search Alko's product catalogue. Read-only. A store id is optional: \
                       omit it for the national catalogue or pass one returned by \
                       search_alko_stores to scope the search to a store."
    )]
    async fn search_alko_products(
        &self,
        Parameters(arg): Parameters<SearchAlkoProductsArg>,
    ) -> Result<Json<AlkoProductSearchView>, ToolFailure> {
        self.client
            .search_products(&arg.query, arg.store_id.as_deref(), arg.limit)
            .await
            .map(Json)
            .map_err(|error| ToolFailure(error.to_string()))
    }

    #[tool(
        annotations(
            title = "Search Alko stores",
            read_only_hint = true,
            idempotent_hint = true
        ),
        description = "List Alko stores, optionally filtered by city. Read-only. Returned \
                       store ids can be passed to search_alko_products."
    )]
    async fn search_alko_stores(
        &self,
        Parameters(arg): Parameters<SearchAlkoStoresArg>,
    ) -> Result<Json<Vec<AlkoStoreView>>, ToolFailure> {
        self.client
            .stores(arg.city.as_deref())
            .await
            .map(Json)
            .map_err(|error| ToolFailure(error.to_string()))
    }
}

#[tool_handler]
impl ServerHandler for AlkoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                format!("{}-alko", env!("CARGO_PKG_NAME")),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Read-only Alko catalogue access. Search products nationally or at a \
                 specific store with `search_alko_products`; use `search_alko_stores` to \
                 discover store ids. This surface cannot place orders or spend money.",
            )
    }
}

pub async fn serve() -> Result<()> {
    let service = AlkoServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
