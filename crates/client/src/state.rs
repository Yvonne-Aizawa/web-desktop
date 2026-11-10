//! Desktop state: the window list plus everything the shell reads.
//!
//! # Why `Vec<WinData>` and not `Vec<WinHandle>`
//!
//! Leptos keyed `For` reuses a row's DOM when its key is unchanged, so the view
//! for a row cannot re-render just because the item's data changed: the row has
//! to read the changing data through a signal. The obvious shape is to put a
//! signal inside each item, but the signals must then be created *inside* an
//! event handler (clicking the start menu opens a window), and a plain
//! `RwSignal` is owned by the reactive owner that was current at creation time.
//! That owner is the menu item's, so closing the menu disposes the window's
//! geometry signal and the next read panics with "Tried to access a reactive
//! value that has already been disposed" (`ArcRwSignal` would fix the disposal
//! but is not `Copy`, which the row views need).
//!
//! So all window state lives in one root-owned `RwSignal<Vec<WinData>>`, and each
//! row derives a [`Memo`] for *its* window. `Memo` is `Copy`, `PartialEq`-cached,
//! and owned by the row, so a drag repaints exactly one window.

use leptos::prelude::*;
use shared::{
    AppId, AppManifest, Health, ProxyInfo, Wallpaper, ai::AiInfo, default_catalog, fs::FsInfo,
};

/// Height of the taskbar in CSS pixels; the window area excludes it.
pub const TASKBAR_HEIGHT: f64 = 52.0;

/// Cascade step for newly opened windows.
const CASCADE: f64 = 34.0;

/// A file to open, and which app should open it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    pub app: AppId,
    pub path: String,
}

/// Handle to an app in the shell's registry.
///
/// A small index rather than the manifest itself, because [`WinData`] has to stay
/// `Copy`: all windows live in one signal, and that is what makes a drag repaint a
/// single window. Served apps (stage 1) carry owned metadata such as entry URLs and
/// hashes, so the window record can only hold a handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AppRef(u32);

/// Geometry of one window. `Copy` on purpose: every field is a scalar, which is
/// what lets the whole list live in a single signal cheaply.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WinData {
    pub id: u32,
    pub app: AppRef,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Stacking order; higher paints on top.
    pub z: u32,
    pub minimized: bool,
    pub maximized: bool,
    /// `[x, y, w, h]` captured when maximizing.
    pub restore: [f64; 4],
}

impl WinData {
    /// Keep a strip of the title bar on screen so a window can always be grabbed.
    pub fn clamped(mut self) -> Self {
        const KEEP_X: f64 = 112.0;
        const KEEP_Y: f64 = 26.0;
        let (vw, vh) = viewport();
        self.x = self
            .x
            .clamp(KEEP_X - self.w, (vw - KEEP_X).max(KEEP_X - self.w));
        self.y = self.y.clamp(0.0, (vh - KEEP_Y).max(0.0));
        self
    }

    /// Inline style for the window frame. `--accent` feeds the title bar color and
    /// comes from the registry rather than the record, so this stays `Copy`.
    pub fn css(&self, accent: &str) -> String {
        if self.maximized {
            format!(
                "left:0;top:0;width:100%;height:100%;z-index:{};--accent:{accent};",
                10 + self.z
            )
        } else {
            format!(
                "left:{}px;top:{}px;width:{}px;height:{}px;z-index:{};--accent:{accent};",
                self.x,
                self.y,
                self.w,
                self.h,
                10 + self.z
            )
        }
    }
}

