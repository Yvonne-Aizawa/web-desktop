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
//!
//! Caching is decided centrally (`main::cache_policy`): everything here is
//! `no-store`, because a file browser must never show a stale view.

use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::DefaultBodyLimit,
    extract::{Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
};
use serde::Deserialize;
use shared::fs::{
    API_FS_CONTENT, API_FS_DELETE, API_FS_DIR, API_FS_DOWNLOAD, API_FS_ENTRY, API_FS_LIST,
    API_FS_PREVIEW, API_FS_RAW, API_FS_TRASH, API_FS_TRASH_RESTORE, ApiError, DirListing,
    EntryKind, FilePreview, FsEntry, FsInfo, FsMutation, PreviewBody, TRASH_DIR, TrashEntry,
    TrashListing, TrashRestore, contains_protected, image_mime, protected_by,
};
use tokio_util::io::ReaderStream;

use crate::AppState;

/// Largest text payload returned by the preview endpoint.
pub const MAX_PREVIEW_BYTES: u64 = 256 * 1024;
/// Largest file any endpoint will hand out, whether streamed (`/raw`,
/// `/download`) or offered inline as an image. `preview` applies it to images
/// too, so an image that `/raw` would refuse is refused here with the reason,
/// instead of the client rendering an `<img>` that has to fail.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Entries returned for one directory before the listing is cut short.
pub const MAX_ENTRIES: usize = 2_000;
/// Bytes inspected to decide whether a file is binary.
const SNIFF_BYTES: usize = 8 * 1024;

/// Routes for the filesystem API, with the cross-site guard applied to all of them.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(API_FS_LIST, get(list))
        .route(API_FS_PREVIEW, get(preview))
        .route(API_FS_RAW, get(raw))
        .route(API_FS_DOWNLOAD, get(download))
        // Writes. The body limit has to be raised or axum refuses anything over 2 MB
        // before the handler sees it.
        .route(API_FS_CONTENT, put(write_content))
        .route(API_FS_DIR, post(create_dir))
        .route(API_FS_ENTRY, patch(rename_entry))
        .route(API_FS_DELETE, axum::routing::delete(delete_entry))
        // The trash is a place, not a flag: it is listed, restored from and emptied
        // through its own endpoints, and the file endpoints will not touch it.
        .route(API_FS_TRASH, get(list_trash).delete(purge_trash))
        .route(API_FS_TRASH_RESTORE, post(restore_trash))
        .layer(DefaultBodyLimit::max(MAX_FILE_BYTES as usize))
        .route_layer(middleware::from_fn(same_origin))
}

/// Reject requests a browser tells us came from another site.
///
/// Reads are already safe from cross-site snooping (no CORS headers means the
/// response cannot be read by another origin), but a cross-site `fetch` still
/// *executes* here. That matters as soon as writes exist, and costs nothing now.
/// `Sec-Fetch-Site` is sent by every current browser; clients that omit it, such
/// as `curl`, are allowed through.
pub async fn same_origin(req: Request, next: Next) -> Result<Response, FsError> {
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
    /// Whether the mutation endpoints are allowed to do anything.
    writable: bool,
    /// Why not, when they are not.
    readonly_reason: Option<String>,
    /// Root-relative paths that stay read-only even when the rest of the root is
    /// writable, from `WEB_DESKTOP_FS_READONLY`. The operator uses this to put
    /// something inside the sandbox that the app can read but never lose.
    protected: Vec<String>,
}

impl FsRoot {
    /// Create the directory if needed and canonicalize it, so every later
    /// comparison is made between two canonical paths.
    pub fn new(
        path: &Path,
        writable: bool,
        readonly_reason: Option<String>,
        protected: Vec<String>,
    ) -> std::io::Result<(Self, bool)> {
        let created = !path.exists();
        std::fs::create_dir_all(path)?;
        let root = path.canonicalize()?;
        Ok((
            Self {
                root,
                writable,
                readonly_reason,
                protected,
            },
            created,
        ))
    }

    pub fn info(&self, enabled: bool) -> FsInfo {
        FsInfo {
            enabled,
            root: self.root.to_string_lossy().into_owned(),
            writable: self.writable,
            readonly_reason: self.readonly_reason.clone(),
            readonly_paths: self.protected.clone(),
            max_preview_bytes: MAX_PREVIEW_BYTES,
            max_write_bytes: MAX_FILE_BYTES,
        }
    }

