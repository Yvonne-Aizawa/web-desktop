//! A read-only view of one directory on this machine.
//!
//! # The fence
//!
//! Every request carries a root-relative path. [`FsRoot::resolve`] joins it onto
//! the root and then **canonicalizes** the result, which resolves symlinks, and
//! refuses anything that lands outside the root. That single check is what makes
//! `..`, absolute paths, and escaping symlinks all fail closed; it is deliberately
//! the only way to turn client input into a filesystem path.
//!
//! # Adding writes
//!
//! The read endpoints are the shape the write endpoints should follow:
//!
//! * `POST /api/fs/dir` `{path, name}`      - create a directory
//! * `PATCH /api/fs/entry` `{path, name}`   - rename or move
//! * `DELETE /api/fs/entry?path=`           - delete
//! * `PUT /api/fs/file?path=`               - write/upload
//!
//! Three things are already in place for that: the router applies
//! [`same_origin`] to everything under `/api/fs`, [`FsInfo::writable`] tells the
//! frontend whether to show mutation UI at all, and creation-only paths resolve
//! by canonicalizing the *parent* (a path that does not exist yet cannot be
//! canonicalized) - see [`FsRoot::resolve_parent`].

use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use shared::fs::{
    API_FS_DOWNLOAD, API_FS_LIST, API_FS_PREVIEW, ApiError, DirListing, EntryKind, FilePreview,
    FsEntry, FsInfo, PreviewBody,
};

use crate::AppState;

/// Largest text payload returned by the preview endpoint.
pub const MAX_PREVIEW_BYTES: u64 = 256 * 1024;
/// Largest file the download endpoint will buffer.
pub const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;
/// Entries returned for one directory before the listing is cut short.
pub const MAX_ENTRIES: usize = 2_000;
/// Bytes inspected to decide whether a file is binary.
const SNIFF_BYTES: usize = 8 * 1024;

/// Routes for the filesystem API, with the cross-site guard applied to all of them.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(API_FS_LIST, get(list))
        .route(API_FS_PREVIEW, get(preview))
        .route(API_FS_DOWNLOAD, get(download))
        .route_layer(middleware::from_fn(same_origin))
}

/// Reject requests a browser tells us came from another site.
///
/// Reads are already safe from cross-site snooping (no CORS headers means the
/// response cannot be read by another origin), but a cross-site `fetch` still
/// *executes* here. That matters as soon as writes exist, and costs nothing now.
/// `Sec-Fetch-Site` is sent by every current browser; clients that omit it, such
/// as `curl`, are allowed through.
async fn same_origin(req: Request, next: Next) -> Result<Response, FsError> {
    let cross_site = req
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        == Some("cross-site");

    if cross_site {
        return Err(FsError::Denied("cross-site request rejected".to_owned()));
    }

    Ok(next.run(req).await)
}

/// The sandboxed root directory.
#[derive(Debug)]
pub struct FsRoot {
    root: PathBuf,
}

impl FsRoot {
    /// Create the directory if needed and canonicalize it, so every later
    /// comparison is made between two canonical paths.
    pub fn new(path: &Path) -> std::io::Result<(Self, bool)> {
        let created = !path.exists();
        std::fs::create_dir_all(path)?;
        let root = path.canonicalize()?;
        Ok((Self { root }, created))
    }

