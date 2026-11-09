//! Types for the real-filesystem browser.
//!
//! The browser never touches the disk: it asks the axum backend, which is fenced
//! inside a single root directory (see `server::fs::FsRoot`). Everything the
//! frontend sees is in this module.
//!
//! Paths on the wire are always **relative to the root**, `/`-separated, with
//! `""` meaning the root itself. Absolute paths never appear in an API request,
//! which is what keeps the fence easy to reason about.

use serde::{Deserialize, Serialize};

/// `GET` with `?path=<relative>` -> [`DirListing`].
pub const API_FS_LIST: &str = "/api/fs/list";
/// `GET` with `?path=<relative>` -> [`FilePreview`].
pub const API_FS_PREVIEW: &str = "/api/fs/preview";
/// `GET` with `?path=<relative>` -> raw bytes with `Content-Disposition`.
pub const API_FS_DOWNLOAD: &str = "/api/fs/download";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Dir,
    File,
    /// Sockets, FIFOs, devices, and symlinks that point outside the root.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsEntry {
    /// File name only, no separators.
    pub name: String,
    /// Root-relative path, `/`-separated. `""` is the root directory itself.
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Unix milliseconds, when the platform reports it.
    pub modified: Option<i64>,
    /// Dotfile. The UI hides these behind a toggle.
    pub hidden: bool,
    pub readonly: bool,
    /// True for a symlink whose target lands outside the root: visible, but
    /// opening it will be refused.
    pub blocked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirListing {
    /// Root-relative path of the directory that was listed.
    pub path: String,
    /// Root-relative path of its parent, or `None` at the root.
    pub parent: Option<String>,
    pub entries: Vec<FsEntry>,
    /// The directory held more entries than the server was willing to list.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePreview {
    pub entry: FsEntry,
    pub body: PreviewBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PreviewBody {
    /// Decoded as UTF-8; `truncated` means only the first chunk was sent.
    Text { content: String, truncated: bool },
    /// Not shown in the DOM. `mime` is a best guess from the extension so the UI
    /// can say something more useful than "binary".
    Binary { mime: String },
}

/// What the backend is willing to expose. Advertised once, in [`crate::Catalog`],
/// so the frontend can gray out features it cannot use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsInfo {
    pub enabled: bool,
    /// Absolute path of the sandbox root, for display in Settings.
    pub root: String,
    /// Always `false` today. The write endpoints are not implemented; this flag
    /// exists so the frontend can hide mutation UI instead of probing for it.
    pub writable: bool,
    pub max_preview_bytes: u64,
}

impl FsInfo {
    /// Used by the client when it cannot reach the backend, and by the server
    /// when the filesystem API is switched off.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            root: String::new(),
            writable: false,
            max_preview_bytes: 0,
        }
    }
}

/// Error body for every non-2xx API response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