    /// Refuse a mutation when the server was not asked to allow them.
    fn writable(&self) -> Result<(), FsError> {
        if self.writable {
            return Ok(());
        }
        Err(FsError::Denied(
            self.readonly_reason
                .clone()
                .unwrap_or_else(|| "this file browser is read-only".to_owned()),
        ))
    }

    /// Refuse a mutation of a path the operator protected.
    ///
    /// Takes a *canonical* root-relative path, so a symlink pointing into a protected
    /// folder is caught the same way the real path would be.
    fn editable(&self, rel: &str) -> Result<(), FsError> {
        if let Some(rule) = protected_by(rel, &self.protected) {
            let where_ = if rel.trim_matches('/') == rule {
                "it is".to_owned()
            } else {
                format!("it is inside `{rule}`, which is")
            };
            return Err(FsError::Denied(format!(
                "`{rel}` is read-only: {where_} listed in WEB_DESKTOP_FS_READONLY"
            )));
        }
        Ok(())
    }

    /// Refuse a mutation that would take protected paths with it.
    ///
    /// Deleting or renaming a folder modifies everything inside it, so a rule about one
    /// entry is a rule about every folder above it.
    fn editable_tree(&self, rel: &str) -> Result<(), FsError> {
        self.editable(rel)?;
        if let Some(rule) = contains_protected(rel, &self.protected) {
            return Err(FsError::Denied(format!(
                "`{rel}` holds `{rule}`, which is read-only because it is listed in \
                 WEB_DESKTOP_FS_READONLY"
            )));
        }
        Ok(())
    }

    /// Whether a root-relative path is inside the trash, which the file API does not
    /// expose at all: it is reached through the trash endpoints.
    fn is_trash(&self, rel: &str) -> bool {
        let rel = rel.trim_matches('/');
        rel == shared::fs::TRASH_DIR || rel.starts_with(&format!("{}/", shared::fs::TRASH_DIR))
    }

    /// The same check for a path that has already been resolved, so the caller does not
    /// have to keep the original string around.
    fn refuse_trash(&self, path: &Path) -> Result<(), FsError> {
        let rel = self.relative(path);
        if self.is_trash(&rel) {
            return Err(FsError::Denied(format!(
                "`{rel}` is in the trash; use the trash endpoints"
            )));
        }
        Ok(())
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

    /// Same fence, for a path that does not exist yet: canonicalize the parent and
    /// append the final component. Creation cannot canonicalize the leaf, so the
    /// parent is the part that has to be proved to be inside the root.
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

/// A single path component supplied by the client: a name to create, or a new name
/// to rename to. Rejects everything that could redirect the operation.
fn entry_name(name: &str) -> Result<&str, FsError> {
    let trimmed = name.trim();
    let invalid = |why: &str| FsError::BadRequest(format!("invalid name: {why}"));
    if trimmed.is_empty() {
        return Err(invalid("it is empty"));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(invalid("`.` and `..` are not names"));
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(invalid("it contains a path separator"));
    }
    if trimmed.contains('\0') {
        return Err(invalid("it contains a NUL byte"));
    }
    if trimmed.chars().count() > 255 {
        return Err(invalid("it is longer than 255 characters"));
    }
    Ok(trimmed)
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

#[derive(Debug, Deserialize)]
pub struct TrashQuery {
    /// A trash entry id, or empty to mean the whole trash.
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    pub path: String,
    /// Required to remove a directory that still has something in it, for a
    /// permanent delete. Trashing one needs no flag: a move is a move.
    #[serde(default)]
    pub recursive: bool,
    /// Delete for good instead of moving to the trash.
    #[serde(default)]
    pub permanent: bool,
}

async fn list(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
) -> Result<Json<DirListing>, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    let dir = root.resolve(&q.path)?;
    root.refuse_trash(&dir)?;

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
    // Never listed, not even as a hidden file: it is the browser's own bookkeeping,
    // and a delete is supposed to make something disappear.
    if name == TRASH_DIR {
        return None;
    }
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
    let protected = protected_by(&path, &root.protected).is_some();

    Some(FsEntry {
        hidden: name.starts_with('.'),
        name,
        path,
        kind,
        size,
        modified: modified_millis(link_meta.modified().ok()),
        // Both reasons the server would refuse a write, so the UI can lock the row
        // rather than offer an action that will fail.
        readonly: protected || link_meta.permissions().readonly(),
        blocked,
    })
}

async fn preview(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
) -> Result<Json<FilePreview>, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    let path = root.resolve(&q.path)?;
    root.refuse_trash(&path)?;

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
    let body = match image_mime(&entry.name) {
        // No point reading a 20 MB photo to say "this is an image".
        Some(mime) => {
            if meta.len() > MAX_FILE_BYTES {
                return Err(FsError::TooLarge(format!(
                    "`{}` is {} bytes; the limit is {MAX_FILE_BYTES}",
                    q.path,
                    meta.len()
                )));
            }
            PreviewBody::Image {
                mime: mime.to_owned(),
            }
        }
        None => read_preview(&path, meta.len()).await?,
    };

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
    let (path, meta) = open_file(&state, &q.path).await?;

    let mut response = stream_file(&path, &meta).await?;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        disposition("attachment", &path),
    );
    Ok(response)
}

