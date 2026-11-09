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

#[cfg(test)]
mod tests {
    use super::{image_mime, looks_like_image};

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