/// Global shell state, handed around with `provide_context`.
#[derive(Clone, Copy)]
pub struct Desktop {
    pub windows: RwSignal<Vec<WinData>>,
    /// What the server advertises *right now*. Drives the start menu and About.
    pub catalog: RwSignal<Vec<AppManifest>>,
    /// Append-only registry behind [`AppRef`], with catalogue entries written in by
    /// [`Desktop::intern`]. Indices never move, so a catalog refresh can add or
    /// relabel apps without a window suddenly pointing at a different one.
    registry: RwSignal<Vec<AppManifest>>,
    pub wallpapers: RwSignal<Vec<Wallpaper>>,
    pub wallpaper: RwSignal<String>,
    /// What the backend is willing to expose to the Files app.
    pub fs: RwSignal<FsInfo>,
    /// What the backend's outbound proxy allows, for Settings and for apps that want
    /// to check before asking.
    pub proxy: RwSignal<ProxyInfo>,
    /// What the backend's chat gateway offers: which models, and which is the default.
    /// Empty `models` means no provider is configured there.
    pub ai: RwSignal<AiInfo>,
    pub start_open: RwSignal<bool>,
    /// A file handed to an app, as `(which app, which path)`.
    ///
    /// A one-shot handoff rather than shared state: the app that matches consumes it,
    /// and a second request replaces an unconsumed one. It is how one app asks another
    /// to open something without knowing anything about its internals.
    pub pending_open: RwSignal<Option<OpenRequest>>,
    /// `None` while the first request is in flight, `Some(Err)` if it failed.
    pub health: RwSignal<Option<Result<Health, String>>>,
    /// Kept separate from [`Self::health`] on purpose: the two requests can fail
    /// independently, and a successful health check used to overwrite a failed
    /// catalog silently, leaving the menu short of apps with nothing to explain it.
    pub catalog_error: RwSignal<Option<String>>,
    next_id: RwSignal<u32>,
    next_z: RwSignal<u32>,
}

impl Default for Desktop {
    fn default() -> Self {
        Self::new()
    }
}

impl Desktop {
    pub fn new() -> Self {
        let fallback = default_catalog();
        Self {
            windows: RwSignal::new(Vec::new()),
            catalog: RwSignal::new(fallback.apps),
            registry: RwSignal::new(Vec::new()),
            wallpapers: RwSignal::new(fallback.wallpapers),
            wallpaper: RwSignal::new(initial_wallpaper()),
            fs: RwSignal::new(FsInfo::disabled()),
            proxy: RwSignal::new(ProxyInfo::default()),
            ai: RwSignal::new(fallback.ai),
            start_open: RwSignal::new(false),
            pending_open: RwSignal::new(None),
            health: RwSignal::new(None),
            catalog_error: RwSignal::new(None),
            next_id: RwSignal::new(1),
            next_z: RwSignal::new(1),
        }
    }

    /// Live view of one window for a keyed row. `stale` is used when the window
    /// has just been closed and the row has not been removed yet.
    pub fn live(self, id: u32, stale: WinData) -> Memo<WinData> {
        Memo::new(move |_| self.window(id).unwrap_or(stale))
    }

    pub fn window(self, id: u32) -> Option<WinData> {
        self.windows
            .with(|ws| ws.iter().copied().find(|w| w.id == id))
    }

    /// Mutate one window in place, if it still exists.
    pub fn edit(self, id: u32, f: impl FnOnce(&mut WinData)) {
        self.windows.update(|ws| {
            if let Some(w) = ws.iter_mut().find(|w| w.id == id) {
                f(w);
            }
        });
    }

    fn bump_z(self) -> u32 {
        self.next_z.update(|z| *z += 1);
        self.next_z.get_untracked()
    }

    /// Id of the window painted on top, ignoring minimized ones.
    pub fn focused(self) -> Option<u32> {
        self.windows.with(|ws| {
            ws.iter()
                .filter(|w| !w.minimized)
                .max_by_key(|w| w.z)
                .map(|w| w.id)
        })
    }

    pub fn is_focused(self, id: u32) -> bool {
        self.focused() == Some(id)
    }

    /// Intern an app and get a handle to it. Idempotent per slug, so reopening an
    /// app or refreshing the catalog never allocates a second handle for it.
    pub fn intern(self, manifest: &AppManifest) -> AppRef {
        let existing = self
            .registry
            .with_untracked(|r| r.iter().position(|a| a.id == manifest.id));

        match existing {
            Some(index) => {
                // Keep the newest labels for an app that is already open.
                self.registry.update(|r| r[index] = manifest.clone());
                AppRef(index as u32)
            }
            None => {
                let index = self.registry.with_untracked(|r| r.len()) as u32;
                self.registry.update(|r| r.push(manifest.clone()));
                AppRef(index)
            }
        }
    }

