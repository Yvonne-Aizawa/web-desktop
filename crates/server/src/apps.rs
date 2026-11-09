//! Served apps: bundles the shell loads at runtime instead of compiling in.
//!
//! Layout, produced by `cargo xtask build-app` and served verbatim:
//!
//! ```text
//! apps/<id>/<version>/manifest.json   metadata advertised in the catalog
//! apps/<id>/<version>/app.js          ES module implementing the mount contract
//! apps/<id>/<version>/app_bg.wasm     the program
//! ```
//!
//! A version directory is immutable by contract, which is what lets the shell and
//! the cache policy treat it as content-addressed. The manifest is the *server's*
//! copy of the truth for the bundle (hash, size, ABI); the app's own metadata is
//! merged into the catalog so the start menu can show it before anything loads.

use std::{collections::HashSet, fs, path::Path};

use shared::{AppManifest, AppSource, ServedApp};

/// The build-time manifest, as written by `xtask build-app`.
#[derive(Debug, serde::Deserialize)]
struct BundleManifest {
    id: String,
    name: String,
    glyph: String,
    accent: String,
    blurb: String,
    default_width: u32,
    default_height: u32,
    /// Informational: the directory name is what the server serves and advertises.
    version: String,
    abi: String,
    entry: String,
    wasm: String,
    sha256: String,
    size: u64,
}

/// Read every bundle under `dir`, newest version per app wins.
///
/// A bundle that is incomplete or does not parse is skipped with a warning: a
/// broken app directory must not take the catalog down with it.
pub fn scan(dir: &Path) -> Vec<AppManifest> {
    if !dir.is_dir() {
        return Vec::new();
    }

    let mut found: Vec<AppManifest> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut apps: Vec<_> = match fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
        Err(err) => {
            tracing::warn!("apps: cannot read {}: {err}", dir.display());
            return found;
        }
    };
    apps.sort();

    for app_dir in apps {
        if !app_dir.is_dir() {
            continue;
        }
        let mut versions: Vec<_> = match fs::read_dir(&app_dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect(),
            Err(_) => continue,
        };
        // Newest first, so the first successful version of an app wins.
        versions.sort_by_key(|path| std::cmp::Reverse(version_key(path)));

        for version_dir in versions {
            let manifest_path = version_dir.join("manifest.json");
            let Ok(text) = fs::read_to_string(&manifest_path) else {
                continue;
            };
            let bundle: BundleManifest = match serde_json::from_str(&text) {
                Ok(bundle) => bundle,
                Err(err) => {
                    tracing::warn!("apps: {}: {err}", manifest_path.display());
                    continue;
                }
            };

            // The directory name is the version that gets served, so it is the one
            // that ends up in URLs; a manifest disagreeing with it is a build that
            // will fetch something other than what it describes.
            let version = version_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if bundle.version != version {
                tracing::warn!(
                    "apps: {} says version {} but lives in {}, using {version}",
                    manifest_path.display(),
                    bundle.version,
                    version
                );
            }

            // Refuse to advertise a bundle whose files are not actually there.
            let missing: Vec<&str> = [bundle.entry.as_str(), bundle.wasm.as_str()]
                .into_iter()
                .filter(|name| !version_dir.join(name).is_file())
                .collect();
            if !missing.is_empty() {
                tracing::warn!(
                    "apps: {} is missing {:?}, skipping",
                    version_dir.display(),
                    missing
                );
                continue;
            }

            if !seen.insert(bundle.id.clone()) {
                continue;
            }

            tracing::debug!(
                "apps: {} v{} ({} KiB, abi {})",
                bundle.id,
                version,
                bundle.size / 1024,
                bundle.abi
            );

            found.push(AppManifest {
                id: bundle.id,
                name: bundle.name,
                glyph: bundle.glyph,
                accent: bundle.accent,
                blurb: bundle.blurb,
                default_width: bundle.default_width,
                default_height: bundle.default_height,
                source: AppSource::Served(ServedApp {
                    version,
                    entry: bundle.entry,
                    wasm: bundle.wasm,
                    sha256: bundle.sha256,
                    size: bundle.size,
                    abi: bundle.abi,
                }),
            });
        }
    }

    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Sort key for a version directory: numeric parts compare numerically, so 0.10.0
/// is newer than 0.9.0.
fn version_key(path: &Path) -> Vec<u32> {
    path.file_name()
        .map(|name| {
            name.to_string_lossy()
                .split(['.', '-', '+'])
                .map(|part| part.parse::<u32>().unwrap_or(0))
                .collect()
        })
        .unwrap_or_default()
}