    pub fn info(&self, enabled: bool) -> FsInfo {
        FsInfo {
            enabled,
            root: self.root.to_string_lossy().into_owned(),
            // No write endpoint exists yet; see the module docs.
            writable: false,
            max_preview_bytes: MAX_PREVIEW_BYTES,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Turn a client-supplied relative path into a canonical path inside the root.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, FsError> {
        let rel = normalise(rel)?;
        if rel.is_empty() {
            return Ok(self.root.clone());
        }

        let candidate = self.root.join(&rel);
        let canonical = candidate
            .canonicalize()
            .map_err(|e| FsError::from_io(&e, &rel))?;

        self.contained(canonical, &rel)
    }

    /// Same fence, for a path that does not exist yet: canonicalize the parent
    /// and append the final component. Unused by the read API; here so the write
    /// endpoints have one obvious place to start.
    #[allow(dead_code)]
    pub fn resolve_parent(&self, rel: &str) -> Result<PathBuf, FsError> {
        let rel = normalise(rel)?;
        let (parent, name) = match rel.rsplit_once('/') {
            Some((parent, name)) => (parent.to_owned(), name.to_owned()),
            None => (String::new(), rel.clone()),
        };
        if name.is_empty() || name == "." || name == ".." {
            return Err(FsError::BadRequest("invalid entry name".to_owned()));
        }
        let dir = self.resolve(&parent)?;
        Ok(dir.join(name))
    }

    fn contained(&self, canonical: PathBuf, rel: &str) -> Result<PathBuf, FsError> {
        // `Path::starts_with` compares whole components, so
        // `/home/yvonne/web-desktop-other` does not match root
        // `/home/yvonne/web-desktop`.
        if canonical.starts_with(&self.root) {
            Ok(canonical)
        } else {
            Err(FsError::Denied(format!(
                "`{rel}` is outside the file browser root"
            )))
        }
    }

    /// Canonical path back to a root-relative, `/`-separated one.
    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// Clean a client path and reject the shapes that should never be accepted even
/// before the canonicalize check: absolute paths, `..`, and embedded NULs.
fn normalise(rel: &str) -> Result<String, FsError> {
    if rel.contains('\0') {
        return Err(FsError::BadRequest("path contains a NUL byte".to_owned()));
    }

    // Decode percent-escapes manually? No: axum already percent-decodes query
    // values, so what arrives here is the real path.
    let trimmed = rel.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }

    let mut parts = Vec::new();
    for part in trimmed.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(FsError::Denied(
                    "`..` is not allowed in a file browser path".to_owned(),
                ));
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

#[derive(Debug, Deserialize)]
pub struct PathQuery {
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct DownloadQuery {
    #[serde(default)]
    pub path: String,
}

async fn list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
) -> Result<Json<DirListing>, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    let dir = root.resolve(&q.path)?;

    let meta = tokio::fs::metadata(&dir)
        .await
        .map_err(|e| FsError::from_io(&e, &q.path))?;
    if !meta.is_dir() {
        return Err(FsError::BadRequest(format!(
            "`{}` is not a directory",
            q.path
        )));
    }

    let rel = root.relative(&dir);
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut read = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| FsError::from_io(&e, &q.path))?;

    while let Some(entry) = read
        .next_entry()
        .await
        .map_err(|e| FsError::from_io(&e, &q.path))?
    {
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        if let Some(converted) = describe(root, &entry).await {
            entries.push(converted);
        }
    }

    // Directories first, then case-insensitive by name.
    entries.sort_by(|a, b| {
        let rank = |e: &FsEntry| match e.kind {
            EntryKind::Dir => 0,
            _ => 1,
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let parent = if rel.is_empty() {
        None
    } else {
        Some(match rel.rsplit_once('/') {
            Some((head, _)) => head.to_owned(),
            None => String::new(),
        })
    };

    Ok(Json(DirListing {
        path: rel,
        parent,
        entries,
        truncated,
    }))
}

/// Build an [`FsEntry`], following symlinks only far enough to tell whether they
/// stay inside the root. `None` means the metadata could not be read at all.
async fn describe(root: &FsRoot, entry: &tokio::fs::DirEntry) -> Option<FsEntry> {
    let name = entry.file_name().to_string_lossy().into_owned();
    let link_meta = tokio::fs::symlink_metadata(entry.path()).await.ok()?;
    let is_symlink = link_meta.file_type().is_symlink();

    // Follow symlinks to classify them, but only trust the result when the
    // target is inside the root.
    let (kind, size, blocked) = if is_symlink {
        match tokio::fs::metadata(entry.path()).await {
            Ok(target_meta) => {
                let inside = tokio::fs::canonicalize(entry.path())
                    .await
                    .map(|p| p.starts_with(root.root()))
                    .unwrap_or(false);
                if !inside {
                    (EntryKind::Other, 0, true)
                } else if target_meta.is_dir() {
                    (EntryKind::Dir, 0, false)
                } else if target_meta.is_file() {
                    (EntryKind::File, target_meta.len(), false)
                } else {
                    (EntryKind::Other, 0, false)
                }
            }
            // A dangling symlink is not an error, just not very useful.
            Err(_) => (EntryKind::Other, 0, false),
        }
    } else if link_meta.is_dir() {
        (EntryKind::Dir, 0, false)
    } else if link_meta.is_file() {
        (EntryKind::File, link_meta.len(), false)
    } else {
        (EntryKind::Other, 0, false)
    };

    let path = root.relative(entry.path().as_path());

    Some(FsEntry {
        hidden: name.starts_with('.'),
        name,
        path,
        kind,
        size,
        modified: modified_millis(link_meta.modified().ok()),
        readonly: link_meta.permissions().readonly(),
        blocked,
    })
}

async fn preview(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
) -> Result<Json<FilePreview>, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    let path = root.resolve(&q.path)?;

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| FsError::from_io(&e, &q.path))?;
    if meta.is_dir() {
        return Err(FsError::BadRequest(format!("`{}` is a directory", q.path)));
    }
    if !meta.is_file() {
        return Err(FsError::BadRequest(format!(
            "`{}` is not a regular file",
            q.path
        )));
    }

    let entry = entry_for(root, &path, &meta);
    let body = read_preview(&path, meta.len()).await?;

    Ok(Json(FilePreview { entry, body }))
}

/// Read at most [`MAX_PREVIEW_BYTES`], then decide whether the bytes are text.
async fn read_preview(path: &Path, size: u64) -> Result<PreviewBody, FsError> {
    use tokio::io::AsyncReadExt;

    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| FsError::from_io(&e, &path.to_string_lossy()))?;

    let limit = MAX_PREVIEW_BYTES.min(size) as usize;
    let mut buf = Vec::with_capacity(limit.min(64 * 1024));
    file.take(MAX_PREVIEW_BYTES)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| FsError::from_io(&e, &path.to_string_lossy()))?;

