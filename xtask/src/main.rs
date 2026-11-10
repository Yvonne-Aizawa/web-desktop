//! Build tooling for the web desktop, run as `cargo xtask <command>`.
//!
//! Commands:
//!   precompress [dir]   write `.br` / `.gz` siblings for the shell's assets, so
//!                       the server never compresses on the fly (default dir:
//!                       `crates/client/dist`)
//!   build-app <dir>     build an app crate into the served app layout, with a
//!                       manifest that carries its hash, size and ABI version
//!   e2e [options]       build everything, then drive a real browser through the
//!                       suite in `crates/e2e` (see that directory's README)
//!   hash-password       read a password from stdin and print the bcrypt hash for
//!                       `WEB_DESKTOP_PASSWORD_HASH`
//!
//! Kept out of the server and the client on purpose: `brotli`, `flate2` and `sha2`
//! are build-time concerns, and `wasm-bindgen`/`wasm-opt` are binaries we only need
//! here.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use sha2::{Digest, Sha256};

/// Extensions worth precompressing. Already-compressed formats (png, jpg, woff2,
/// gz, br) are omitted: a second pass makes them bigger.
const COMPRESSIBLE: [&str; 7] = ["wasm", "js", "css", "html", "json", "svg", "txt"];

/// Interface version the shell implements. Bump it when the host object or the
/// mount contract changes incompatibly.
const ABI: &str = "1";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = match args.first().map(String::as_str) {
        Some("precompress") => {
            // With no argument, the shell's dist. `apps` and `dist` are accepted as
            // shorthands so the command works whatever directory it is run from.
            let dir = match args.get(1).map(String::as_str) {
                None => repo_root().join("crates/client/dist"),
                Some("dist") => repo_root().join("crates/client/dist"),
                Some("apps") => repo_root().join("apps"),
                Some(other) => PathBuf::from(other),
            };
            precompress(&dir)
        }
        Some("build-app") => match args.get(1) {
            Some(dir) => build_app(Path::new(dir)),
            None => Err("usage: cargo xtask build-app <crate-dir>".to_owned()),
        },
        Some("e2e") => e2e(&args[1..]),
        Some("hash-password") => hash_password(),
        Some(other) => Err(format!("unknown command `{other}`")),
        None => {
            eprintln!(
                "usage:\n  \
                 cargo xtask precompress [dir]\n  \
                 cargo xtask build-app <crate-dir>\n  \
                 cargo xtask hash-password\n  \
                 cargo xtask e2e [--list] [--only name,name] [--no-build] [--keep] [--skip-network]"
            );
            std::process::exit(2);
        }
    };

    if let Err(err) = result {
        eprintln!("xtask: {err}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------- precompress

fn precompress(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }

    let mut counts = Counts::default();
    let mut total_before = 0u64;
    let mut total_after = 0u64;

    for path in walk(dir)? {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !COMPRESSIBLE.contains(&ext.to_ascii_lowercase().as_str()) {
            continue;
        }

        let source = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let modified = fs::metadata(&path).and_then(|m| m.modified()).ok();
        total_before += source.len() as u64;

        for (suffix, encoded) in [
            (".br", brotli_encode(&source)?),
            (".gz", gzip_encode(&source)?),
        ] {
            let out = with_suffix(&path, suffix);

            // Only keep it if it actually helps.
            if encoded.len() >= source.len() {
                let _ = fs::remove_file(&out);
                continue;
            }
            // Regenerate only when the source is newer, so repeated builds are cheap.
            if is_fresh(&out, modified) {
                total_after += fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                counts.skipped += 1;
                continue;
            }

            fs::write(&out, &encoded).map_err(|e| format!("{}: {e}", out.display()))?;
            total_after += encoded.len() as u64;
            counts.written += 1;
        }
    }

    println!(
        "precompress: {} written, {} up to date  ({} KiB -> {} KiB on the wire)",
        counts.written,
        counts.skipped,
        total_before / 1024,
        total_after / 1024
    );
    Ok(())
}

#[derive(Default)]
struct Counts {
    written: usize,
    skipped: usize,
}

/// Brotli at maximum quality: this runs once per build, not per request.
fn brotli_encode(source: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut writer = brotli::CompressorWriter::new(&mut out, 4096, 11, 22);
    std::io::Write::write_all(&mut writer, source).map_err(|e| e.to_string())?;
    drop(writer);
    Ok(out)
}

fn gzip_encode(source: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::{Compression, write::GzEncoder};
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    std::io::Write::write_all(&mut encoder, source).map_err(|e| e.to_string())?;
    encoder.finish().map_err(|e| e.to_string())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn is_fresh(encoded: &Path, source_modified: Option<SystemTime>) -> bool {
    let (Ok(encoded_meta), Some(source_modified)) = (fs::metadata(encoded), source_modified) else {
        return false;
    };
    matches!(encoded_meta.modified(), Ok(t) if t >= source_modified)
}

fn walk(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current).map_err(|e| format!("{}: {e}", current.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

// ------------------------------------------------------------------ build-app

/// Metadata an app crate declares in `app.json`.
#[derive(serde::Deserialize)]
struct AppMeta {
    id: String,
    name: String,
    glyph: String,
    accent: String,
    blurb: String,
    default_width: u32,
    default_height: u32,
    #[serde(default = "default_version")]
    version: String,
}

fn default_version() -> String {
    "0.1.0".to_owned()
}

fn build_app(crate_dir: &Path) -> Result<(), String> {
    let meta_path = crate_dir.join("app.json");
    let meta: AppMeta = serde_json::from_str(
        &fs::read_to_string(&meta_path).map_err(|e| format!("{}: {e}", meta_path.display()))?,
    )
    .map_err(|e| format!("{}: {e}", meta_path.display()))?;

    let crate_name = package_name(crate_dir)?;
    let out_dir = crate_dir.join("dist");

    // 1. the app crate itself
    run(
        Command::new("cargo")
            .args([
                "build",
                "--profile",
                "wasm-release",
                "--target",
                "wasm32-unknown-unknown",
            ])
            .arg("--manifest-path")
            .arg(crate_dir.join("Cargo.toml")),
        "cargo build",
    )?;

    let target_root = target_dir(crate_dir);
    let raw_wasm = target_root
        .join("wasm32-unknown-unknown/wasm-release")
        .join(format!("{}.wasm", crate_name.replace('-', "_")));
    if !raw_wasm.is_file() {
        return Err(format!("expected {} to exist", raw_wasm.display()));
    }

    // 2. wasm-bindgen glue. Trunk caches the CLI; we reuse it rather than shipping
    //    another install path for it.
    let bindgen = find_cached_tool("wasm-bindgen")?;
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    run(
        Command::new(&bindgen)
            .args(["--target", "web", "--no-typescript", "--out-dir"])
            .arg(&out_dir)
            .arg(&raw_wasm),
        "wasm-bindgen",
    )?;

    let wasm = out_dir.join(format!("{}_bg.wasm", crate_name.replace('-', "_")));
    let glue = out_dir.join(format!("{}.js", crate_name.replace('-', "_")));
    if !wasm.is_file() || !glue.is_file() {
        return Err(format!(
            "wasm-bindgen produced neither {} nor {}",
            wasm.display(),
            glue.display()
        ));
    }

    // 3. wasm-opt, if we can find it
    if let Ok(opt) = find_cached_tool("wasm-opt") {
        let optimized = out_dir.join("optimized.wasm");
        if run(
            Command::new(&opt)
                .args(["-Oz", "-o"])
                .arg(&optimized)
                .arg(&wasm),
            "wasm-opt",
        )
        .is_ok()
        {
            let _ = fs::rename(&optimized, &wasm);
        }
    }

    // 4. stage it as apps/<id>/<version>/, hashing the wasm
    let staged = repo_root().join("apps").join(&meta.id).join(&meta.version);
    let _ = fs::remove_dir_all(&staged);
    fs::create_dir_all(&staged).map_err(|e| e.to_string())?;

    let wasm_bytes = fs::read(&wasm).map_err(|e| e.to_string())?;
    let wasm_name = "app_bg.wasm";
    let glue_name = "app.js";
    fs::write(staged.join(wasm_name), &wasm_bytes).map_err(|e| e.to_string())?;
    fs::copy(&glue, staged.join(glue_name)).map_err(|e| e.to_string())?;

    let manifest = serde_json::json!({
        "id": meta.id,
        "name": meta.name,
        "glyph": meta.glyph,
        "accent": meta.accent,
        "blurb": meta.blurb,
        "default_width": meta.default_width,
        "default_height": meta.default_height,
        "version": meta.version,
        "abi": ABI,
        "entry": glue_name,
        "wasm": wasm_name,
        "sha256": hex(&Sha256::digest(&wasm_bytes)),
        "size": wasm_bytes.len(),
    });
    fs::write(
        staged.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .map_err(|e| e.to_string())?;

    println!(
        "build-app: {} v{} -> {} ({} KiB wasm, abi {ABI})",
        meta.id,
        meta.version,
        staged.display(),
        wasm_bytes.len() / 1024
    );
    Ok(())
}

fn package_name(crate_dir: &Path) -> Result<String, String> {
    let text = fs::read_to_string(crate_dir.join("Cargo.toml"))
        .map_err(|e| format!("{}: {e}", crate_dir.display()))?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("name = ") {
            return Ok(rest.trim().trim_matches('"').to_owned());
        }
    }
    Err(format!("no package name in {}", crate_dir.display()))
}

/// The workspace target dir, from either the app dir or a nested one.
fn target_dir(crate_dir: &Path) -> PathBuf {
    let mut dir = crate_dir.to_path_buf();
    loop {
        let candidate = dir.join("target");
        if candidate.is_dir() {
            return candidate;
        }
        if !dir.pop() {
            return crate_dir.join("target");
        }
    }
}

fn find_cached_tool(name: &str) -> Result<PathBuf, String> {
    let cache = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".cache/trunk");
    let mut best: Option<PathBuf> = None;
    if let Ok(entries) = fs::read_dir(&cache) {
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with(name) {
                continue;
            }
            let binary = entry.path().join(name);
            if binary.is_file() {
                best = Some(binary);
            }
        }
    }
    best.ok_or_else(|| {
        format!(
            "no {name} in {} — run `trunk build --release` once to populate it",
            cache.display()
        )
    })
}

// ----------------------------------------------------------------------- e2e

/// Everything the suite needs built, and a Python environment to drive it.
/// The heavy options are passed straight through to `crates/e2e/run.py`.
fn e2e(args: &[String]) -> Result<(), String> {
    let root = repo_root();
    let suite = root.join("crates/e2e");
    if !suite.is_dir() {
        return Err(format!("{} is missing", suite.display()));
    }

    let mut passthrough = Vec::new();
    let mut build = true;
    let mut list = false;
    for arg in args {
        match arg.as_str() {
            "--no-build" => build = false,
            "--list" => {
                list = true;
                passthrough.push(arg.clone());
            }
            _ => passthrough.push(arg.clone()),
        }
    }

    let work = root.join("target/e2e");

    if build && !list {
        println!("xtask e2e: building the server, the shell and the served apps");
        run(
            Command::new("cargo")
                .args(["build", "--release", "-p", "server"])
                .current_dir(&root),
            "cargo build --release -p server",
        )?;
        run(
            Command::new("trunk")
                .args(["build", "--release"])
                .current_dir(root.join("crates/client")),
            "trunk build --release (is trunk installed?)",
        )?;
        for app in ["calculator", "pokedex"] {
            build_app(&root.join("crates/apps").join(app))?;
        }
        // the shell serves the precompressed siblings directly
        precompress(&root.join("crates/client/dist"))?;
        precompress(&root.join("apps"))?;
    }

    let python = ensure_venv(&work, &suite)?;
    let mut command = Command::new(&python);
    command
        .arg(suite.join("run.py"))
        .args(&passthrough)
        .current_dir(&root)
        .env("E2E_PYTHON", &python)
        .env("E2E_REPO", &root)
        .env("E2E_WORK", work.join("work"))
        .env("E2E_DIST", root.join("crates/client/dist"))
        .env("E2E_APPS", root.join("apps"));
    run(&mut command, "the browser suite")
}

/// A throwaway venv under `target/`, so the suite never depends on (or disturbs)
/// whatever Python the machine happens to have.
fn ensure_venv(work: &Path, suite: &Path) -> Result<PathBuf, String> {
    let venv = work.join("venv");
    let python = venv.join("bin/python");
    if !python.exists() {
        fs::create_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;
        run(
            Command::new("python3").args(["-m", "venv"]).arg(&venv),
            "python3 -m venv",
        )?;
    }
    let ready = Command::new(&python)
        .args(["-c", "import selenium, PIL, brotli"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ready {
        println!("xtask e2e: installing the suite's Python packages");
        run(
            Command::new(&python)
                .args(["-m", "pip", "install", "-q", "-r"])
                .arg(suite.join("requirements.txt")),
            "pip install -r crates/e2e/requirements.txt",
        )?;
    }
    Ok(python)
}

// ------------------------------------------------------------- hash-password

/// Turn a password into the bcrypt hash `WEB_DESKTOP_PASSWORD_HASH` wants.
///
/// The password is read from stdin rather than taken as an argument: arguments end up
/// in shell history and in `ps` output, and this is the one secret the operator types.
/// Only the hash goes to stdout, so
/// `WEB_DESKTOP_PASSWORD_HASH=$(cargo xtask hash-password)` works as written.
fn hash_password() -> Result<(), String> {
    use std::io::{BufRead, Write};

    eprint!("password: ");
    std::io::stderr().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let password = line.trim_end_matches(['\n', '\r']);
    if password.is_empty() {
        return Err("no password given".to_owned());
    }

    // The crate's default cost (12) on purpose: this runs once, and the cost is what
    // makes guessing over HTTP pointless.
    let hash = bcrypt::hash(password, bcrypt::DEFAULT_COST).map_err(|e| e.to_string())?;
    println!("{hash}");
    Ok(())
}

fn run(command: &mut Command, what: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|e| format!("{what}: could not run: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed: {status}"))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The workspace root, whatever directory the command was run from.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}
