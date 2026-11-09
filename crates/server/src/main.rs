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
//!   WEB_DESKTOP_FS_WRITE
//!                     `on`/`off` for the write endpoints. Default: on for the
//!                     sandbox this server creates itself, off for a root chosen
//!                     with WEB_DESKTOP_ROOT, so pointing it at real work stays
//!                     read-only until you say otherwise.
//!   WEB_DESKTOP_DIST  path to the built client bundle
//!   WEB_DESKTOP_APPS  directory of served app bundles, default ./apps

mod apps;
mod fs;

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Instant};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderValue, header},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use shared::{API_CATALOG, API_HEALTH, Catalog, Health, default_catalog};
use tower_http::{compression::CompressionLayer, services::ServeDir, trace::TraceLayer};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::fs::FsRoot;

#[derive(Debug)]
pub struct AppState {
    started: Instant,
    /// Compiled-in apps and the rest of the bootstrap payload. Served apps are
    /// merged in per request, so rebuilding one does not need a server restart.
    catalog: Catalog,
    /// Directory of served app bundles; `None` when there is nothing to serve.
    apps_dir: Option<PathBuf>,
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

    // Served apps are appended to the compiled-in ones. They live under
    // /apps/<id>/<version>/, which is immutable, so the cache policy can treat
    // everything there as content-addressed.
    let apps_dir = std::env::var_os("WEB_DESKTOP_APPS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("apps"));
    let served = apps::scan(&apps_dir);
    if served.is_empty() {
        tracing::info!(
            "no served apps in {} (build one with `cargo xtask build-app crates/apps/<name>`)",
            apps_dir.display()
        );
    } else {
        tracing::info!("{} served app(s) in {}", served.len(), apps_dir.display());
    }

    let fs_root = match init_fs() {
        Ok(Some(root)) => {
            catalog.fs = root.info(true);
            tracing::info!(
                "file browser root: {} ({})",
                catalog.fs.root,
                if catalog.fs.writable {
                    "read-write"
                } else {
                    "read-only"
                }
            );
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
        apps_dir: apps_dir.is_dir().then_some(apps_dir.clone()),
        fs: fs_root,
    });

    let api = Router::new()
        .route(API_HEALTH, get(health))
        .route(API_CATALOG, get(catalog_handler))
        .merge(fs::router())
        .with_state(state.clone());

    // App bundles: plain static files, so `ServeDir` handles ranges, MIME types
    // (it knows .wasm and .js) and path traversal, and picks up precompressed
    // siblings when `cargo xtask precompress apps` has been run over them.
    let app = if apps_dir.is_dir() {
        api.nest_service(
            "/apps",
            ServeDir::new(&apps_dir)
                .precompressed_br()
                .precompressed_gzip(),
        )
    } else {
        api
    };

    let dist = resolve_dist();
    let index = dist.join("index.html");
    let app = if index.is_file() {
        tracing::info!("serving client bundle from {}", dist.display());
        // `precompressed_*` serve the `.br`/`.gz` siblings `cargo xtask
        // precompress` writes, so brotli quality 11 is paid once at build time
        // instead of on every request. Both fall back to the plain file when a
        // sibling is missing, and CompressionLayer covers anything left. Plain
        // `ServeDir` also maps `/` to `index.html`, which is right for an app with
        // no client-side router; with `leptos_router` you would want a history
        // fallback here instead.
        app.fallback_service(
            ServeDir::new(&dist)
                .precompressed_br()
                .precompressed_gzip(),
        )
    } else {
        tracing::warn!(
            "no client bundle at {} - run `cd crates/client && trunk build` (API only for now)",
            dist.display()
        );
        app.route(
            "/",
            get(|| async {
                "web-desktop API is up. The frontend bundle is missing: run `cd crates/client && trunk build`."
            }),
        )
    }
    // The wasm bundle is ~800 KB raw, so compression is the biggest single win for
    // load time. tower-http's default predicate compresses everything except images
    // (SVG is an exception), gRPC, SSE and bodies under 32 bytes, and it skips
    // responses that already carry a `content-encoding`, so precompressed assets
    // are not encoded twice.
    .layer(CompressionLayer::new())
    .layer(middleware::from_fn(cache_policy))
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
    let mut catalog = state.catalog.clone();
    if let Some(dir) = &state.apps_dir {
        // Rescanned per request: a few small file reads, and it means `cargo xtask
        // build-app` is picked up by the next page load rather than a server restart.
        catalog.apps.extend(apps::scan(dir));
    }
    Json(catalog)
}

/// Set up the sandboxed root: `WEB_DESKTOP_ROOT`, else `~/web-desktop`.
///
/// Whether it is writable depends on where it came from. A directory the server
/// created for this purpose is fair game; one the operator pointed at is not, until
/// they opt in — and either way the frontend is told which, so it can explain rather
/// than fail on save.
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

