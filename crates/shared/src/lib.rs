//! Types shared verbatim by the `server` and `client` crates.
//!
//! Everything here derives `serde`, so the wire format between the axum backend
//! and the WASM frontend is defined once and checked at compile time on both
//! ends. Nothing in this crate depends on `tokio` or `web-sys`, so it builds for
//! the host and for `wasm32-unknown-unknown` alike.

use serde::{Deserialize, Serialize};

pub mod fs;

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
///
/// `id` is a plain slug rather than an [`AppId`] on purpose. The shell has to be
/// able to *hold and display* an app it cannot run: if the server advertises an app
/// from a newer build, decoding the id as a closed enum would fail and take the
/// entire catalog down with it. Instead the entry survives, [`Self::builtin_id`]
/// returns `None`, and the window says the app is not installed in this build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppManifest {
    /// Stable slug, e.g. `terminal`. Matches [`AppId::slug`] for built-ins.
    pub id: String,
    pub name: String,
    pub glyph: String,
    pub accent: String,
    pub blurb: String,
    pub default_width: u32,
    pub default_height: u32,
}

impl AppManifest {
    /// The catalog entry for a built-in app.
    pub fn builtin(id: AppId) -> Self {
        let (width, height) = id.default_size();
        Self {
            id: id.slug().to_owned(),
            name: id.name().to_owned(),
            glyph: id.glyph().to_owned(),
            accent: id.accent().to_owned(),
            blurb: id.blurb().to_owned(),
            default_width: width as u32,
            default_height: height as u32,
        }
    }

    /// Stands in for a window whose app handle is not in the shell's registry.
    /// Unreachable through the public API, but a UI must not panic on it, and the
    /// resulting "not installed" pane is the honest thing to show anyway.
    pub fn placeholder() -> Self {
        Self {
            id: String::new(),
            name: "Unknown app".to_owned(),
            glyph: "?".to_owned(),
            accent: "#97a1b4".to_owned(),
            blurb: "No catalog entry".to_owned(),
            default_width: 420,
            default_height: 260,
        }
    }

    /// The built-in this entry refers to, or `None` when the shell has no program
    /// registered for this slug.
    pub fn builtin_id(&self) -> Option<AppId> {
        AppId::from_slug(&self.id)
    }

    /// Whether this build can actually run the app.
    pub fn is_runnable(&self) -> bool {
        self.builtin_id().is_some()
    }
}

impl From<AppId> for AppManifest {
    fn from(id: AppId) -> Self {
        Self::builtin(id)
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
    pub fs: fs::FsInfo,
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
        fs: fs::FsInfo::disabled(),
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

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn an_unknown_app_does_not_break_the_catalog() {
        // Exactly the skew case: a newer server advertising an app this build has
        // never heard of. Decoding must succeed, with the entry kept intact.
        // `r##` because the JSON contains `"#` in the accent colours.
        let json = r##"{
            "hostname": "web-desktop",
            "apps": [
                {"id":"terminal","name":"Terminal","glyph":">_","accent":"#4ade80",
                 "blurb":"shell","default_width":680,"default_height":420},
                {"id":"music","name":"Music","glyph":"\u266b","accent":"#f472b6",
                 "blurb":"plays things","default_width":520,"default_height":360}
            ],
            "wallpapers": [],
            "fs": {"enabled":false,"root":"","writable":false,"max_preview_bytes":0}
        }"##;

        let catalog: Catalog =
            serde_json::from_str(json).expect("a catalog with an unknown app must still decode");
        assert_eq!(catalog.apps.len(), 2);
        assert_eq!(catalog.apps[0].builtin_id(), Some(AppId::Terminal));
        assert!(catalog.apps[0].is_runnable());
        assert_eq!(catalog.apps[1].id, "music");
        assert_eq!(catalog.apps[1].builtin_id(), None);
        assert!(!catalog.apps[1].is_runnable());
        // Still displayable, which is the whole point of keeping it.
        assert_eq!(catalog.apps[1].name, "Music");
        assert_eq!(catalog.apps[1].accent, "#f472b6");
    }

    #[test]
    fn a_manifest_id_is_wire_compatible_with_the_enum() {
        // `AppId` serializes to a snake_case string and so does the slug, so
        // turning the field into a `String` did not change the wire format.
        let from_enum = serde_json::to_string(&AppId::Terminal).unwrap();
        let from_manifest =
            serde_json::to_string(&AppManifest::builtin(AppId::Terminal).id).unwrap();
        assert_eq!(from_enum, from_manifest);
        assert_eq!(from_enum, r#""terminal""#);
    }

    #[test]
    fn placeholder_is_not_runnable() {
        let placeholder = AppManifest::placeholder();
        assert!(!placeholder.is_runnable());
        assert_eq!(placeholder.builtin_id(), None);
        assert_eq!(placeholder.name, "Unknown app");
    }

    #[test]
    fn the_default_catalog_is_all_runnable() {
        for app in default_catalog().apps {
            assert!(app.is_runnable(), "{} should map to a built-in", app.id);
        }
    }
}
