//! Password login for the web desktop.
//!
//! Authentication is optional. If `WEB_DESKTOP_PASSWORD_HASH` is not set, every
//! request is allowed and the shell is told auth is disabled. When it is set, every
//! `/api/*` request except session, login and logout must carry a valid session
//! cookie.
//!
//! The password is stored as a bcrypt hash: the plaintext is never written down, and
//! verification costs enough that guessing over HTTP is not a practical attack. The
//! session is a signed, HTTP-only cookie carrying only its own expiry — no user data,
//! so nothing to leak — signed with a key derived from `WEB_DESKTOP_SECRET` when one
//! is set. Without a secret, a random key is generated and a warning says why the
//! sessions will not survive a restart.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use cookie::{Cookie, Key, SameSite};
use shared::auth::{LoginRequest, LoginResponse, SessionResponse};
use tower_cookies::{CookieManagerLayer, Cookies};
use tracing::warn;

use crate::AppState;

/// Name of the session cookie.
const SESSION_COOKIE: &str = "wd_session";
/// Session lifetime when `WEB_DESKTOP_SESSION_DAYS` says nothing.
const DEFAULT_SESSION_DAYS: u64 = 7;

/// Password hash and cookie key. `None` in [`AppState`] means auth is off.
pub struct Auth {
    password_hash: String,
    session_seconds: i64,
    key: Key,
}

// Written by hand so a derived `Debug` cannot print the hash or the key into a log.
impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("session_seconds", &self.session_seconds)
            .finish_non_exhaustive()
    }
}

