//! Streamable HTTP transport for the unified Finnish grocery MCP.
//!
//! Two ways to deploy it:
//!
//! - **Behind Origo** (OAuth for outside clients): bind to loopback and let the Origo
//!   sidecar be the edge. No token is needed here, because nothing else can reach it.
//! - **On a private network** (an agent's tool gateway in the same Docker network): set
//!   `K_RUOKA_HTTP_TOKEN`, and every `/mcp` request must carry it as a bearer token. Pass
//!   the service's hostname with `--allowed-host`, because rmcp only accepts loopback
//!   `Host` headers by default (its DNS-rebinding guard).
//!
//! Binding anywhere but loopback without a token is refused rather than warned about:
//! this server drives a signed-in K-Ruoka session, so an open port is an open cart.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;

use crate::browser::session::default_store_path;
use crate::browser::{LaunchMode, Session, session::default_profile_dir};
use crate::grocery_mcp::GroceryServer;
use crate::login_flow::ChildLogin;

pub const DEFAULT_BIND: &str = "127.0.0.1:8000";

/// The bearer token, from the environment only: an argument would show up in `ps`.
pub const TOKEN_ENV: &str = "K_RUOKA_HTTP_TOKEN";

/// rmcp's own default `Host` allowlist, which `--allowed-host` extends.
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

pub async fn serve(bind: &str, allowed_hosts: &[String]) -> Result<()> {
    let token = std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty());
    check_exposure(bind, token.is_some())?;

    let profile_dir = default_profile_dir()?;
    let store_path = default_store_path(&profile_dir);
    let session = Arc::new(Session::new(profile_dir, LaunchMode::Headless)?);
    let login = Arc::new(ChildLogin::new(Arc::clone(&session)));
    let handler = GroceryServer::from_session(Arc::clone(&session), Arc::clone(&login), store_path);

    let cancellation = CancellationToken::new();
    let service_cancellation = cancellation.child_token();
    let handler_template = handler.clone();
    let hosts = LOOPBACK_HOSTS
        .iter()
        .map(|h| (*h).to_owned())
        .chain(allowed_hosts.iter().cloned());
    let mcp_service = StreamableHttpService::new(
        move || Ok(handler_template.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_allowed_hosts(hosts)
            .with_cancellation_token(service_cancellation),
    );

    let mcp = Router::new().nest_service("/mcp", mcp_service);
    let mcp = match token {
        Some(token) => mcp.layer(middleware::from_fn_with_state(
            Arc::new(token),
            require_bearer,
        )),
        None => mcp,
    };
    let router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(mcp);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding grocery MCP HTTP server to {bind}"))?;

    eprintln!("k-ruoka-mcp: unified grocery MCP listening on http://{bind}/mcp");
    let shutdown = cancellation.clone();
    let serving = axum::serve(listener, router).with_graceful_shutdown(async move {
        shutdown_signal().await;
        shutdown.cancel();
    });
    let outcome = serving.await;

    cancellation.cancel();
    session.signal_shutdown();
    login.shutdown().await;
    session.close().await.ok();

    outcome.context("grocery MCP HTTP server failed")?;
    Ok(())
}

/// Ctrl-C, or SIGTERM: `docker stop` sends the latter, and Chrome must be closed cleanly
/// so the profile (the login) isn't left locked.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn check_exposure(bind: &str, has_token: bool) -> Result<()> {
    if has_token || is_loopback(bind) {
        return Ok(());
    }
    bail!(
        "refusing to serve on {bind} without authentication: set {TOKEN_ENV}, or bind to \
         loopback and put Origo in front"
    )
}

fn is_loopback(bind: &str) -> bool {
    match bind.parse::<SocketAddr>() {
        Ok(addr) => addr.ip().is_loopback(),
        Err(_) => bind
            .rsplit_once(':')
            .is_some_and(|(host, _)| host == "localhost"),
    }
}

async fn require_bearer(
    State(token): State<Arc<String>>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(p) if constant_time_eq(p.as_bytes(), token.as_bytes()) => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "missing or wrong bearer token",
        )
            .into_response(),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_binds_need_no_token() {
        assert!(check_exposure("127.0.0.1:8000", false).is_ok());
        assert!(check_exposure("[::1]:8000", false).is_ok());
        assert!(check_exposure("localhost:8000", false).is_ok());
    }

    #[test]
    fn an_exposed_bind_without_a_token_is_refused() {
        assert!(check_exposure("0.0.0.0:8000", false).is_err());
        assert!(check_exposure("192.168.1.5:8000", false).is_err());
        assert!(check_exposure("k-ruoka:8000", false).is_err());
        assert!(check_exposure("0.0.0.0:8000", true).is_ok());
    }

    #[test]
    fn token_comparison_is_exact() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret2"));
        assert!(!constant_time_eq(b"", b"secret"));
    }
}