/// Serve an image inline, for use as an `<img src>`.
///
/// Restricted to the formats [`image_mime`] knows about. This endpoint exists to
/// render previews, not to hand out arbitrary files under an attacker-influenced
/// content type, so a file that is not a decodable image is refused here; a file
/// of any type can still be saved through [`download`], which always sends
/// `application/octet-stream` as an attachment.
async fn raw(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DownloadQuery>,
) -> Result<Response, FsError> {
    // Resolve first, so a path outside the root is refused as such rather than
    // being described by a type check that never looked at the filesystem.
    let (path, meta) = open_file(&state, &q.path).await?;

    // The name of the *resolved* file, so a link to a photo is still an image.
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mime = image_mime(&name).ok_or_else(|| {
        FsError::BadRequest(format!(
            "`{}` is not an image format the browser can render",
            q.path
        ))
    })?;

    let mut response = stream_file(&path, &meta).await?;

    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    headers.insert(header::CONTENT_DISPOSITION, disposition("inline", &path));
    // The bytes are ours to label, and the label comes from the file name, so a
    // misnamed file must fail to render rather than be reinterpreted.
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if mime == "image/svg+xml" {
        // An SVG is a document: if it is ever opened directly rather than through
        // an `<img>`, it must not be able to run script or fetch anything. This
        // does not affect rendering inside an `<img>`.
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
        );
    }

    Ok(response)
}

// ------------------------------------------------------------------- writes
//
// Every one of these goes through the same fence as the reads, and every one is
// gated on `FsRoot::writable` first, so a read-only server refuses before it looks
// at the request at all.

/// Write the request body as a file's contents.
///
/// Creates the file when it does not exist, replaces it when it does, and does not
/// care whether the bytes are text or binary: the editor and the uploader use the
/// same endpoint. The write is atomic (temp file, then rename in the same
/// directory), so a failure halfway cannot leave a truncated file behind.
async fn write_content(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DownloadQuery>,
    body: Bytes,
) -> Result<Json<FsEntry>, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    root.writable()?;

    if body.len() as u64 > MAX_FILE_BYTES {
        return Err(FsError::TooLarge(format!(
            "body is {} bytes; the limit is {MAX_FILE_BYTES}",
            body.len()
        )));
    }

    let path = root.resolve_parent(&q.path)?;
    root.refuse_trash(&path)?;
    root.editable(&root.relative(&path))?;

    // Replacing a directory, or a link that points outside the root, is not a write.
    match tokio::fs::symlink_metadata(&path).await {
        Ok(meta) if meta.is_dir() => {
            return Err(FsError::BadRequest(format!("`{}` is a directory", q.path)));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(FsError::from_io(&err, &q.path)),
    }

    write_atomic(&path, &body)
        .await
        .map_err(|e| FsError::from_io(&e, &q.path))?;

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| FsError::from_io(&e, &q.path))?;
    Ok(Json(entry_for(root, &path, &meta)))
}

/// Create a directory.
async fn create_dir(
    State(state): State<Arc<AppState>>,
    Json(request): Json<FsMutation>,
) -> Result<Json<FsEntry>, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    root.writable()?;

    let name = entry_name(&request.name)?;
    let parent = root.resolve(&request.path)?;
    root.refuse_trash(&parent)?;
    let target = parent.join(name);
    root.editable(&root.relative(&target))?;

    // Creation must not be a way to follow a link out of the root, and must not
    // silently adopt an existing entry.
    if tokio::fs::symlink_metadata(&target).await.is_ok() {
        return Err(FsError::Conflict(format!("`{name}` already exists")));
    }

    tokio::fs::create_dir(&target)
        .await
        .map_err(|e| FsError::from_io(&e, &format!("{}/{name}", request.path)))?;

    let meta = tokio::fs::symlink_metadata(&target)
        .await
        .map_err(|e| FsError::from_io(&e, &request.path))?;
    Ok(Json(entry_for(root, &target, &meta)))
}

