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
/// `GET` with `?path=<relative>` -> the bytes of an image, inline, for use as an
/// `<img src>`. Only the formats [`image_mime`] knows about are served.
pub const API_FS_RAW: &str = "/api/fs/raw";
/// `GET` with `?path=<relative>` -> raw bytes with `Content-Disposition`.
pub const API_FS_DOWNLOAD: &str = "/api/fs/download";

/// `PUT` with `?path=<relative>`: write the request body as the file's contents.
/// Creates the file if it does not exist, replaces it if it does, and is the same
/// endpoint for a text save and a binary upload — the body is opaque bytes.
pub const API_FS_CONTENT: &str = "/api/fs/content";
/// `POST` with `{"path": <dir>, "name": <name>}`: create a directory.
pub const API_FS_DIR: &str = "/api/fs/dir";
/// `PATCH` with `{"path": <entry>, "name": <new name>}`: rename within its folder.
pub const API_FS_ENTRY: &str = "/api/fs/entry";
/// `DELETE` with `?path=<entry>[&recursive=true]`: move a file or a directory to the
/// trash, and answer with the [`TrashEntry`] so the client can offer an undo.
///
/// `&permanent=true` deletes it for good instead, which is there for a script or an app
/// that means it; the desktop itself always trashes, so its irreversible delete is the
/// one in the trash view, where the user can see what they are losing.
pub const API_FS_DELETE: &str = "/api/fs/entry";
/// `GET` -> [`TrashListing`]. `DELETE` with `?id=<id>` removes one entry for good;
/// with no `id` at all it empties the trash.
pub const API_FS_TRASH: &str = "/api/fs/trash";
/// `POST` with `{"id": <id>}` -> the restored [`FsEntry`].
pub const API_FS_TRASH_RESTORE: &str = "/api/fs/trash/restore";

/// The folder deletes are moved into, inside the root.
///
/// The browser's own bookkeeping rather than the user's files: the file API will not
/// list, read or modify anything under it, and the trash API is the only way in. That
/// keeps "deleted" and "not deleted" from leaking into each other.
pub const TRASH_DIR: &str = ".trash";

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

/// One thing that was deleted, and can be put back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashEntry {
    /// Opaque handle for restoring or purging this entry.
    pub id: String,
    /// The name it had, which is not the name it has in the trash.
    pub name: String,
    /// Root-relative path it was deleted from, and would be restored to.
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Unix milliseconds: when it was deleted.
    pub deleted_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashListing {
    /// Newest first.
    pub entries: Vec<TrashEntry>,
    /// What the trash is holding, so the UI can say how much emptying would free.
    pub total_bytes: u64,
}

/// Request body for [`API_FS_TRASH_RESTORE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashRestore {
    pub id: String,
}

/// Request body for [`API_FS_DIR`] and [`API_FS_ENTRY`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsMutation {
    /// Root-relative path of the folder (for `dir`) or the entry (for `entry`).
    pub path: String,
    /// The new name, without separators.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PreviewBody {
    /// Decoded as UTF-8; `truncated` means only the first chunk was sent.
    Text { content: String, truncated: bool },
    /// A format the browser can decode, so the client points an `<img>` at
    /// [`API_FS_RAW`]. No bytes travel in this response.
    Image { mime: String },
    /// Not shown as an image. `mime` is a best guess from the extension so the UI
    /// can say something more useful than "binary".
    Binary { mime: String },
}

/// MIME type for a file name whose extension is an image format browsers render
/// inline, or `None` if it is not one.
///
/// One source of truth for both sides: the server uses it to choose between
/// [`PreviewBody::Image`] and [`PreviewBody::Binary`] and to set the
/// `Content-Type` of [`API_FS_RAW`], and the client uses it to decide which rows
/// deserve a thumbnail.
///
/// Formats browsers cannot decode (TIFF, HEIC, PSD, ...) are deliberately absent.
/// They fall through to [`PreviewBody::Binary`] and get an honest message instead
/// of a broken `<img>`; see [`looks_like_image`] for the icon.
pub fn image_mime(name: &str) -> Option<&'static str> {
    let mime = match extension(name).as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        _ => return None,
    };
    Some(mime)
}

