//! Talking to the axum backend. All payload types come from `shared`.

use gloo_net::http::Request;
use serde::de::DeserializeOwned;
use shared::fs::{
    API_FS_DOWNLOAD, API_FS_LIST, API_FS_PREVIEW, API_FS_RAW, ApiError, DirListing, FilePreview,
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

/// `GET` and decode, turning a non-2xx [`ApiError`] body into the message the UI
/// shows. Every failure the user can act on arrives as a string here.
async fn get_json<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    let response = Request::get(url)
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;

    let status = response.status();
    if !(200..300).contains(&status) {
        let message = response
            .json::<ApiError>()
            .await
            .map(|body| body.error)
            .unwrap_or_else(|_| format!("HTTP {status}"));
        return Err(message);
    }

    response
        .json::<T>()
        .await
        .map_err(|err| format!("unexpected response: {err}"))
}
