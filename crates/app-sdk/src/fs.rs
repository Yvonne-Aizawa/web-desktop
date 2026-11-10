//! Keeping something on disk, through the shell's file API.
//!
//! An app does not get a filesystem of its own. It gets the same fenced view the Files
//! app has: paths are relative to the one root the server was pointed at, `..` and
//! absolute paths are refused there, and a root that is read-only stays read-only. So
//! these helpers can fail for reasons the user should be told about — the app is a
//! guest in a folder it may not own.
//!
//! ```no_run
//! # async fn example() {
//! // Best-effort: the folder may already exist, and the root may be read-only — in
//! // which case the write below fails with the server's own reason.
//! let _ = app_sdk::fs::create_dir("", "notes").await;
//! app_sdk::fs::write_text("notes/hello.txt", "hi").await.unwrap();
//! # }
//! ```
//!
//! Nothing here trusts the server's answer about *which* files exist: a name is a name,
//! and `list` is how an app finds out what is really there.

use gloo_net::http::Request;
use shared::fs::{
    API_FS_CONTENT, API_FS_DELETE, API_FS_DIR, API_FS_LIST, API_FS_PREVIEW, ApiError,
};

// The wire types live in `shared` so both ends agree by construction; re-exported here
// so an app can name them — a listing's entries, an entry's kind — without having to
// depend on `shared` itself.
pub use shared::fs::{
    DirListing, EntryKind, FilePreview, FsEntry, PreviewBody, TrashEntry, image_mime,
};

/// The URL an `<img src>` can point at for a file, if it is a picture the browser can
/// decode.
///
/// The path is escaped the same way every read on the file API takes it, so a name with
/// a space or a `#` in it still works.
pub fn raw_url(path: &str) -> String {
    path_url(shared::fs::API_FS_RAW, path)
}

/// The URL a download link points at: the raw bytes, as an attachment.
pub fn download_url(path: &str) -> String {
    path_url(shared::fs::API_FS_DOWNLOAD, path)
}

/// One directory's entries, newest state on disk.
pub async fn list(path: &str) -> Result<DirListing, String> {
    get_json(&path_url(API_FS_LIST, path)).await
}

/// Read a text file into a `String`.
///
/// Refuses anything the server did not decode as text — an image, or a binary — rather
/// than handing back mojibake. A file longer than the server's preview cap comes back
/// truncated, which is why the flag is returned alongside it.
pub async fn read_text(path: &str) -> Result<String, String> {
    let preview: FilePreview = get_json(&path_url(API_FS_PREVIEW, path)).await?;
    match preview.body {
        PreviewBody::Text { content, .. } => Ok(content),
        PreviewBody::Image { mime } => Err(format!("{} is an image ({mime})", preview.entry.name)),
        PreviewBody::Binary { mime } => Err(format!("{} is not text ({mime})", preview.entry.name)),
    }
}

/// Write `text` as a file's whole contents, creating it if it does not exist.
///
/// Atomic on the server: a failure partway cannot leave half a file behind.
pub async fn write_text(path: &str, text: &str) -> Result<FsEntry, String> {
    let request = Request::put(&path_url(API_FS_CONTENT, path))
        .header("content-type", "text/plain; charset=utf-8")
        .body(text)
        .map_err(|err| format!("could not build the request: {err}"))?;
    send(request).await
}

/// Write raw `bytes` as a file's whole contents, creating it if it does not exist.
///
/// This is the same endpoint as [`write_text`] with a different `content-type`, which is
/// all the server needs to tell an upload from a save. `content_type` is the app's claim
/// about the bytes — a generated image knows its own type — and the server stores the
/// bytes regardless, so a wrong guess costs a `Content-Type` on a download and nothing
/// more.
pub async fn write_bytes(path: &str, bytes: &[u8], content_type: &str) -> Result<FsEntry, String> {
    let request = Request::put(&path_url(API_FS_CONTENT, path))
        .header("content-type", content_type)
        .body(js_sys::Uint8Array::from(bytes))
        .map_err(|err| format!("could not build the request: {err}"))?;
    send(request).await
}

/// Create a directory inside `parent`.
pub async fn create_dir(parent: &str, name: &str) -> Result<FsEntry, String> {
    let body = shared::fs::FsMutation {
        path: parent.to_owned(),
        name: name.to_owned(),
    };
    let request = Request::post(API_FS_DIR)
        .json(&body)
        .map_err(|err| format!("could not build the request: {err}"))?;
    send(request).await
}

/// Move a file to the trash, and say what was moved.
///
/// A delete here is recoverable: the shell's trash view is where it is put back from,
/// which is why an app is free to offer a delete button without asking twice.
pub async fn trash(path: &str) -> Result<TrashEntry, String> {
    let request = Request::delete(&path_url(API_FS_DELETE, path))
        .build()
        .map_err(|err| format!("could not build the request: {err}"))?;
    send(request).await
}

/// `endpoint?path=<encoded>`, the shape every read on the file API takes.
fn path_url(endpoint: &str, path: &str) -> String {
    format!("{endpoint}?path={}", crate::url::encode(path))
}

async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    let request = Request::get(url)
        .build()
        .map_err(|err| format!("could not build the request: {err}"))?;
    send(request).await
}

async fn send<T: serde::de::DeserializeOwned>(request: Request) -> Result<T, String> {
    let response = request
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;

    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(response
            .json::<ApiError>()
            .await
            .map(|body| body.error)
            .unwrap_or_else(|_| format!("HTTP {status}")));
    }
    response
        .json::<T>()
        .await
        .map_err(|err| format!("unexpected response: {err}"))
}

#[cfg(test)]
mod tests {
    use super::path_url;
    use shared::fs::API_FS_LIST;

    #[test]
    fn a_path_travels_as_one_query_value() {
        assert_eq!(
            path_url(API_FS_LIST, "notes/todo.md"),
            "/api/fs/list?path=notes%2Ftodo.md"
        );
        // The root is the empty path, which is a legal thing to ask for.
        assert_eq!(path_url(API_FS_LIST, ""), "/api/fs/list?path=");
    }
}
