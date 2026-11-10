//! Reading data from a host that does not send CORS headers.
//!
//! From the browser this is impossible: a served app is a wasm module in a page, so a
//! cross-origin `fetch` is readable only if the other end opts in. So the request goes
//! to the shell's own server instead, which makes it and hands back the body:
//!
//! ```no_run
//! # async fn example() {
//! #[derive(serde::Deserialize)]
//! struct Thing { id: u32 }
//!
//! let things: Vec<Thing> = app_sdk::fetch::json("https://api.example.com/things").await.unwrap();
//! let logo = app_sdk::fetch::bytes("https://api.example.com/logo.png").await.unwrap();
//! # }
//! ```
//!
//! Which hosts are reachable is the *server's* decision, not this module's: the proxy
//! only talks to hosts named in `WEB_DESKTOP_PROXY_HOSTS`, and never to private or
//! loopback addresses unless the operator says so. An app asking for anything else gets
//! a 403 with the reason, which is why there is no allow-list of its own here.

use gloo_net::http::Request;
use serde::de::DeserializeOwned;
use shared::API_PROXY;

use crate::url::encode;

/// The proxy URL for an absolute URL, for anyone who would rather use their own client.
///
/// Percent-encodes everything outside the unreserved set, so a query string inside
/// `url` arrives intact.
pub fn proxy_url(url: &str) -> String {
    format!("{API_PROXY}?url={}", encode(url))
}

/// Fetch a URL through the server and return its bytes.
pub async fn bytes(url: &str) -> Result<Vec<u8>, String> {
    let response = Request::get(&proxy_url(url))
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;

    let status = response.status();
    if !(200..300).contains(&status) {
        // The server explains refusals as JSON; fall back to the status if not.
        let message = match response.json::<shared::fs::ApiError>().await {
            Ok(body) => body.error,
            Err(_) => format!("HTTP {status}"),
        };
        return Err(message);
    }

    response
        .binary()
        .await
        .map_err(|err| format!("could not read the response: {err}"))
}

/// Fetch a URL through the server and decode it as JSON.
pub async fn json<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    let bytes = bytes(url).await?;
    serde_json::from_slice(&bytes).map_err(|err| format!("unexpected JSON from {url}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::proxy_url;

    #[test]
    fn a_url_survives_the_query_string() {
        assert_eq!(
            proxy_url("https://api.example.com/a?b=c&d=e"),
            "/api/proxy?url=https%3A%2F%2Fapi.example.com%2Fa%3Fb%3Dc%26d%3De"
        );
    }
}
