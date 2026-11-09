//! Minimal axum backend for the web desktop.
//!
//! Two jobs:
//!   1. Serve the JSON API defined in the `shared` crate.
//!   2. In single-port mode, serve the `trunk` output directory (`client/dist`).
//!
//! During development you normally run `trunk serve` (port 8081) with its proxy
//! pointed here (port 8080) and let trunk own the frontend.

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Instant};

use axum::{Json, Router, extract::State, routing::get};
use shared::{API_CATALOG, API_HEALTH, Catalog, Health, default_catalog};
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug)]
struct AppState {
    started: Instant,
    catalog: Catalog,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "server=info,tower_http=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let addr: SocketAddr = std::env::var("WEB_DESKTOP_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_owned())
        .parse()?;

    let state = Arc::new(AppState {
        started: Instant::now(),
        catalog: default_catalog(),
    });

    let api = Router::new()
        .route(API_HEALTH, get(health))
        .route(API_CATALOG, get(catalog))
        .with_state(state.clone());

    let dist = resolve_dist();
    let index = dist.join("index.html");
    let app = if index.is_file() {
        tracing::info!("serving client bundle from {}", dist.display());
        // Plain `ServeDir` maps `/` to `index.html` and 404s everything else.
        // This app has no client-side router, so that is exactly right. If you
        // add `leptos_router`, swap in a history fallback:
        //   ServeDir::new(&dist).fallback(ServeFile::new(&index))
        api.fallback_service(ServeDir::new(&dist))
    } else {
        tracing::warn!(
            "no client bundle at {} - run `cd crates/client && trunk build` (API only for now)",
            dist.display()
        );
        api.route(
            "/",
            get(|| async {
                "web-desktop API is up. The frontend bundle is missing: run `cd crates/client && trunk build`."
            }),
        )
    }
    .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Health> {
    Json(Health {
        name: "web-desktop".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        uptime_secs: state.started.elapsed().as_secs(),
    })
}

async fn catalog(State(state): State<Arc<AppState>>) -> Json<Catalog> {
    Json(state.catalog.clone())
}

/// Find the trunk output dir, accepting a few working directories so that
/// `cargo run -p server` works from the workspace root or from `crates/server`.
fn resolve_dist() -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(explicit) = std::env::var("WEB_DESKTOP_DIST") {
        candidates.push(PathBuf::from(explicit));
    }
    candidates.push(PathBuf::from("crates/client/dist"));
    candidates.push(PathBuf::from("../client/dist"));
    candidates.push(PathBuf::from("dist"));
    candidates
        .iter()
        .find(|p| p.is_dir())
        .cloned()
        .unwrap_or_else(|| PathBuf::from("crates/client/dist"))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