/// True for names that are an image in the ordinary sense, *including* formats
/// the browser cannot decode.
///
/// Separate from [`image_mime`] because the two answer different questions: one
/// decides whether an `<img>` can be pointed at the file, the other whether the
/// listing should show a picture icon. A TIFF should look like a picture, and then
/// the preview pane explains why it cannot be shown.
pub fn looks_like_image(name: &str) -> bool {
    image_mime(name).is_some()
        || matches!(
            extension(name).as_str(),
            "tiff"
                | "tif"
                | "heic"
                | "heif"
                | "psd"
                | "xcf"
                | "eps"
                | "ai"
                | "raw"
                | "cr2"
                | "nef"
                | "arw"
                | "dng"
                | "jxl"
                | "jp2"
        )
}

/// Lowercased extension, or `""` when the name has none. A leading dot means the
/// whole name is treated as a stem (`.hidden` has no extension).
fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// What the backend is willing to expose. Advertised once, in [`crate::Catalog`],
/// so the frontend can gray out features it cannot use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsInfo {
    pub enabled: bool,
    /// Absolute path of the sandbox root, for display in Settings.
    pub root: String,
    /// Whether the write endpoints will accept anything. The frontend hides its
    /// editing affordances when this is false rather than failing on save.
    pub writable: bool,
    /// Why writes are off, when they are: shown to the user instead of a silent
    /// read-only pane.
    #[serde(default)]
    pub readonly_reason: Option<String>,
    /// Root-relative paths this server refuses to modify, from
    /// `WEB_DESKTOP_FS_READONLY`. Shown as locked in the UI, and enforced by the
    /// backend whatever the UI does.
    #[serde(default)]
    pub readonly_paths: Vec<String>,
    pub max_preview_bytes: u64,
    /// Largest body [`API_FS_CONTENT`] will accept, so the editor can refuse before
    /// uploading something the server would reject.
    #[serde(default)]
    pub max_write_bytes: u64,
}

impl FsInfo {
    /// Used by the client when it cannot reach the backend, and by the server
    /// when the filesystem API is switched off.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            root: String::new(),
            writable: false,
            readonly_reason: Some("the file browser is switched off on the server".to_owned()),
            readonly_paths: Vec::new(),
            max_preview_bytes: 0,
            max_write_bytes: 0,
        }
    }

    /// Whether the frontend should offer editing at all.
    pub fn can_write(&self) -> bool {
        self.enabled && self.writable
    }
}

/// Whether `child` is `parent` itself or lies inside it.
///
/// Both are root-relative with no leading slash, and `""` is the root, which contains
/// everything. Compared a component at a time so that a rule protecting `Lumina` does
/// not also protect `Luminaria`.
fn within(child: &str, parent: &str) -> bool {
    parent.is_empty() || child == parent || child.starts_with(&format!("{parent}/"))
}

/// The rule out of `protected` that covers `path`, if any.
///
/// A path is protected when it is one of the listed paths or lies inside one. Empty
/// rules are ignored, so an unset or empty setting protects nothing.
pub fn protected_by<'a>(path: &str, protected: &'a [String]) -> Option<&'a str> {
    let path = path.trim_matches('/');
    protected
        .iter()
        .map(|rule| rule.trim_matches('/'))
        .find(|rule| !rule.is_empty() && within(path, rule))
}

/// The rule out of `protected` that sits at or inside `path`, if any.
///
/// The other direction, and the reason it is needed: deleting `projects` has to be
/// refused when `projects/lumina` is protected, even though `projects` itself is not.
/// Anything that removes or moves a directory has to ask this, not just [`protected_by`].
pub fn contains_protected<'a>(path: &str, protected: &'a [String]) -> Option<&'a str> {
    let path = path.trim_matches('/');
    protected
        .iter()
        .map(|rule| rule.trim_matches('/'))
        .find(|rule| !rule.is_empty() && within(rule, path))
}

