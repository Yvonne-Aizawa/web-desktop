//! An outbound proxy, so a served app can read data from a host that does not send
//! CORS headers.
//!
//! # Why this exists, and why it is fenced
//!
//! A served app runs in the browser, so a `fetch` to a third party only works if that
//! party opts in with `Access-Control-Allow-Origin`. Asking the server instead
//! sidesteps CORS — and hands any app (or any page that can reach this server) a
//! request-forgery tool pointed at whatever it names. `?url=` without a policy is the
//! classic SSRF hole: cloud metadata at `169.254.169.254`, other services on loopback,
//! everything on the LAN, all reachable from this machine.
//!
//! So the policy is the feature:
//!
//! * **Off until hosts are named** in `WEB_DESKTOP_PROXY_HOSTS`. A listed host also
//!   allows its subdomains.
//! * **Private, loopback, link-local and metadata addresses are refused** even for an
//!   allowed host, after resolving the name — so an internal DNS entry pointing at the
//!   LAN does not sneak through. `WEB_DESKTOP_PROXY_ALLOW_PRIVATE=on` lifts that, for
//!   proxying to something of your own.
//! * **Every redirect hop is checked again**, because an allowed host can redirect
//!   anywhere.
//! * **GET only**, no request bodies, and the caller's headers are never forwarded.
//!   Adding other methods later means widening [`Request`] deliberately; the policy
//!   checks apply to whatever it becomes.
//! * Bounded: a response size cap, a timeout, and a redirect limit.
//!
//! What this does *not* defend against: a name that resolves to a public address for
//! the check and a private one for the connection (DNS rebinding between the two). The
//! server binds to loopback and the allow-list is explicit, so that is a deliberate
//! omission rather than an oversight.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use shared::{ProxyInfo, fs::ApiError};

use crate::AppState;

/// Largest upstream body returned.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Whole-request budget for the upstream call.
pub const TIMEOUT_SECS: u64 = 10;
/// Redirect hops before giving up.
const MAX_REDIRECTS: usize = 3;

/// The proxy's configuration, derived from the environment once at startup.
#[derive(Debug, Clone, Default)]
pub struct Proxy {
    hosts: Vec<String>,
    allow_private: bool,
}

impl Proxy {
    /// `WEB_DESKTOP_PROXY_HOSTS=api.example.com,example.org` and, optionally,
    /// `WEB_DESKTOP_PROXY_ALLOW_PRIVATE=on`.
    pub fn from_env() -> Self {
        let hosts = std::env::var("WEB_DESKTOP_PROXY_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(|host| host.trim().trim_start_matches("*.").to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect::<Vec<_>>();

        let allow_private = std::env::var("WEB_DESKTOP_PROXY_ALLOW_PRIVATE")
            .map(|v| {
                matches!(
                    v.to_ascii_lowercase().as_str(),
                    "on" | "1" | "true" | "yes" | "enabled"
                )
            })
            .unwrap_or(false);

        Self {
            hosts,
            allow_private,
        }
    }

    pub fn info(&self) -> ProxyInfo {
        ProxyInfo {
            hosts: self.hosts.clone(),
            allow_private: self.allow_private,
            max_bytes: MAX_BYTES,
            timeout_secs: TIMEOUT_SECS,
        }
    }

    /// Is this host allowed? A listed host also covers its subdomains, so
    /// `example.org` allows `cdn.example.org` but not `example.org.evil.test`.
    pub fn host_allowed(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.hosts.iter().any(|allowed| {
            // Tolerate a `*.` written in the config, so the rule holds however the
            // list was built.
            let allowed = allowed.trim_start_matches("*.");
            !allowed.is_empty()
                && (host == allowed
                    || host
                        .strip_suffix(allowed)
                        .is_some_and(|prefix| prefix.ends_with('.')))
        })
    }

    /// Resolve a host and refuse anything that is not a public address.
    ///
    /// Resolution happens here rather than implicitly in the client so a name that
    /// points somewhere private is refused, not just a literal IP.
    async fn resolve_public(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, ProxyError> {
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|err| ProxyError::Upstream(format!("cannot resolve `{host}`: {err}")))?
            .collect();

        if addrs.is_empty() {
            return Err(ProxyError::Upstream(format!(
                "`{host}` resolved to nothing"
            )));
        }
        if !self.allow_private
            && let Some(blocked) = addrs.iter().find(|addr| !is_public(addr.ip()))
        {
            return Err(ProxyError::Denied(format!(
                "`{host}` resolves to {}, which is a private or local address; \
                 set WEB_DESKTOP_PROXY_ALLOW_PRIVATE=on to allow that",
                blocked.ip()
            )));
        }
        Ok(addrs)
    }

    /// Everything that has to be true before a request goes out, for one hop.
    async fn check(&self, url: &reqwest::Url) -> Result<(), ProxyError> {
        match url.scheme() {
            "http" | "https" => {}
            other => {
                return Err(ProxyError::BadRequest(format!(
                    "only http and https are proxied, not `{other}`"
                )));
            }
        }

        let host = url
            .host_str()
            .ok_or_else(|| ProxyError::BadRequest("the url has no host".to_owned()))?;
        if !self.host_allowed(host) {
            return Err(ProxyError::Denied(format!(
                "`{host}` is not in WEB_DESKTOP_PROXY_HOSTS"
            )));
        }

        let port = url.port_or_known_default().unwrap_or(80);
        // Resolving is the point: it is what stops a name from standing in for a
        // private address.
        drop(self.resolve_public(host, port).await?);
        Ok(())
    }
}

/// Whether an address is somewhere a proxied request must not go.
///
/// Written out rather than reached for from a crate: this is the check the whole
/// feature rests on, and it should be readable and unit-tested.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            // An IPv4-mapped address is still an IPv4 address.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            is_public_v6(v6)
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        // carrier-grade NAT
        || (a == 100 && (64..128).contains(&b))
        // benchmarking
        || (a == 198 && (b == 18 || b == 19))
        // reserved, and the broadcast-ish top of the space
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        // site-local, deprecated but still a thing
        || (ip.segments()[0] & 0xffc0) == 0xfec0
        // documentation
        || (ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8))
}