    /// The descriptor behind a handle.
    ///
    /// Untracked on purpose. A window's app is fixed for its lifetime, and a
    /// tracked read inside a keyed row would let a catalog refresh re-run the row's
    /// builder and remount the app, throwing away its state (terminal scrollback,
    /// current directory).
    pub fn manifest(self, app: AppRef) -> AppManifest {
        self.registry
            .with_untracked(|r| r.get(app.0 as usize).cloned())
            .unwrap_or_else(AppManifest::placeholder)
    }

    /// Open (or raise) an app from a catalog entry.
    pub fn open_manifest(self, manifest: &AppManifest) {
        self.open(self.intern(manifest));
    }

    /// Open a built-in by enum, preferring the catalog's copy of it so a
    /// server-supplied label or size wins over the compiled-in default.
    pub fn open_builtin(self, id: AppId) {
        let manifest = self
            .catalog
            .with_untracked(|apps| apps.iter().find(|a| a.builtin_id() == Some(id)).cloned())
            .unwrap_or_else(|| AppManifest::builtin(id));
        self.open_manifest(&manifest);
    }

    /// Ask an app to open a file, raising it if it is not already running.
    ///
    /// The file type decides the app, and callers decide the file type: the shell does
    /// not keep a table of who opens what.
    pub fn request_open(self, app: AppId, path: String) {
        self.pending_open.set(Some(OpenRequest { app, path }));
        self.open_builtin(app);
    }

    /// Take a request addressed to `app`, if there is one waiting.
    pub fn take_open_request(self, app: AppId) -> Option<String> {
        let request = self.pending_open.get()?;
        if request.app != app {
            return None;
        }
        self.pending_open.set(None);
        Some(request.path)
    }

