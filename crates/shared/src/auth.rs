//! Authentication types shared between the axum backend and the leptos shell.
//!
//! The server is the only place the password hash lives. The browser gets a signed
//! HTTP-only cookie after a successful login, and every API call after that is checked
//! for that cookie. If no password is configured, the server reports that auth is
//! disabled and the shell never shows a login screen.

use serde::{Deserialize, Serialize};

/// Endpoint for checking the current session.
pub const API_SESSION: &str = "/api/session";
/// Endpoint for logging in.
pub const API_LOGIN: &str = "/api/login";
/// Endpoint for logging out.
pub const API_LOGOUT: &str = "/api/logout";

/// Response from [`API_SESSION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResponse {
    /// Whether the caller is currently authenticated.
    pub authenticated: bool,
    /// Whether the server even requires authentication. When this is `false`, the
    /// shell can skip the login screen regardless of `authenticated`.
    pub enabled: bool,
}

/// Login request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

/// Login response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    pub ok: bool,
}