    let mime = guess_mime(path);
    let sniff = &buf[..buf.len().min(SNIFF_BYTES)];
    let looks_binary = sniff.contains(&0) || std::str::from_utf8(&buf).is_err();

    if looks_binary {
        return Ok(PreviewBody::Binary { mime });
    }

    Ok(PreviewBody::Text {
        content: String::from_utf8_lossy(&buf).into_owned(),
        truncated: size > MAX_PREVIEW_BYTES,
    })
}

async fn download(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DownloadQuery>,
) -> Result<Response, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    let path = root.resolve(&q.path)?;

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| FsError::from_io(&e, &q.path))?;
    if !meta.is_file() {
        return Err(FsError::BadRequest(format!(
            "`{}` is not a regular file",
            q.path
        )));
    }
    if meta.len() > MAX_DOWNLOAD_BYTES {
        return Err(FsError::TooLarge(format!(
            "`{}` is {} bytes; the download limit is {MAX_DOWNLOAD_BYTES}",
            q.path,
            meta.len()
        )));
    }

    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| FsError::from_io(&e, &q.path))?;

    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        // Strip anything that could break out of the quoted filename.
        let safe: String = name.replace(['"', '\\', '\r', '\n'], "");
        if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{safe}\"")) {
            headers.insert(header::CONTENT_DISPOSITION, value);
        }
    }
    Ok(response)
}

fn entry_for(root: &FsRoot, path: &Path, meta: &std::fs::Metadata) -> FsEntry {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    FsEntry {
        hidden: name.starts_with('.'),
        path: root.relative(path),
        name,
        kind: if meta.is_dir() {
            EntryKind::Dir
        } else if meta.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        },
        size: meta.len(),
        modified: modified_millis(meta.modified().ok()),
        readonly: meta.permissions().readonly(),
        blocked: false,
    }
}

fn modified_millis(time: Option<std::time::SystemTime>) -> Option<i64> {
    let millis = time?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    i64::try_from(millis).ok()
}

/// Best-effort MIME guess from the extension, so a binary preview can say
/// something more useful than "binary".
fn guess_mime(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    };
    mime.to_owned()
}

/// Every failure the filesystem API can produce, with the status code and JSON
/// body the frontend expects.
#[derive(Debug)]
pub enum FsError {
    BadRequest(String),
    Denied(String),
    NotFound(String),
    TooLarge(String),
    ServiceUnavailable(String),
    Internal(String),
}

impl FsError {
    fn fs_disabled() -> Self {
        Self::ServiceUnavailable(
            "the file browser is disabled (set WEB_DESKTOP_ROOT or unset WEB_DESKTOP_FS=off)"
                .to_owned(),
        )
    }

    /// Map an io error onto a status without leaking whether a path exists when
    /// the answer would be "outside the root" anyway.
    fn from_io(err: &std::io::Error, rel: &str) -> Self {
        use std::io::ErrorKind;
        match err.kind() {
            ErrorKind::NotFound => Self::NotFound(format!("`{rel}` does not exist")),
            ErrorKind::PermissionDenied => Self::Denied(format!("permission denied for `{rel}`")),
            ErrorKind::InvalidInput => Self::BadRequest(format!("invalid path `{rel}`")),
            // A client mistake, not a server fault: `notes.txt/more` and the
            // like must not turn into a 500.
            ErrorKind::NotADirectory => Self::BadRequest(format!("`{rel}` is not a directory")),
            ErrorKind::IsADirectory => Self::BadRequest(format!("`{rel}` is a directory")),
            ErrorKind::DirectoryNotEmpty => Self::BadRequest(format!("`{rel}` is not empty")),
            _ => Self::Internal(format!("filesystem error for `{rel}`: {err}")),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Denied(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::BadRequest(m)
            | Self::Denied(m)
            | Self::NotFound(m)
            | Self::TooLarge(m)
            | Self::ServiceUnavailable(m)
            | Self::Internal(m) => m,
        }
    }
}

impl IntoResponse for FsError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = self.message().to_owned();
        if status.is_server_error() {
            tracing::warn!("fs: {status}: {message}");
        } else {
            tracing::debug!("fs: {status}: {message}");
        }
        (status, Json(ApiError { error: message })).into_response()
    }
}

/// Unused today, but `Component` is what a stricter `normalise` would use; kept
/// so the import does not drift when the write endpoints land.
#[allow(dead_code)]
fn is_plain_component(part: &str) -> bool {
    Path::new(part)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}