impl Auth {
    /// Read auth configuration from the environment. `None` disables it entirely.
    pub fn from_env() -> Option<Self> {
        let password_hash = std::env::var("WEB_DESKTOP_PASSWORD_HASH")
            .ok()
            .filter(|hash| !hash.trim().is_empty())?;

        // A hash that cannot be parsed would make every login fail with a 500, which
        // looks like a wrong password. Say so once, at startup, instead.
        if let Err(err) = bcrypt::verify("", &password_hash) {
            tracing::error!(
                "auth: WEB_DESKTOP_PASSWORD_HASH is not a bcrypt hash ({err}); logins will fail. \
                 Generate one with `cargo xtask hash-password`."
            );
        }

        let session_days = std::env::var("WEB_DESKTOP_SESSION_DAYS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|days| *days > 0)
            .unwrap_or(DEFAULT_SESSION_DAYS);

        let key = match std::env::var("WEB_DESKTOP_SECRET") {
            Ok(secret) if !secret.trim().is_empty() => {
                // Derive rather than use the bytes directly: `Key` wants 32+ bytes of
                // key material, and an operator's passphrase is rarely that long or
                // that random. Hashing it first means any secret is acceptable, and
                // `derive_from` then splits it into signing and encryption keys.
                use sha2::{Digest, Sha256};
                let material = Sha256::digest(secret.as_bytes());
                Key::derive_from(&material)
            }
            _ => {
                warn!(
                    "auth: WEB_DESKTOP_SECRET is not set; using a random cookie key, so every \
                     session ends when the server restarts. Set a long random secret to keep \
                     logins across restarts."
                );
                Key::generate()
            }
        };

        Some(Self {
            password_hash,
            session_seconds: (session_days * 24 * 60 * 60) as i64,
            key,
        })
    }

    /// Whether this string is the password, in constant-ish time (bcrypt handles that).
    fn verify(&self, password: &str) -> Result<bool, bcrypt::BcryptError> {
        bcrypt::verify(password, &self.password_hash)
    }

    /// Sign a session cookie and put it in the response. The expiry lives *inside* the
    /// signed value as well as in `Max-Age`, so a cookie whose attributes a client
    /// stripped still expires on the server's terms.
    fn sign_in(&self, cookies: &Cookies) {
        let expires = now_secs().saturating_add_signed(self.session_seconds);
        let cookie = Cookie::build((SESSION_COOKIE, format!("v1:{expires}")))
            .http_only(true)
            .same_site(SameSite::Strict)
            .path("/")
            .max_age(cookie::time::Duration::seconds(self.session_seconds))
            .build();
        cookies.signed(&self.key).add(cookie);
    }

    /// Forget the session: a `Set-Cookie` that expires the cookie now.
    fn sign_out(&self, cookies: &Cookies) {
        cookies
            .signed(&self.key)
            .remove(Cookie::build(SESSION_COOKIE).path("/").build());
    }

    /// Whether the request carries a signed session cookie that has not expired.
    ///
    /// The signature check is the whole authentication: `get` returns `None` for a
    /// cookie this server did not sign, so a forged value never reaches the parse.
    fn signed_in(&self, cookies: &Cookies) -> bool {
        let Some(value) = cookies.signed(&self.key).get(SESSION_COOKIE) else {
            return false;
        };
        let Some(expires) = value
            .value()
            .strip_prefix("v1:")
            .and_then(|rest| rest.parse::<u64>().ok())
        else {
            return false;
        };
        expires > now_secs()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// bcrypt of "correct horse" at cost 4, so tests stay quick. A fixture rather than a
/// hash computed at test time: what is being tested is that a hash stored by some
/// earlier run still accepts its own password.
#[cfg(test)]
const HASH: &str = "$2b$04$1gKxhVtjiVoc2J63v9cRA.WVoE6dDokTQaIvkPMQXHAZm51HL4e0e";

/// The cookie middleware, installed on the whole app so the extractor works anywhere.
pub fn cookie_layer() -> CookieManagerLayer {
    CookieManagerLayer::new()
}

/// Guard for the API: everything reaches a handler only through here.
///
/// A missing or bad session is a plain `401`, with no body and no hint: a caller that
/// is not signed in learns nothing except that it is not signed in.
pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    cookies: Cookies,
    request: Request,
    next: Next,
) -> Response {
    match state.auth.as_ref() {
        Some(auth) if !auth.signed_in(&cookies) => StatusCode::UNAUTHORIZED.into_response(),
        _ => next.run(request).await,
    }
}

/// `GET /api/session` - what the shell asks before deciding to show a login screen.
pub async fn session(
    State(state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Json<SessionResponse> {
    match state.auth.as_ref() {
        // `authenticated` is reported honestly even when auth is off: the field means
        // "this request is signed in", and the shell only cares once `enabled` is true.
        None => Json(SessionResponse {
            authenticated: false,
            enabled: false,
        }),
        Some(auth) => Json(SessionResponse {
            authenticated: auth.signed_in(&cookies),
            enabled: true,
        }),
    }
}

/// `POST /api/login` - check the password and start a session.
///
/// A wrong password is `200 {"ok": false}`, not an error status: it is a normal
/// answer to a normal request, and the client has to handle it either way.
pub async fn login(
    State(state): State<Arc<AppState>>,
    cookies: Cookies,
    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, StatusCode> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(StatusCode::NOT_FOUND);
    };

    let ok = auth.verify(&body.password).map_err(|err| {
        // Only reachable with an unparseable hash, which startup already warned
        // about. Log each occurrence so the cause is not a mystery.
        tracing::error!("auth: could not verify the password against the stored hash: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if ok {
        auth.sign_in(&cookies);
    }
    Ok(Json(LoginResponse { ok }))
}

/// `POST /api/logout` - end the session, signed in or not.
pub async fn logout(State(state): State<Arc<AppState>>, cookies: Cookies) -> StatusCode {
    if let Some(auth) = state.auth.as_ref() {
        auth.sign_out(&cookies);
    }
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> Auth {
        Auth {
            password_hash: HASH.to_owned(),
            session_seconds: 3600,
            key: Key::derive_from(&[7u8; 32]),
        }
    }

    fn cookies() -> Cookies {
        Cookies::default()
    }

    #[test]
    fn the_right_password_is_accepted_and_a_wrong_one_is_not() {
        let auth = auth();
        // The hash above is generated from this passphrase; if the fixture ever drifts
        // the first assertion catches it rather than the tests passing vacuously.
        assert!(auth.verify("correct horse").unwrap());
        assert!(!auth.verify("Correct horse").unwrap());
        assert!(!auth.verify("").unwrap());
    }

    #[test]
    fn signing_in_makes_the_cookie_verify() {
        let auth = auth();
        let cookies = cookies();
        assert!(!auth.signed_in(&cookies), "no cookie yet");
        auth.sign_in(&cookies);
        assert!(auth.signed_in(&cookies));
    }

    #[test]
    fn signing_out_ends_the_session() {
        let auth = auth();
        let cookies = cookies();
        auth.sign_in(&cookies);
        auth.sign_out(&cookies);
        assert!(!auth.signed_in(&cookies));
    }

    #[test]
    fn a_cookie_signed_with_another_key_is_not_a_session() {
        let auth = auth();
        let cookies = cookies();
        let other = Auth {
            key: Key::derive_from(&[9u8; 32]),
            ..auth_with_same_hash()
        };
        other.sign_in(&cookies);
        assert!(
            !auth.signed_in(&cookies),
            "a session from a differently-keyed server must not be accepted"
        );
    }

    #[test]
    fn a_session_that_has_expired_is_not_a_session() {
        let auth = auth();
        let cookies = cookies();
        // Signed by this server, so the signature check passes and the expiry is the
        // only thing that can reject it: an absolute time one second after the epoch.
        cookies
            .signed(&auth.key)
            .add(Cookie::new(SESSION_COOKIE, "v1:1"));
        assert!(!auth.signed_in(&cookies));
    }

    #[test]
    fn an_unsigned_cookie_of_the_right_shape_is_refused() {
        let auth = auth();
        let cookies = cookies();
        let far_future = now_secs() + 10_000;
        cookies.add(Cookie::new(SESSION_COOKIE, format!("v1:{far_future}")));
        assert!(
            !auth.signed_in(&cookies),
            "a hand-written value must not authenticate without a signature"
        );
    }

    #[test]
    fn a_signed_cookie_with_a_silly_value_is_refused() {
        let auth = auth();
        let cookies = cookies();
        for value in [
            "",
            "v1:",
            "v1:soon",
            "nonsense",
            "v1:99999999999999999999999",
        ] {
            cookies
                .signed(&auth.key)
                .add(Cookie::new(SESSION_COOKIE, value));
            assert!(
                !auth.signed_in(&cookies),
                "value {value:?} must not sign in"
            );
        }
    }

    /// A second `Auth` with the same hash, for `..` updates that vary one field.
    fn auth_with_same_hash() -> Auth {
        Auth {
            password_hash: HASH.to_owned(),
            session_seconds: 3600,
            key: Key::derive_from(&[7u8; 32]),
        }
    }
}

/// The middleware, over real HTTP, wired the way `main` wires it.
///
/// The unit tests above check the cookie logic; these check that the *router* uses it:
/// that the guard is on the routes that should have it, that the login endpoints are
/// reachable without it, and that the cookie the login sets is what the guard wants.
#[cfg(test)]
mod http_tests {
    use super::*;
    use axum::{
        Router, middleware,
        routing::{get, post},
    };

    /// A miniature of the server's router: one protected route plus the public ones.
    fn app(auth: Option<Auth>) -> axum::Router {
        let state = Arc::new(AppState {
            started: std::time::Instant::now(),
            catalog: shared::default_catalog(),
            apps_dir: None,
            proxy: None,
            ai: None,
            images: None,
            fs: None,
            auth,
        });

        let protected = Router::new()
            .route("/api/secret", get(|| async { "signed in" }))
            .route_layer(middleware::from_fn_with_state(state.clone(), require_auth))
            .with_state(state.clone());

        Router::new()
            .route(shared::auth::API_SESSION, get(session))
            .route(shared::auth::API_LOGIN, post(login))
            .route(shared::auth::API_LOGOUT, post(logout))
            .with_state(state)
            .merge(protected)
            .layer(cookie_layer())
    }

    /// Serve one router on a free port and hand back its base URL.
    async fn serve(auth: Option<Auth>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = app(auth);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    fn test_auth() -> Auth {
        Auth {
            password_hash: HASH.to_owned(),
            session_seconds: 3600,
            key: Key::derive_from(&[7u8; 32]),
        }
    }

    /// The `Cookie` header the browser would send back, taken from a `Set-Cookie`.
    fn cookie_header(response: &reqwest::Response) -> Option<String> {
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(|value| value.split(';').next())
            .filter(|pair| pair.starts_with(SESSION_COOKIE))
            .map(str::to_owned)
    }

    async fn post_login(
        base: &str,
        password: &str,
    ) -> (reqwest::StatusCode, Option<String>, String) {
        let response = reqwest::Client::new()
            .post(format!("{base}{}", shared::auth::API_LOGIN))
            .json(&serde_json::json!({ "password": password }))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let cookie = cookie_header(&response);
        (status, cookie, response.text().await.unwrap())
    }

    #[tokio::test]
    async fn with_no_password_everything_is_open() {
        let base = serve(None).await;
        let client = reqwest::Client::new();

        assert_eq!(
            client
                .get(format!("{base}/api/secret"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let session: serde_json::Value = client
            .get(format!("{base}{}", shared::auth::API_SESSION))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(session["enabled"], false);
        assert_eq!(session["authenticated"], false);

        // Logging in is not a thing when there is no password to log in with.
        assert_eq!(post_login(&base, "anything").await.0, 404);
    }

    #[tokio::test]
    async fn with_a_password_the_guard_is_on_every_route_it_is_wired_to() {
        let base = serve(Some(test_auth())).await;
        let client = reqwest::Client::new();

        assert_eq!(
            client
                .get(format!("{base}/api/secret"))
                .send()
                .await
                .unwrap()
                .status(),
            401,
            "a route behind the guard must refuse an anonymous request"
        );
        // ... while the login screen's own endpoints stay reachable, or nobody
        // could ever sign in.
        assert_eq!(
            client
                .get(format!("{base}{}", shared::auth::API_SESSION))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }

    #[tokio::test]
    async fn the_cookie_from_a_good_login_is_what_the_guard_wants() {
        let base = serve(Some(test_auth())).await;

        let (status, cookie, body) = post_login(&base, "correct horse").await;
        assert_eq!(status, 200);
        assert!(body.contains("true"), "{body}");
        let cookie = cookie.expect("a successful login sets a session cookie");
        assert!(cookie.starts_with(SESSION_COOKIE), "{cookie}");

        let response = reqwest::Client::new()
            .get(format!("{base}/api/secret"))
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), "signed in");
    }

    #[tokio::test]
    async fn a_wrong_password_sets_no_cookie() {
        let base = serve(Some(test_auth())).await;

        let (status, cookie, body) = post_login(&base, "nope").await;
        assert_eq!(status, 200, "a wrong password is an answer, not an error");
        assert!(body.contains("false"), "{body}");
        assert!(cookie.is_none(), "{cookie:?}");
    }

    #[tokio::test]
    async fn a_cookie_this_server_did_not_sign_is_refused() {
        let base = serve(Some(test_auth())).await;
        let far_future = now_secs() + 10_000;

        let status = reqwest::Client::new()
            .get(format!("{base}/api/secret"))
            .header(
                reqwest::header::COOKIE,
                format!("{SESSION_COOKIE}=v1:{far_future}"),
            )
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 401, "the expiry alone must not be enough");
    }

    #[tokio::test]
    async fn signing_out_expires_the_cookie_it_was_given() {
        let base = serve(Some(test_auth())).await;
        let (_, Some(cookie), _) = post_login(&base, "correct horse").await else {
            panic!("login should have set a cookie");
        };

        let response = reqwest::Client::new()
            .post(format!("{base}{}", shared::auth::API_LOGOUT))
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 204);

        // The answer must tell the browser to drop it: same name, already expired.
        let cleared = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with(SESSION_COOKIE))
            .unwrap_or_else(|| panic!("a sign-out must clear the cookie"));
        assert!(
            cleared.contains("Max-Age=0") || cleared.contains("Expires="),
            "{cleared}"
        );
    }
}
