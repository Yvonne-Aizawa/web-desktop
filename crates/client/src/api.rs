//! Talking to the axum backend. All payload types come from `shared`.

use gloo_net::http::{Request, Response};
use serde::de::DeserializeOwned;
use shared::fs::{
    API_FS_CONTENT, API_FS_DELETE, API_FS_DIR, API_FS_DOWNLOAD, API_FS_ENTRY, API_FS_LIST,
    API_FS_PREVIEW, API_FS_RAW, ApiError, DirListing, FilePreview, FsEntry, FsMutation,
};
use shared::{API_CATALOG, API_HEALTH, Catalog, Health};

use crate::format::encode_query;

pub async fn fetch_catalog() -> Result<Catalog, String> {
    get_json(API_CATALOG).await
}

pub async fn fetch_health() -> Result<Health, String> {
    get_json(API_HEALTH).await
}

/// List one directory inside the file browser root.
pub async fn list_dir(path: &str) -> Result<DirListing, String> {
    get_json(&format!("{}?path={}", API_FS_LIST, encode_query(path))).await
}

/// Fetch a text or binary preview of one file.
pub async fn preview_file(path: &str) -> Result<FilePreview, String> {
    get_json(&format!("{}?path={}", API_FS_PREVIEW, encode_query(path))).await
}

/// URL for the download endpoint, for use in an `<a href>`.
pub fn download_url(path: &str) -> String {
    format!("{}?path={}", API_FS_DOWNLOAD, encode_query(path))
}

/// URL for the inline image endpoint, for use in an `<img src>`.
///
/// Only image formats the browser can decode are served here; anything else is
/// refused, so this is safe to point an `<img>` at without checking the type
/// first (the server has the final say).
pub fn raw_url(path: &str) -> String {
    format!("{}?path={}", API_FS_RAW, encode_query(path))
}

// ------------------------------------------------------------------- writes

/// Write `bytes` as a file's contents, creating it if needed.
pub async fn write_file_bytes(path: &str, bytes: &[u8]) -> Result<FsEntry, String> {
    let url = format!("{}?path={}", API_FS_CONTENT, encode_query(path));
    let request = Request::put(&url)
        .header("content-type", "application/octet-stream")
        .body(js_sys::Uint8Array::from(bytes))
        .map_err(|err| format!("could not build the request: {err}"))?;
    send(request).await
}

/// Write a text file. Separate from [`write_file_bytes`] only so the body does not
/// have to be converted by hand at every call site.
pub async fn write_file(path: &str, text: &[u8]) -> Result<FsEntry, String> {
    write_file_bytes(path, text).await
}

/// Create a directory inside `parent`.
pub async fn create_dir(parent: &str, name: &str) -> Result<FsEntry, String> {
    let body = FsMutation {
        path: parent.to_owned(),
        name: name.to_owned(),
    };
    let request = Request::post(API_FS_DIR)
        .json(&body)
        .map_err(|err| format!("could not build the request: {err}"))?;
    send(request).await
}

/// Rename an entry within its own folder.
pub async fn rename_entry(path: &str, name: &str) -> Result<FsEntry, String> {
    let url = format!("{}?path={}", API_FS_ENTRY, encode_query(path));
    let body = FsMutation {
        path: path.to_owned(),
        name: name.to_owned(),
    };
    let request = Request::patch(&url)
        .json(&body)
        .map_err(|err| format!("could not build the request: {err}"))?;
    send(request).await
}

/// Delete a file, or a directory when `recursive` is set.
pub async fn delete_entry(path: &str, recursive: bool) -> Result<(), String> {
    let url = format!(
        "{}?path={}&recursive={recursive}",
        API_FS_DELETE,
        encode_query(path)
    );
    let request = Request::delete(&url)
        .build()
        .map_err(|err| format!("could not build the request: {err}"))?;
    send_empty(request).await
}

/// `GET` and decode, turning a non-2xx [`ApiError`] body into the message the UI
/// shows. Every failure the user can act on arrives as a string here.
async fn get_json<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    let request = Request::get(url)
        .build()
        .map_err(|err| format!("could not build the request: {err}"))?;
    send(request).await
}

async fn send<T: DeserializeOwned>(request: Request) -> Result<T, String> {
    let response = request
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    decode(response).await
}

/// For endpoints that answer `204 No Content`.
async fn send_empty(request: Request) -> Result<(), String> {
    let response = request
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    let status = response.status();
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(error_message(response, status).await)
}

async fn decode<T: DeserializeOwned>(response: Response) -> Result<T, String> {
    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(error_message(response, status).await);
    }
    response
        .json::<T>()
        .await
        .map_err(|err| format!("unexpected response: {err}"))
}

/// The server's own words when it sends [`ApiError`], a status code otherwise.
async fn error_message(response: Response, status: u16) -> String {
    response
        .json::<ApiError>()
        .await
        .map(|body| body.error)
        .unwrap_or_else(|_| format!("HTTP {status}"))
}