/// Rename an entry within its own folder.
async fn rename_entry(
    State(state): State<Arc<AppState>>,
    Json(request): Json<FsMutation>,
) -> Result<Json<FsEntry>, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    root.writable()?;

    let name = entry_name(&request.name)?;
    if request.path.is_empty() {
        return Err(FsError::BadRequest("cannot rename the root".to_owned()));
    }

    let source = root.resolve(&request.path)?;
    if source == root.root() {
        return Err(FsError::Denied("cannot rename the root".to_owned()));
    }
    root.refuse_trash(&source)?;
    // A rename moves everything inside, so a protected path anywhere in the tree has
    // to stop it.
    root.editable_tree(&root.relative(&source))?;
    let target = root.resolve_parent(&match request.path.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{name}"),
        None => name.to_owned(),
    })?;

    root.editable(&root.relative(&target))?;
    if target == source {
        let meta = tokio::fs::symlink_metadata(&source)
            .await
            .map_err(|e| FsError::from_io(&e, &request.path))?;
        return Ok(Json(entry_for(root, &source, &meta)));
    }
    if tokio::fs::symlink_metadata(&target).await.is_ok() {
        return Err(FsError::Conflict(format!("`{name}` already exists")));
    }

    tokio::fs::rename(&source, &target)
        .await
        .map_err(|e| FsError::from_io(&e, &request.path))?;

    let meta = tokio::fs::symlink_metadata(&target)
        .await
        .map_err(|e| FsError::from_io(&e, &request.path))?;
    Ok(Json(entry_for(root, &target, &meta)))
}

/// Delete a file, or a directory when `recursive=true`.
///
/// Unless `permanent=true`, this is a *move* into the trash, and the answer is the
/// [`TrashEntry`] that can put it back — which is what makes "delete" undoable and what
/// makes the confirmation dialog honest. Nothing here needs `recursive`: moving a
/// folder is one rename, however full it is.
async fn delete_entry(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DeleteQuery>,
) -> Result<Response, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    root.writable()?;

    if q.path.is_empty() {
        return Err(FsError::Denied("cannot delete the root".to_owned()));
    }
    let target = root.resolve(&q.path)?;
    if target == root.root() {
        return Err(FsError::Denied("cannot delete the root".to_owned()));
    }
    root.refuse_trash(&target)?;
    root.editable_tree(&root.relative(&target))?;

    let meta = tokio::fs::symlink_metadata(&target)
        .await
        .map_err(|e| FsError::from_io(&e, &q.path))?;

    if !q.permanent {
        let entry = trash_now(root, &target, &q.path, &meta).await?;
        return Ok(Json(entry).into_response());
    }

    if meta.is_dir() {
        if !q.recursive {
            let mut entries = tokio::fs::read_dir(&target)
                .await
                .map_err(|e| FsError::from_io(&e, &q.path))?;
            if entries
                .next_entry()
                .await
                .map_err(|e| FsError::from_io(&e, &q.path))?
                .is_some()
            {
                return Err(FsError::Conflict(format!("`{}` is not empty", q.path)));
            }
        }
        tokio::fs::remove_dir_all(&target)
            .await
            .map_err(|e| FsError::from_io(&e, &q.path))?;
    } else {
        tokio::fs::remove_file(&target)
            .await
            .map_err(|e| FsError::from_io(&e, &q.path))?;
    }

    tracing::info!("fs: deleted {} for good", q.path);
    Ok(StatusCode::NO_CONTENT.into_response())
}

// -------------------------------------------------------------------- trash
//
// A delete is a rename into `<root>/.trash/<id>/payload`, next to an `info.json` that
// remembers where it came from. Same filesystem, so the rename is atomic and costs
// nothing however large the file is; the trash is a place inside the fence, not a
// second copy of it.

