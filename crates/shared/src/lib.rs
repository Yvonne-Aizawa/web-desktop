//! Types shared verbatim by the `server` and `client` crates.
//!
//! Everything here derives `serde`, so the wire format between the axum backend
//! and the WASM frontend is defined once and checked at compile time on both
//! ends. Nothing in this crate depends on `tokio` or `web-sys`, so it builds for
//! the host and for `wasm32-unknown-unknown` alike.

use serde::{Deserialize, Serialize};

pub mod ai;
pub mod fs;

/// Interface version between the shell and a served app.
///
/// Bump this when the host object or the mount contract changes incompatibly:
/// `xtask build-app` stamps it into every manifest, and the shell refuses to load
/// an app it does not implement.
pub const APP_ABI: &str = "1";

/// Endpoint returning [`Catalog`].
pub const API_CATALOG: &str = "/api/catalog";
/// Endpoint returning [`Health`].
pub const API_HEALTH: &str = "/api/health";
/// `GET` with `?url=<absolute url>`: fetch a third-party URL on the app's behalf.
///
/// The point is CORS: a served app runs in the browser, so a response from a host
/// without CORS headers is unreadable no matter who asks. Asking the server to make
/// the request sidesteps that, and puts the decision about *which* hosts are reachable
/// where it cannot be bypassed.
pub const API_PROXY: &str = "/api/proxy";

/// Every application the desktop can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppId {
    Terminal,
    Files,
    Editor,
    Gallery,
    Settings,
    About,
}

impl AppId {
    /// Catalog order, also used by the client when the backend is unreachable.
    pub const ALL: [AppId; 6] = [
        Self::Terminal,
        Self::Files,
        Self::Editor,
        Self::Gallery,
        Self::Settings,
        Self::About,
    ];

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Files => "files",
            Self::Editor => "editor",
            Self::Gallery => "gallery",
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
            Self::Editor => "Text Editor",
            Self::Gallery => "Gallery",
            Self::Settings => "Settings",
            Self::About => "About",
        }
    }

    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Terminal => ">_",
            Self::Files => "🗂",
            Self::Editor => "✎",
            Self::Gallery => "🖼",
            Self::Settings => "⚙",
            Self::About => "◍",
        }
    }

    pub const fn accent(self) -> &'static str {
        match self {
            Self::Terminal => "#4ade80",
            Self::Files => "#60a5fa",
            Self::Editor => "#34d399",
            Self::Gallery => "#f472b6",
            Self::Settings => "#c084fc",
            Self::About => "#fbbf24",
        }
    }

    pub const fn blurb(self) -> &'static str {
        match self {
            Self::Terminal => "Shell with a handful of built-ins",
            Self::Files => "Browse the virtual filesystem",
            Self::Editor => "Edit several files at once",
            Self::Gallery => "Look at the photos in your files",
            Self::Settings => "Wallpaper, connectivity, layout",
            Self::About => "Build info and credits",
        }
    }

    /// Initial window size in CSS pixels.
    pub const fn default_size(self) -> (f64, f64) {
        match self {
            Self::Terminal => (680.0, 420.0),
            Self::Files => (760.0, 480.0),
            Self::Editor => (940.0, 620.0),
            Self::Gallery => (900.0, 640.0),
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
    /// Where the program comes from.
    #[serde(default)]
    pub source: AppSource,
}

/// A program compiled into the shell versus one the server hands over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppSource {
    /// Compiled in; `id` resolves through [`AppId::from_slug`].
    #[default]
    Builtin,
    /// Served as its own module, loaded at runtime.
    Served(ServedApp),
}