#[derive(Debug, Deserialize)]
pub struct ProxyQuery {
    /// Absolute URL to fetch.
    pub url: String,
}

/// `GET /api/proxy?url=…`
pub async fn get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ProxyQuery>,
) -> Result<Response, ProxyError> {
    let proxy = state.proxy.as_ref().ok_or_else(|| {
        ProxyError::Denied(
            "the proxy is off; name the hosts you want in WEB_DESKTOP_PROXY_HOSTS".to_owned(),
        )
    })?;

    let mut url = reqwest::Url::parse(&query.url)
        .map_err(|err| ProxyError::BadRequest(format!("`{}` is not a url: {err}", query.url)))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        // Redirects are followed by hand so each hop can be checked.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| ProxyError::Internal(format!("no http client: {err}")))?;

    // GET only, no caller headers, no cookies, no body.
    let request = Request::Get;

    for hop in 0..=MAX_REDIRECTS {
        proxy.check(&url).await?;
        tracing::debug!("proxy: {} {}", request.method(), url);

        let response = client
            .request(request.method(), url.clone())
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    ProxyError::Timeout(format!("{url} took longer than {TIMEOUT_SECS}s"))
                } else {
                    ProxyError::Upstream(format!("{url}: {err}"))
                }
            })?;

        if response.status().is_redirection() {
            if hop == MAX_REDIRECTS {
                return Err(ProxyError::Upstream(format!(
                    "{url} redirected more than {MAX_REDIRECTS} times"
                )));
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    ProxyError::Upstream(format!("{url} redirected without a Location"))
                })?;
            url = url.join(location).map_err(|err| {
                ProxyError::Upstream(format!("{url} redirected somewhere unusable: {err}"))
            })?;
            continue;
        }

        return finish(response).await;
    }

    Err(ProxyError::Upstream("too many redirects".to_owned()))
}