    let (path, chosen_root) = match std::env::var_os("WEB_DESKTOP_ROOT") {
        Some(dir) => (PathBuf::from(dir), true),
        None => (home_dir().join("web-desktop"), false),
    };

    let requested = std::env::var("WEB_DESKTOP_FS_WRITE").ok().map(|v| {
        matches!(
            v.to_ascii_lowercase().as_str(),
            "on" | "1" | "true" | "yes" | "enabled"
        )
    });
    let writable = requested.unwrap_or(!chosen_root);
    let readonly_reason = (!writable).then(|| match requested {
        Some(_) => "editing is switched off on this server (WEB_DESKTOP_FS_WRITE=off)".to_owned(),
        None => "this root was chosen with WEB_DESKTOP_ROOT, so it is read-only until you start the server with WEB_DESKTOP_FS_WRITE=on".to_owned(),
    });

    let (root, created) = FsRoot::new(&path, writable, readonly_reason)
        .map_err(|e| format!("{}: {e}", path.display()))?;
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
         The backend serves it over /api/fs/*, fenced to this directory: nothing\n\
         outside it is reachable, and links that point out are refused. Because\n\
         this sandbox was created for the purpose, it is read-write; a root you\n\
         point at yourself with WEB_DESKTOP_ROOT stays read-only until you start\n\
         the server with WEB_DESKTOP_FS_WRITE=on.\n\n\
         Delete these sample files whenever you like.\n",
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

/// Cache policy, in one place so the three cases cannot drift apart.
///
/// * Content-hashed assets (trunk's `filehash`), where the URL changes whenever
///   the bytes do: cached forever.
/// * Everything under `/api/fs/`: `no-store`. A file browser must never show a
///   stale view of a file the user just edited, and none of these responses carry
///   validators, so `no-cache` would mean a full refetch anyway.
/// * The rest, `index.html` above all, since it is what points at the hashes:
///   revalidate.
///
/// A handler that sets its own policy still wins.
async fn cache_policy(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let value = if looks_hashed(path) {
        "public, max-age=31536000, immutable"
    } else if path.starts_with("/api/fs/") {
        "no-store"
    } else {
        "no-cache"
    };

    let mut response = next.run(req).await;
    if !response.headers().contains_key(header::CACHE_CONTROL) {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    }
    response
}

/// Whether a path carries a trunk-style content hash, e.g.
/// `client-7d4238f865daf939_bg.wasm` or `style-b42eb280e7a76748.css`.
fn looks_hashed(path: &str) -> bool {
    let Some(file) = path.rsplit('/').next() else {
        return false;
    };
    // The hash width varies (15 and 16 hex characters both turn up, because the wasm
    // is named `client-<hash>_bg.wasm`), so look for a plausible hex run rather than
    // an exact width. Trunk owns the names in its output directory, and a false
    // positive there would only mean an unnecessarily long cache lifetime.
    file.split('-')
        .skip(1)
        .any(|segment| segment.chars().take_while(char::is_ascii_hexdigit).count() >= 12)
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
