//! Talking to the axum backend. All payload types come from `shared`.

use gloo_net::http::Request;
use shared::{API_CATALOG, API_HEALTH, Catalog, Health};

pub async fn fetch_catalog() -> Result<Catalog, String> {
    Request::get(API_CATALOG)
        .send()
        .await
        .map_err(|e| format!("catalog request failed: {e}"))?
        .json::<Catalog>()
        .await
        .map_err(|e| format!("catalog decode failed: {e}"))
}

pub async fn fetch_health() -> Result<Health, String> {
    Request::get(API_HEALTH)
        .send()
        .await
        .map_err(|e| format!("health request failed: {e}"))?
        .json::<Health>()
        .await
        .map_err(|e| format!("health decode failed: {e}"))
}
