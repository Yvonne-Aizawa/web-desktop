//! Minimal axum backend for the web desktop.
//!
//! Three jobs:
//!   1. Serve the JSON API defined in the `shared` crate.
//!   2. Serve the `trunk` output directory (`client/dist`) in single-port mode.
//!   3. Expose one directory on this machine to the Files app, fenced inside a
//!      single root (see [`fs`]).
//!
//! During development you normally run `trunk serve` (port 8081) with its proxy
//! pointed here (port 8080) and let trunk own the frontend.
//!
//! Configuration:
//!   WEB_DESKTOP_ADDR  listen address, default 127.0.0.1:8080
//!   WEB_DESKTOP_ROOT  directory the file browser may read, default ~/web-desktop
//!   WEB_DESKTOP_FS    set to `off` to disable the filesystem API entirely
//!   WEB_DESKTOP_DIST  path to the built client bundle

mod fs;

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Instant};

use axum::{Json, Router, extract::State, routing::get};
use shared::{API_CATALOG, API_HEALTH, Catalog, Health, default_catalog};
use tower_http::{compression::CompressionLayer, services::ServeDir, trace::TraceLayer};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::fs::FsRoot;

#[derive(Debug)]
pub struct AppState {
    started: Instant,
    catalog: Catalog,
    /// `None` when the filesystem API is switched off or its root is unusable.
    fs: Option<FsRoot>,
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
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()?;

    let mut catalog = default_catalog();
    let fs_root = match init_fs() {
        Ok(Some(root)) => {
            catalog.fs = root.info(true);
            tracing::info!("file browser root: {} (read-only)", catalog.fs.root);
            Some(root)
        }
        Ok(None) => {
            tracing::info!("file browser disabled by WEB_DESKTOP_FS");
            None
        }
        Err(err) => {
            tracing::error!("file browser disabled: {err}");
            None
        }
    };

    let state = Arc::new(AppState {
        started: Instant::now(),
        catalog,
        fs: fs_root,
    });

    let api = Router::new()
        .route(API_HEALTH, get(health))
        .route(API_CATALOG, get(catalog_handler))
        .merge(fs::router())
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
    // The wasm bundle is ~2.8 MB raw and ~370 KB gzipped, so this is the single
    // biggest win for load time. tower-http's default predicate compresses
    // everything except images (SVG is an exception), gRPC, SSE and bodies under
    // 32 bytes, so the image endpoint does not pay for it.
    .layer(CompressionLayer::new())
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

async fn catalog_handler(State(state): State<Arc<AppState>>) -> Json<Catalog> {
    Json(state.catalog.clone())
}

/// Set up the sandboxed root: `WEB_DESKTOP_ROOT`, else `~/web-desktop`.
fn init_fs() -> Result<Option<FsRoot>, String> {
    let disabled = std::env::var("WEB_DESKTOP_FS")
        .map(|v| {
            matches!(
                v.to_ascii_lowercase().as_str(),
                "off" | "0" | "false" | "disabled"
            )
        })
        .unwrap_or(false);
    if disabled {
        return Ok(None);
    }

    let path = match std::env::var_os("WEB_DESKTOP_ROOT") {
        Some(dir) => PathBuf::from(dir),
        None => home_dir().join("web-desktop"),
    };

    let (root, created) = FsRoot::new(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if created {
        tracing::info!("created sandbox at {}", root.root().display());
        if let Err(err) = seed(root.root()) {
            tracing::warn!("could not write sample files: {err}");
        }
    }
    Ok(Some(root))
}

/// Fill a brand new sandbox with a few files, so the first run shows something
/// instead of an empty folder. Only ever called when we created the directory.
fn seed(root: &std::path::Path) -> std::io::Result<()> {
    let notes = root.join("notes");
    std::fs::create_dir_all(&notes)?;
    std::fs::write(
        root.join("README.txt"),
        "This folder is the sandbox for the web desktop's Files app.\n\n\
         The backend serves it read-only over /api/fs/*; nothing outside this\n\
         directory is reachable. Delete these sample files whenever you like.\n\n\
         Point the API somewhere else with WEB_DESKTOP_ROOT=/some/path.\n",
    )?;
    std::fs::write(
        notes.join("todo.md"),
        "# todo\n\n- [x] window manager\n- [x] real file browser\n- [ ] write support\n",
    )?;
    std::fs::write(
        notes.join("scratch.rs"),
        "fn main() {\n    println!(\"hello from the sandbox\");\n}\n",
    )?;
    std::fs::write(
        root.join(".hidden-example"),
        "dotfiles are hidden by default\n",
    )?;
    Ok(())
}

fn home_dir() -> PathBuf {
    // `std::env::home_dir` is deprecated; HOME covers the platforms this targets.
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
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