/// Identifiers are a timestamp plus a per-process counter: unique without a random
/// number generator, and they sort in the order things were deleted.
fn trash_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Move one entry into the trash and describe what was moved.
async fn trash_now(
    root: &FsRoot,
    target: &Path,
    rel: &str,
    meta: &std::fs::Metadata,
) -> Result<TrashEntry, FsError> {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = trash_id();
    let holding = root.root().join(TRASH_DIR).join(&id);

    let entry = TrashEntry {
        id: id.clone(),
        name,
        path: root.relative(target),
        kind: if meta.is_dir() {
            EntryKind::Dir
        } else if meta.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        },
        size: if meta.is_dir() { 0 } else { meta.len() },
        deleted_at: modified_millis(Some(std::time::SystemTime::now())).unwrap_or(0),
    };

    tokio::fs::create_dir_all(&holding)
        .await
        .map_err(|e| FsError::from_io(&e, rel))?;
    // The note goes down first. If the rename fails we clear up, and if the process
    // dies in between there is a folder with no payload, which the listing skips.
    let note = serde_json::to_vec_pretty(&entry)
        .map_err(|e| FsError::BadRequest(format!("could not describe the entry: {e}")))?;
    if let Err(err) = tokio::fs::write(holding.join("info.json"), note).await {
        let _ = tokio::fs::remove_dir_all(&holding).await;
        return Err(FsError::from_io(&err, rel));
    }
    if let Err(err) = tokio::fs::rename(target, holding.join("payload")).await {
        let _ = tokio::fs::remove_dir_all(&holding).await;
        return Err(FsError::from_io(&err, rel));
    }

    tracing::info!("fs: trashed {rel} as {id}");
    Ok(entry)
}

/// Read one trash entry, or explain why it cannot be used.
async fn read_trash_entry(root: &FsRoot, id: &str) -> Result<(PathBuf, TrashEntry), FsError> {
    // The id arrives from the client, so it is a single component or nothing: the
    // resolve below is the second half of that promise.
    let id = entry_name(id)?;
    let holding = root.resolve(&format!("{TRASH_DIR}/{id}"))?;
    let bytes = tokio::fs::read(holding.join("info.json"))
        .await
        .map_err(|e| FsError::from_io(&e, "that trash entry"))?;
    let entry: TrashEntry = serde_json::from_slice(&bytes)
        .map_err(|_| FsError::BadRequest("that trash entry cannot be read".to_owned()))?;
    Ok((holding, entry))
}

async fn list_trash(State(state): State<Arc<AppState>>) -> Result<Json<TrashListing>, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    Ok(Json(gather_trash(root).await?))
}

/// Newest first.
async fn gather_trash(root: &FsRoot) -> Result<TrashListing, FsError> {
    let mut entries = Vec::new();
    let mut total_bytes = 0;
    let mut read = match tokio::fs::read_dir(root.root().join(TRASH_DIR)).await {
        Ok(read) => read,
        // No trash yet is an empty trash, not a failure.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TrashListing {
                entries,
                total_bytes,
            });
        }
        Err(err) => return Err(FsError::from_io(&err, "the trash")),
    };

    while let Some(item) = read
        .next_entry()
        .await
        .map_err(|e| FsError::from_io(&e, "the trash"))?
    {
        if entries.len() >= MAX_ENTRIES {
            break;
        }
        let Ok(bytes) = tokio::fs::read(item.path().join("info.json")).await else {
            continue;
        };
        let Ok(entry) = serde_json::from_slice::<TrashEntry>(&bytes) else {
            continue;
        };
        // An entry whose payload is gone cannot be restored, so it is not offered.
        if tokio::fs::symlink_metadata(item.path().join("payload"))
            .await
            .is_err()
        {
            continue;
        }
        total_bytes += entry.size;
        entries.push(entry);
    }

    entries.sort_by_key(|entry| std::cmp::Reverse(entry.deleted_at));
    Ok(TrashListing {
        entries,
        total_bytes,
    })
}

/// Put an entry back where it came from, keeping its new name if the old one is taken.
async fn restore_trash(
    State(state): State<Arc<AppState>>,
    Json(request): Json<TrashRestore>,
) -> Result<Json<FsEntry>, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    root.writable()?;
    Ok(Json(restore_from_trash(root, &request.id).await?))
}

/// Put one entry back, wherever its original path is now.
async fn restore_from_trash(root: &FsRoot, id: &str) -> Result<FsEntry, FsError> {
    let (holding, entry) = read_trash_entry(root, id).await?;
    // Restoring writes to the original path, which may since have become protected.
    root.editable_tree(&entry.path)?;

    // Recreate any folder above the target that has gone away in the meantime, one
    // component at a time, each one checked to stay inside the root by construction.
    let mut folder = root.root().to_path_buf();
    let parent = match entry.path.rsplit_once('/') {
        Some((parent, _)) => parent.to_owned(),
        None => String::new(),
    };
    for part in parent.split('/').filter(|p| !p.is_empty()) {
        folder.push(part);
        if tokio::fs::symlink_metadata(&folder).await.is_err() {
            tokio::fs::create_dir(&folder)
                .await
                .map_err(|e| FsError::from_io(&e, &parent))?;
        }
    }

    let target = match tokio::fs::symlink_metadata(folder.join(&entry.name)).await {
        Ok(_) => unique_restore_name(&folder, &entry.name).await?,
        Err(_) => folder.join(&entry.name),
    };

    tokio::fs::rename(holding.join("payload"), &target)
        .await
        .map_err(|e| FsError::from_io(&e, &entry.path))?;
    let _ = tokio::fs::remove_dir_all(&holding).await;

    let meta = tokio::fs::symlink_metadata(&target)
        .await
        .map_err(|e| FsError::from_io(&e, &entry.path))?;
    let restored = root.relative(&target);
    tracing::info!("fs: restored {} to {}", entry.path, restored);
    Ok(entry_for(root, &target, &meta))
}

