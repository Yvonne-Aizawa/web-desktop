//! Types shared verbatim by the `server` and `client` crates.
//!
//! Everything here derives `serde`, so the wire format between the axum backend
//! and the WASM frontend is defined once and checked at compile time on both
//! ends. Nothing in this crate depends on `tokio` or `web-sys`, so it builds for
//! the host and for `wasm32-unknown-unknown` alike.

use serde::{Deserialize, Serialize};

/// Endpoint returning [`Catalog`].
pub const API_CATALOG: &str = "/api/catalog";
/// Endpoint returning [`Health`].
pub const API_HEALTH: &str = "/api/health";

/// Every application the desktop can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppId {
    Terminal,
    Files,
    Settings,
    About,
}

impl AppId {
    /// Catalog order, also used by the client when the backend is unreachable.
    pub const ALL: [AppId; 4] = [Self::Terminal, Self::Files, Self::Settings, Self::About];

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Files => "files",
            Self::Settings => "settings",
            Self::About => "about",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.slug() == slug)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Files => "Files",
            Self::Settings => "Settings",
            Self::About => "About",
        }
    }

    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Terminal => ">_",
            Self::Files => "🗂",
            Self::Settings => "⚙",
            Self::About => "◍",
        }
    }

    pub const fn accent(self) -> &'static str {
        match self {
            Self::Terminal => "#4ade80",
            Self::Files => "#60a5fa",
            Self::Settings => "#c084fc",
            Self::About => "#fbbf24",
        }
    }

    pub const fn blurb(self) -> &'static str {
        match self {
            Self::Terminal => "Shell with a handful of built-ins",
            Self::Files => "Browse the virtual filesystem",
            Self::Settings => "Wallpaper, connectivity, layout",
            Self::About => "Build info and credits",
        }
    }

    /// Initial window size in CSS pixels.
    pub const fn default_size(self) -> (f64, f64) {
        match self {
            Self::Terminal => (680.0, 420.0),
            Self::Files => (760.0, 480.0),
            Self::Settings => (620.0, 460.0),
            Self::About => (460.0, 400.0),
        }
    }
}

/// How the backend advertises an application to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppManifest {
    pub id: AppId,
    pub name: String,
    pub glyph: String,
    pub accent: String,
    pub blurb: String,
    pub default_width: u32,
    pub default_height: u32,
}

impl From<AppId> for AppManifest {
    fn from(id: AppId) -> Self {
        let (w, h) = id.default_size();
        Self {
            id,
            name: id.name().to_owned(),
            glyph: id.glyph().to_owned(),
            accent: id.accent().to_owned(),
            blurb: id.blurb().to_owned(),
            default_width: w as u32,
            default_height: h as u32,
        }
    }
}

/// A background the desktop can paint. `css` is a full CSS `background` shorthand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wallpaper {
    pub id: String,
    pub name: String,
    pub css: String,
}

/// Everything the frontend needs to bootstrap, in one round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub hostname: String,
    pub apps: Vec<AppManifest>,
    pub wallpapers: Vec<Wallpaper>,
}

/// Liveness payload for the About/Settings panels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub name: String,
    pub version: String,
    pub uptime_secs: u64,
}

/// The built-in catalog. The server serves this; the client uses it as a
/// fallback so the desktop still boots when `trunk serve` runs alone.
pub fn default_catalog() -> Catalog {
    Catalog {
        hostname: "web-desktop".to_owned(),
        apps: AppId::ALL.into_iter().map(AppManifest::from).collect(),
        wallpapers: default_wallpapers(),
    }
}

pub fn default_wallpapers() -> Vec<Wallpaper> {
    [
        (
            "dusk",
            "Dusk",
            "radial-gradient(circle at 20% 15%, #2b3a67 0%, #131a2b 45%, #07080d 100%)",
        ),
        (
            "ember",
            "Ember",
            "radial-gradient(circle at 80% 85%, #6d2233 0%, #241020 50%, #0a0609 100%)",
        ),
        (
            "moss",
            "Moss",
            "linear-gradient(160deg, #0f2b23 0%, #10321f 40%, #06120e 100%)",
        ),
        (
            "nebula",
            "Nebula",
            "radial-gradient(circle at 70% 20%, #4c2a72 0%, #1a1030 50%, #05040b 100%)",
        ),
        (
            "slate",
            "Slate",
            "linear-gradient(200deg, #22262e 0%, #14171c 55%, #0a0b0e 100%)",
        ),
        (
            "solar",
            "Solar",
            "radial-gradient(circle at 30% 80%, #7a5218 0%, #2b1d0c 50%, #0b0805 100%)",
        ),
    ]
    .into_iter()
    .map(|(id, name, css)| Wallpaper {
        id: id.to_owned(),
        name: name.to_owned(),
        css: css.to_owned(),
    })
    .collect()
}