/// Everything the shell needs to fetch and run a served app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedApp {
    /// Directory under `/apps/<id>/<version>/`, immutable once built.
    pub version: String,
    /// ES module implementing the mount contract (`init`, `mount`, `unmount`).
    pub entry: String,
    /// The wasm the entry module instantiates.
    pub wasm: String,
    /// Lowercase hex SHA-256 of `wasm`, checked before it is instantiated.
    pub sha256: String,
    /// Bytes of `wasm`, so the shell can refuse an absurd download up front.
    pub size: u64,
    /// The [`APP_ABI`] this app was built against.
    pub abi: String,
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
            source: AppSource::Builtin,
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
            source: AppSource::Builtin,
        }
    }

    /// The built-in this entry refers to, or `None` when the shell has no program
    /// registered for this slug.
    pub fn builtin_id(&self) -> Option<AppId> {
        AppId::from_slug(&self.id)
    }

    /// The served bundle, when this app is not compiled in.
    pub fn served(&self) -> Option<&ServedApp> {
        match &self.source {
            AppSource::Served(served) => Some(served),
            AppSource::Builtin => None,
        }
    }

    /// Whether this shell can run the app at all: either it is compiled in, or it
    /// is served and the frontend has a loader (checked against [`APP_ABI`] there).
    pub fn is_runnable(&self) -> bool {
        self.builtin_id().is_some() || self.served().is_some()
    }

    /// Where an app's bundle lives, so both sides agree on the layout.
    pub fn served_base_url(&self) -> Option<String> {
        self.served()
            .map(|s| format!("/apps/{}/{}/", self.id, s.version))
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
    /// What the outbound proxy will do, so an app (and Settings) can tell whether
    /// fetching a third-party URL is even possible here.
    #[serde(default)]
    pub proxy: ProxyInfo,
    /// What the chat gateway offers: which models, and which one is the default.
    /// Empty `models` means it is off.
    #[serde(default)]
    pub ai: ai::AiInfo,
}

/// What the outbound proxy allows. Empty `hosts` means it is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProxyInfo {
    pub hosts: Vec<String>,
    /// Whether private and loopback addresses are reachable (off unless asked for).
    pub allow_private: bool,
    pub max_bytes: u64,
    pub timeout_secs: u64,
}

impl ProxyInfo {
    pub fn enabled(&self) -> bool {
        !self.hosts.is_empty()
    }
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
        proxy: ProxyInfo::default(),
        ai: ai::AiInfo::default(),
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
            "fs": {"enabled":false,"root":"","writable":false,"max_preview_bytes":0},
            "proxy": {"hosts": ["example.org"], "allow_private": false,
                      "max_bytes": 1024, "timeout_secs": 5}
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
        assert!(catalog.proxy.enabled());
        assert_eq!(catalog.proxy.hosts, ["example.org"]);
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
    fn a_served_app_decodes_and_is_runnable() {
        let json = r##"{
            "id": "calculator", "name": "Calculator", "glyph": "\u2327", "accent": "#fb923c",
            "blurb": "does arithmetic", "default_width": 320, "default_height": 320,
            "source": {"served": {"version": "0.1.0", "entry": "app.js", "wasm": "app_bg.wasm",
                                  "sha256": "abc123", "size": 4242, "abi": "1"}}
        }"##;
        let manifest: AppManifest =
            serde_json::from_str(json).expect("served manifest must decode");
        assert!(!manifest.is_runnable() || manifest.builtin_id().is_none());
        assert!(manifest.served().is_some());
        assert_eq!(
            manifest.served_base_url().as_deref(),
            Some("/apps/calculator/0.1.0/")
        );
        assert_eq!(manifest.served().unwrap().size, 4242);
    }

    #[test]
    fn a_builtin_manifest_carries_no_bundle() {
        let manifest = AppManifest::builtin(AppId::Terminal);
        assert_eq!(manifest.source, AppSource::Builtin);
        assert!(manifest.served().is_none());
        assert!(manifest.served_base_url().is_none());
        assert!(manifest.is_runnable());
    }

    #[test]
    fn a_manifest_without_a_source_field_is_a_builtin() {
        // Forward compatibility with a catalog that predates served apps.
        let json = r##"{"id":"files","name":"Files","glyph":"g","accent":"#fff",
                        "blurb":"b","default_width":10,"default_height":10}"##;
        let manifest: AppManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.source, AppSource::Builtin);
    }

    #[test]
    fn a_catalog_without_a_proxy_block_is_off() {
        let json = r##"{"hostname":"h","apps":[],"wallpapers":[],
                        "fs":{"enabled":false,"root":"","writable":false,"max_preview_bytes":0}}"##;
        let catalog: Catalog = serde_json::from_str(json).unwrap();
        assert!(!catalog.proxy.enabled());
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