/// Error body for every non-2xx API response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::{FsInfo, image_mime, looks_like_image};

    use super::{contains_protected, protected_by};

    fn rules() -> Vec<String> {
        vec!["Lumina".to_owned(), "notes/keep.md".to_owned()]
    }

    #[test]
    fn a_protected_path_covers_itself_and_its_contents() {
        for path in ["Lumina", "/Lumina", "Lumina/", "Lumina/gallery/a.png"] {
            assert_eq!(protected_by(path, &rules()), Some("Lumina"), "{path}");
        }
        assert_eq!(
            protected_by("notes/keep.md", &rules()),
            Some("notes/keep.md")
        );
    }

    #[test]
    fn protection_is_compared_a_component_at_a_time() {
        // The trap: a prefix is not a folder.
        for path in ["Luminaria", "Lumina.txt", "notes", "notes/other.md", ""] {
            assert_eq!(protected_by(path, &rules()), None, "{path}");
        }
    }

    #[test]
    fn deleting_a_folder_that_holds_something_protected_is_refused() {
        assert_eq!(contains_protected("notes", &rules()), Some("notes/keep.md"));
        assert_eq!(contains_protected("", &rules()), Some("Lumina"));
        assert_eq!(contains_protected("Lumina", &rules()), Some("Lumina"));
        assert_eq!(contains_protected("other", &rules()), None);
    }

    #[test]
    fn no_rules_protect_nothing() {
        let none: Vec<String> = Vec::new();
        assert_eq!(protected_by("anything/at/all", &none), None);
        assert_eq!(contains_protected("anything", &none), None);
    }

    #[test]
    fn can_write_needs_both_flags() {
        let mut info = FsInfo {
            enabled: true,
            root: "/tmp".to_owned(),
            writable: true,
            readonly_reason: None,
            readonly_paths: Vec::new(),
            max_preview_bytes: 1,
            max_write_bytes: 1,
        };
        assert!(info.can_write());
        info.writable = false;
        assert!(!info.can_write());
        info.writable = true;
        info.enabled = false;
        assert!(!info.can_write());
    }

    #[test]
    fn a_disabled_file_browser_explains_itself() {
        let info = FsInfo::disabled();
        assert!(!info.can_write());
        assert!(info.readonly_reason.is_some());
    }

    #[test]
    fn known_image_formats() {
        assert_eq!(image_mime("photo.PNG"), Some("image/png"));
        assert_eq!(image_mime("photo.jpeg"), Some("image/jpeg"));
        assert_eq!(image_mime("a.b.webp"), Some("image/webp"));
        assert_eq!(image_mime("logo.svg"), Some("image/svg+xml"));
    }

    #[test]
    fn formats_browsers_cannot_decode_are_not_offered() {
        for name in ["scan.tiff", "scan.tif", "photo.heic", "art.psd", "raw.cr2"] {
            assert_eq!(
                image_mime(name),
                None,
                "{name} should fall through to binary"
            );
            assert!(
                looks_like_image(name),
                "{name} should still look like an image"
            );
        }
    }

    #[test]
    fn names_without_a_usable_extension() {
        for name in ["README", ".hidden-png", "archive.tar.gz", "dir.d/file", ""] {
            assert_eq!(image_mime(name), None, "{name} has no image extension");
            assert!(!looks_like_image(name), "{name} is not an image");
        }
    }

    #[test]
    fn plain_files_are_not_images() {
        for name in ["note.txt", "main.rs", "data.json", "png", "logo.svgz"] {
            assert!(!looks_like_image(name), "{name} is not an image");
            assert_eq!(image_mime(name), None, "{name} is not decodable");
        }
    }
}