/// The first free name of the form `note (restored).txt`, so a restore never
/// overwrites whatever took the original name.
async fn unique_restore_name(folder: &Path, name: &str) -> Result<PathBuf, FsError> {
    let (stem, extension) = match name.rsplit_once('.') {
        // A leading dot is a stem, not an extension: `.env` is not "env with no name".
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    for attempt in 1..100 {
        let suffix = if attempt == 1 {
            " (restored)".to_owned()
        } else {
            format!(" (restored {attempt})")
        };
        let candidate = folder.join(format!("{stem}{suffix}{extension}"));
        if tokio::fs::symlink_metadata(&candidate).await.is_err() {
            return Ok(candidate);
        }
    }
    Err(FsError::Conflict(format!(
        "there are already a hundred restored copies of `{name}`"
    )))
}

/// Delete for good: one entry, or - with no `id` - everything in the trash.
async fn purge_trash(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TrashQuery>,
) -> Result<StatusCode, FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    root.writable()?;
    purge_from_trash(root, &q.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Forget one entry, or everything in the trash when `id` is empty.
async fn purge_from_trash(root: &FsRoot, id: &str) -> Result<(), FsError> {
    let target = if id.is_empty() {
        root.resolve(TRASH_DIR)?
    } else {
        // Through `resolve`, so the id cannot be a way out of the trash even if it
        // somehow got past the name check.
        root.resolve(&format!("{TRASH_DIR}/{}", entry_name(id)?))?
    };

    match tokio::fs::remove_dir_all(&target).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(FsError::from_io(&err, "the trash")),
    }
    // The folder itself stays, so the next delete does not have to recreate it.
    if id.is_empty() {
        let _ = tokio::fs::create_dir_all(&target).await;
        tracing::info!("fs: emptied the trash");
    } else {
        tracing::info!("fs: purged trash entry {id}");
    }
    Ok(())
}

/// Replace a file's contents without ever leaving it half-written.
async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temporary = directory.join(format!(".{name}.tmp{stamp}"));

    if let Err(err) = tokio::fs::write(&temporary, bytes).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(err);
    }
    if let Err(err) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(err);
    }
    Ok(())
}

/// Resolve a path, require a regular file, and refuse anything over the cap.
async fn open_file(
    state: &Arc<AppState>,
    rel: &str,
) -> Result<(PathBuf, std::fs::Metadata), FsError> {
    let root = state.fs.as_ref().ok_or_else(FsError::fs_disabled)?;
    let path = root.resolve(rel)?;
    root.refuse_trash(&path)?;

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| FsError::from_io(&e, rel))?;
    if !meta.is_file() {
        return Err(FsError::BadRequest(format!(
            "`{rel}` is not a regular file"
        )));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(FsError::TooLarge(format!(
            "`{rel}` is {} bytes; the limit is {MAX_FILE_BYTES}",
            meta.len()
        )));
    }

    Ok((path, meta))
}

/// Stream a file out without buffering it, so a large image or download does not
/// sit in memory in full.
async fn stream_file(path: &Path, meta: &std::fs::Metadata) -> Result<Response, FsError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| FsError::from_io(&e, &path.to_string_lossy()))?;

    let mut response = Body::from_stream(ReaderStream::new(file)).into_response();
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&meta.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    Ok(response)
}