/// Turn the upstream response into ours, capped and with its content type.
async fn finish(response: reqwest::Response) -> Result<Response, ProxyError> {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| HeaderValue::from_str(value).ok())
        .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"));

    if let Some(length) = response.content_length()
        && length > MAX_BYTES
    {
        return Err(ProxyError::TooLarge(format!(
            "the response is {length} bytes; the limit is {MAX_BYTES}"
        )));
    }

    // Read with a ceiling instead of trusting `content-length`, which a server may
    // omit or lie about.
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|err| ProxyError::Upstream(format!("reading the body: {err}")))?;
        if body.len() as u64 + chunk.len() as u64 > MAX_BYTES {
            return Err(ProxyError::TooLarge(format!(
                "the response is larger than {MAX_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }

    let mut out = (status, body).into_response();
    let headers = out.headers_mut();
    headers.insert(header::CONTENT_TYPE, content_type);
    // A proxy response is not the shell's to cache, and the same URL can answer
    // differently for different callers.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(out)
}

/// What the proxy was asked to do. Today that is only `GET`; widening this is the
/// single place a future POST would have to be allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    Get,
}

impl Request {
    fn method(self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
        }
    }
}

#[derive(Debug)]
pub enum ProxyError {
    BadRequest(String),
    Denied(String),
    TooLarge(String),
    Timeout(String),
    Upstream(String),
    Internal(String),
}

impl ProxyError {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Denied(_) => StatusCode::FORBIDDEN,
            Self::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::BadRequest(m)
            | Self::Denied(m)
            | Self::TooLarge(m)
            | Self::Timeout(m)
            | Self::Upstream(m)
            | Self::Internal(m) => m,
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = self.message().to_owned();
        if status.is_server_error() || status == StatusCode::BAD_GATEWAY {
            tracing::warn!("proxy: {status}: {message}");
        } else {
            tracing::debug!("proxy: {status}: {message}");
        }
        (status, Json(ApiError { error: message })).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(hosts: &str, allow_private: bool) -> Proxy {
        Proxy {
            hosts: hosts
                .split(',')
                .filter(|h| !h.is_empty())
                .map(|h| h.trim().to_ascii_lowercase())
                .collect(),
            allow_private,
        }
    }

    #[test]
    fn nothing_is_allowed_until_a_host_is_named() {
        let p = proxy("", false);
        assert!(!p.host_allowed("example.org"));
        assert!(!p.info().enabled());
    }

    #[test]
    fn a_listed_host_covers_its_subdomains_only() {
        let p = proxy("example.org", false);
        assert!(p.host_allowed("example.org"));
        assert!(p.host_allowed("EXAMPLE.org"));
        assert!(p.host_allowed("cdn.example.org"));
        assert!(p.host_allowed("example.org."));
        // the classic suffix mistake
        assert!(!p.host_allowed("example.org.evil.test"));
        assert!(!p.host_allowed("notexample.org"));
        assert!(!p.host_allowed("example.com"));
    }

    #[test]
    fn a_wildcard_prefix_is_accepted_as_a_plain_host() {
        let p = proxy("*.example.org", false);
        assert!(p.host_allowed("api.example.org"));
        assert!(p.host_allowed("example.org"));
    }

    #[test]
    fn private_and_local_addresses_are_not_public() {
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254", // cloud metadata
            "0.0.0.0",
            "100.64.0.1", // carrier-grade NAT
            "198.18.0.1", // benchmarking
            "240.0.0.1",  // reserved
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1", // v4-mapped loopback
        ] {
            let ip: IpAddr = address.parse().unwrap();
            assert!(!is_public(ip), "{address} should not be public");
        }
    }

    #[test]
    fn ordinary_public_addresses_are_public() {
        for address in ["1.1.1.1", "93.184.216.34", "2606:4700:4700::1111"] {
            let ip: IpAddr = address.parse().unwrap();
            assert!(is_public(ip), "{address} should be public");
        }
    }

    #[test]
    fn only_http_urls_are_proxied() {
        let p = proxy("example.org", false);
        for bad in [
            "file:///etc/passwd",
            "data:text/plain,hi",
            "ftp://example.org/x",
        ] {
            let url = reqwest::Url::parse(bad).unwrap();
            let err = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(p.check(&url))
                .unwrap_err();
            assert!(matches!(err, ProxyError::BadRequest(_)), "{bad} -> {err:?}");
        }
    }

    #[test]
    fn a_host_outside_the_list_is_refused_before_any_request() {
        let p = proxy("example.org", false);
        let url = reqwest::Url::parse("https://evil.test/steal").unwrap();
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(p.check(&url))
            .unwrap_err();
        assert!(matches!(err, ProxyError::Denied(_)), "{err:?}");
    }

    #[test]
    fn an_allowed_name_pointing_at_loopback_is_still_refused() {
        let p = proxy("localhost", false);
        let url = reqwest::Url::parse("http://localhost:8080/").unwrap();
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(p.check(&url))
            .unwrap_err();
        assert!(matches!(err, ProxyError::Denied(_)), "{err:?}");
        assert!(
            err.message().contains("private or local"),
            "{}",
            err.message()
        );
    }
}