    /// Open by slug, for the terminal's `open` command. Fails only when nothing
    /// advertises or implements the slug.
    pub fn open_slug(self, slug: &str) -> Result<(), String> {
        let advertised = self
            .catalog
            .with_untracked(|apps| apps.iter().find(|a| a.id == slug).cloned());

        match advertised {
            Some(manifest) => {
                self.open_manifest(&manifest);
                Ok(())
            }
            None => match AppId::from_slug(slug) {
                Some(id) => {
                    self.open_builtin(id);
                    Ok(())
                }
                None => {
                    let known = self.catalog.with_untracked(|apps| {
                        apps.iter()
                            .map(|a| a.id.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    });
                    Err(format!("unknown app `{slug}` (try: {known})"))
                }
            },
        }
    }

    /// Open an app, or raise its window if it is already running.
    pub fn open(self, app: AppRef) {
        if let Some(existing) = self
            .windows
            .with(|ws| ws.iter().find(|w| w.app == app).map(|w| w.id))
        {
            self.windows.update(|ws| {
                if let Some(w) = ws.iter_mut().find(|w| w.id == existing) {
                    w.minimized = false;
                }
            });
            self.focus(existing);
            return;
        }

        let manifest = self.manifest(app);
        let w = f64::from(manifest.default_width.max(260));
        let h = f64::from(manifest.default_height.max(150));
        let (vw, vh) = viewport();
        let step = self.windows.with(|ws| ws.len()) as f64;
        let x = (140.0 + step * CASCADE).min((vw - w - 24.0).max(16.0));
        let y = (72.0 + step * CASCADE).min((vh - h - 16.0).max(8.0));

        self.next_id.update(|n| *n += 1);
        let id = self.next_id.get_untracked();
        let z = self.bump_z();

        self.windows.update(|ws| {
            ws.push(WinData {
                id,
                app,
                x,
                y,
                w,
                h,
                z,
                minimized: false,
                maximized: false,
                restore: [x, y, w, h],
            })
        });
    }

    /// Raise a window, unless it is already on top.
    pub fn focus(self, id: u32) {
        if self.is_focused(id) {
            return;
        }
        let z = self.bump_z();
        self.edit(id, |w| {
            w.z = z;
            w.minimized = false;
        });
    }

    pub fn close(self, id: u32) {
        self.windows.update(|ws| ws.retain(|w| w.id != id));
    }

    pub fn close_all(self) {
        self.windows.update(|ws| ws.clear());
    }

    pub fn toggle_minimize(self, id: u32) {
        // Clicking the focused window's taskbar button hides it; clicking any
        // other button raises that window.
        if self.is_focused(id) {
            self.edit(id, |w| w.minimized = true);
        } else {
            let z = self.bump_z();
            self.edit(id, |w| {
                w.minimized = false;
                w.z = z;
            });
        }
    }

    pub fn toggle_maximize(self, id: u32) {
        self.edit(id, |d| {
            if d.maximized {
                let [x, y, w, h] = d.restore;
                d.maximized = false;
                (d.x, d.y, d.w, d.h) = (x, y, w, h);
                *d = d.clamped();
            } else {
                d.restore = [d.x, d.y, d.w, d.h];
                d.maximized = true;
            }
        });
    }

    /// Pull the catalog and health payload from the axum backend.
    ///
    /// The shell stays usable if this fails: `new()` already seeded the catalog
    /// with the shared defaults, and the failure shows up in Settings/About.
    pub fn refresh(self) {
        self.health.set(None);
        wasm_bindgen_futures::spawn_local(async move {
            match crate::api::fetch_catalog().await {
                Ok(catalog) => {
                    self.catalog_error.set(None);
                    self.fs.set(catalog.fs);
                    self.proxy.set(catalog.proxy);
                    self.ai.set(catalog.ai);
                    if !catalog.wallpapers.is_empty() {
                        // Only fill in a default; never replace what is already set.
                        // The Gallery can set a wallpaper from a photo, which is a
                        // `url(...)` that no catalog entry will ever match.
                        if self.wallpaper.get_untracked().is_empty() {
                            self.wallpaper.set(catalog.wallpapers[0].css.clone());
                        }
                        self.wallpapers.set(catalog.wallpapers);
                    }
                    if !catalog.apps.is_empty() {
                        self.catalog.set(catalog.apps);
                    }
                }
                Err(err) => {
                    self.catalog_error.set(Some(err.clone()));
                    self.health.set(Some(Err(err)));
                }
            }

            match crate::api::fetch_health().await {
                Ok(health) => self.health.set(Some(Ok(health))),
                Err(err) => self.health.set(Some(Err(err))),
            }
        });
    }

    /// Human-readable backend status for the tray and panels.
    pub fn status_line(self) -> String {
        // A dead backend is the more useful thing to say when both requests failed.
        if let Some(Err(err)) = self.health.get() {
            return format!("offline ({err})");
        }
        // Reachable, but the catalog was rejected: version skew, not a dead server.
        if let Some(err) = self.catalog_error.get() {
            return format!("catalog: {err}");
        }
        match self.health.get() {
            Some(Ok(h)) => format!("{} v{} · up {}s", h.name, h.version, h.uptime_secs),
            _ => "connecting…".to_owned(),
        }
    }

    pub fn is_online(self) -> bool {
        matches!(self.health.get(), Some(Ok(_)))
    }
}

fn initial_wallpaper() -> String {
    default_catalog()
        .wallpapers
        .into_iter()
        .next()
        .map(|w| w.css)
        .unwrap_or_else(|| "#0b0d12".to_owned())
}

/// `(width, height)` of the window area, i.e. the viewport minus the taskbar.
pub fn viewport() -> (f64, f64) {
    let Some(window) = web_sys::window() else {
        return (1280.0, 748.0);
    };
    let w = window
        .inner_width()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(1280.0);
    let h = window
        .inner_height()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(800.0);
    (w, (h - TASKBAR_HEIGHT).max(120.0))
}

/// `HH:MM` plus a short date, in the browser's local time.
pub fn clock_strings() -> (String, String) {
    let d = js_sys::Date::new_0();
    let time = format!("{:02}:{:02}", d.get_hours(), d.get_minutes());
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let month = MONTHS[(d.get_month() as usize).min(11)];
    let day = DAYS[(d.get_day() as usize).min(6)];
    (time, format!("{day} {} {month}", d.get_date()))
}