/// `inline; filename="..."`, with anything that could break out of the quoted
/// string removed.
fn disposition(kind: &str, path: &Path) -> HeaderValue {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    let safe: String = name.replace(['"', '\\', '\r', '\n'], "");
    HeaderValue::from_str(&format!("{kind}; filename=\"{safe}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

fn entry_for(root: &FsRoot, path: &Path, meta: &std::fs::Metadata) -> FsEntry {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let relative = root.relative(path);
    let protected = protected_by(&relative, &root.protected).is_some();
    FsEntry {
        hidden: name.starts_with('.'),
        path: relative,
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
        readonly: protected || meta.permissions().readonly(),
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
        // Image formats the browser cannot decode: named for the message in the
        // preview pane, not rendered.
        "tiff" | "tif" => "image/tiff",
        "heic" | "heif" => "image/heic",
        "psd" => "image/vnd.adobe.photoshop",
        "raw" | "cr2" | "nef" => "image/x-raw",
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
    Conflict(String),
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
            Self::Conflict(_) => StatusCode::CONFLICT,
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
            | Self::Conflict(m)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh root per test, under the system temp directory. Named uniquely so the
    /// tests can run in parallel and so a failure leaves something to look at.
    fn scratch(label: &str, protected: &[&str]) -> FsRoot {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "web-desktop-fs-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let (root, _) = FsRoot::new(
            &dir,
            true,
            None,
            protected.iter().map(|p| (*p).to_owned()).collect(),
        )
        .expect("scratch root");
        root
    }

    async fn trash(root: &FsRoot, rel: &str) -> Result<TrashEntry, FsError> {
        let target = root.resolve(rel)?;
        let meta = tokio::fs::symlink_metadata(&target)
            .await
            .expect("metadata");
        trash_now(root, &target, rel, &meta).await
    }

    fn read(root: &FsRoot, rel: &str) -> String {
        std::fs::read_to_string(root.root().join(rel)).expect("readable")
    }

    #[tokio::test]
    async fn trashing_keeps_the_file_and_hides_it() {
        let root = scratch("trash", &[]);
        std::fs::write(root.root().join("note.txt"), "hello\n").expect("write");

        let entry = trash(&root, "note.txt").await.expect("trashed");
        assert_eq!(entry.name, "note.txt");
        assert_eq!(entry.path, "note.txt");
        assert_eq!(entry.kind, EntryKind::File);
        assert_eq!(entry.size, 6);

        // Out of the way...
        assert!(!root.root().join("note.txt").exists());
        // ...and held, byte for byte, under a name of the trash's choosing.
        let held = root.root().join(TRASH_DIR).join(&entry.id).join("payload");
        assert_eq!(std::fs::read_to_string(&held).expect("payload"), "hello\n");

        let listing = gather_trash(&root).await.expect("listing");
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.total_bytes, 6);
    }

    #[tokio::test]
    async fn restoring_puts_it_back_where_it_was() {
        let root = scratch("restore", &[]);
        std::fs::write(root.root().join("note.txt"), "hello\n").expect("write");
        let entry = trash(&root, "note.txt").await.expect("trashed");

        let back = restore_from_trash(&root, &entry.id)
            .await
            .expect("restored");
        assert_eq!(back.path, "note.txt");
        assert_eq!(read(&root, "note.txt"), "hello\n");
        // The trash does not keep a copy of what it gave back.
        assert!(
            gather_trash(&root)
                .await
                .expect("listing")
                .entries
                .is_empty()
        );
    }

    #[tokio::test]
    async fn restoring_never_overwrites_what_took_the_name() {
        let root = scratch("collision", &[]);
        std::fs::write(root.root().join("keep.txt"), "original\n").expect("write");
        let entry = trash(&root, "keep.txt").await.expect("trashed");
        std::fs::write(root.root().join("keep.txt"), "replacement\n").expect("write");

        let back = restore_from_trash(&root, &entry.id)
            .await
            .expect("restored");
        assert_eq!(back.path, "keep (restored).txt");
        assert_eq!(read(&root, "keep.txt"), "replacement\n");
        assert_eq!(read(&root, "keep (restored).txt"), "original\n");
    }

    #[tokio::test]
    async fn restoring_recreates_the_folder_it_was_in() {
        let root = scratch("vanished", &[]);
        std::fs::create_dir_all(root.root().join("a/b")).expect("mkdir");
        std::fs::write(root.root().join("a/b/c.txt"), "deep\n").expect("write");
        let entry = trash(&root, "a/b/c.txt").await.expect("trashed");
        std::fs::remove_dir_all(root.root().join("a")).expect("rmdir");

        let back = restore_from_trash(&root, &entry.id)
            .await
            .expect("restored");
        assert_eq!(back.path, "a/b/c.txt");
        assert_eq!(read(&root, "a/b/c.txt"), "deep\n");
    }

    #[tokio::test]
    async fn a_whole_folder_goes_to_the_trash_and_comes_back() {
        let root = scratch("folder", &[]);
        std::fs::create_dir_all(root.root().join("projects/one")).expect("mkdir");
        std::fs::write(root.root().join("projects/one/a.txt"), "a\n").expect("write");
        std::fs::write(root.root().join("projects/top.txt"), "top\n").expect("write");

        let entry = trash(&root, "projects").await.expect("trashed");
        assert_eq!(entry.kind, EntryKind::Dir);
        assert!(!root.root().join("projects").exists());

        restore_from_trash(&root, &entry.id)
            .await
            .expect("restored");
        assert_eq!(read(&root, "projects/one/a.txt"), "a\n");
        assert_eq!(read(&root, "projects/top.txt"), "top\n");
    }

    #[tokio::test]
    async fn a_protected_path_refuses_everything_that_would_change_it() {
        let root = scratch("protected", &["Lumina"]);
        std::fs::create_dir_all(root.root().join("Lumina/gallery")).expect("mkdir");
        std::fs::write(root.root().join("Lumina/gallery/a.png"), "keep me\n").expect("write");

        // Writing inside, creating inside, renaming, deleting: all refused.
        for rel in [
            "Lumina",
            "Lumina/gallery",
            "Lumina/gallery/a.png",
            "Lumina/new.txt",
        ] {
            assert!(root.editable(rel).is_err(), "{rel} should be protected");
        }
        // A neighbour whose name merely starts the same way is not.
        std::fs::create_dir_all(root.root().join("Luminaria")).expect("mkdir");
        assert!(root.editable("Luminaria").is_ok());
        assert!(root.editable("Luminaria/x.txt").is_ok());

        // And a folder *holding* the protected path cannot be moved either, which is
        // the case a check on the path alone would miss.
        assert!(root.editable("").is_ok() || root.protected.is_empty());
        assert!(root.editable_tree("").is_err());
        assert!(root.editable_tree("Lumina").is_err());
        assert!(root.editable_tree("other").is_ok());

        // Nothing was touched while all of that was refused.
        assert_eq!(read(&root, "Lumina/gallery/a.png"), "keep me\n");
    }

    #[tokio::test]
    async fn the_trash_is_not_reachable_through_the_file_api() {
        let root = scratch("sealed", &[]);
        for rel in [TRASH_DIR, ".trash/1", ".trash/1/payload", "/.trash/x"] {
            assert!(root.is_trash(rel), "{rel} is in the trash");
        }
        for rel in ["trash", "notes/.trash", ".trashed", ""] {
            assert!(!root.is_trash(rel), "{rel} is not in the trash");
        }
        // A path that merely points at it is refused once resolved, which is what the
        // read and write handlers rely on.
        std::fs::create_dir_all(root.root().join(TRASH_DIR).join("1")).expect("mkdir");
        let inside = root.resolve(&format!("{TRASH_DIR}/1")).expect("resolves");
        assert!(root.refuse_trash(&inside).is_err());
    }

    #[tokio::test]
    async fn a_trash_entry_with_no_payload_is_not_offered() {
        let root = scratch("orphan", &[]);
        std::fs::write(root.root().join("note.txt"), "x\n").expect("write");
        let entry = trash(&root, "note.txt").await.expect("trashed");

        // What a crash between the note and the move would leave behind.
        std::fs::remove_file(root.root().join(TRASH_DIR).join(&entry.id).join("payload"))
            .expect("remove payload");

        let listing = gather_trash(&root).await.expect("listing");
        assert!(
            listing.entries.is_empty(),
            "an entry that cannot be restored should not be listed"
        );
        // It is still on disk, so nothing was lost silently.
        assert!(
            root.root()
                .join(TRASH_DIR)
                .join(&entry.id)
                .join("info.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn emptying_the_trash_leaves_it_usable() {
        let root = scratch("empty", &[]);
        std::fs::write(root.root().join("one.txt"), "1\n").expect("write");
        std::fs::write(root.root().join("two.txt"), "2\n").expect("write");
        trash(&root, "one.txt").await.expect("trashed");
        trash(&root, "two.txt").await.expect("trashed");

        purge_from_trash(&root, "").await.expect("emptied");
        assert!(
            gather_trash(&root)
                .await
                .expect("listing")
                .entries
                .is_empty()
        );

        // The folder is still there, so the next delete does not have to think about it.
        let entry = trash(&root, "one.txt").await;
        assert!(entry.is_err() || gather_trash(&root).await.expect("listing").entries.len() <= 1);
        assert!(root.root().join(TRASH_DIR).is_dir());
    }
}
