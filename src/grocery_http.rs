//! Streamable HTTP transport for the unified Finnish grocery MCP.
//!
//! Authentication is intentionally not implemented here. The deployment boundary is
//! Origo, which owns OAuth and forwards authenticated traffic to this service. Bind to
//! loopback when Origo runs as a sidecar; use a pod/service address only with an explicit
//! network policy protecting this backend.

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{Router, routing::get};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;

use crate::browser::{LaunchMode, Session, session::default_profile_dir};
use crate::browser::session::default_store_path;
use crate::grocery_mcp::GroceryServer;
use crate::login_flow::ChildLogin;

pub const DEFAULT_BIND: &str = "127.0.0.1:8000";

pub async fn serve(bind: &str) -> Result<()> {
    let profile_dir = default_profile_dir()?;
    let store_path = default_store_path(&profile_dir);
    let session = Arc::new(Session::new(profile_dir, LaunchMode::Headless)?);
    let login = Arc::new(ChildLogin::new(Arc::clone(&session)));
    let handler = GroceryServer::from_session(Arc::clone(&session), Arc::clone(&login), store_path);

    let cancellation = CancellationToken::new();
    let service_cancellation = cancellation.child_token();
    let handler_template = handler.clone();
    let mcp_service = StreamableHttpService::new(
        move || Ok(handler_template.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(service_cancellation),
    );

    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest_service("/mcp", mcp_service);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding grocery MCP HTTP server to {bind}"))?;

    eprintln!("k-ruoka-mcp: unified grocery MCP listening on http://{bind}/mcp");
    let shutdown = cancellation.clone();
    let serving = axum::serve(listener, router).with_graceful_shutdown(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            shutdown.cancel();
        }
    });
    let outcome = serving.await;

    cancellation.cancel();
    session.signal_shutdown();
    login.shutdown().await;
    session.close().await.ok();

    outcome.context("grocery MCP HTTP server failed")?;
    Ok(())
}
