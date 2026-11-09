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
use shared::{AppId, AppManifest, Health, Wallpaper, default_catalog};

/// Height of the taskbar in CSS pixels; the window area excludes it.
pub const TASKBAR_HEIGHT: f64 = 52.0;

/// Cascade step for newly opened windows.
const CASCADE: f64 = 34.0;

/// Geometry of one window. `Copy` on purpose: every field is a scalar, which is
/// what lets the whole list live in a single signal cheaply.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WinData {
    pub id: u32,
    pub app: AppId,
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
    pub const fn title(self) -> &'static str {
        self.app.name()
    }

    pub const fn glyph(self) -> &'static str {
        self.app.glyph()
    }

    pub const fn accent(self) -> &'static str {
        self.app.accent()
    }

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

    /// Inline style for the window frame. `--accent` feeds the title bar color.
    pub fn css(self) -> String {
        let accent = self.accent();
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
    /// Catalog as advertised by the backend, preloaded with the shared default.
    pub catalog: RwSignal<Vec<AppManifest>>,
    pub wallpapers: RwSignal<Vec<Wallpaper>>,
    pub wallpaper: RwSignal<String>,
    pub start_open: RwSignal<bool>,
    /// `None` while the first request is in flight, `Some(Err)` if it failed.
    pub health: RwSignal<Option<Result<Health, String>>>,
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
            wallpapers: RwSignal::new(fallback.wallpapers),
            wallpaper: RwSignal::new(initial_wallpaper()),
            start_open: RwSignal::new(false),
            health: RwSignal::new(None),
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

    /// Open an app, or raise its window if it is already running.
    pub fn open(self, app: AppId) {
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

        let (w, h) = app.default_size();
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
                    if !catalog.wallpapers.is_empty() {
                        let current = self.wallpaper.get_untracked();
                        if !catalog.wallpapers.iter().any(|w| w.css == current) {
                            self.wallpaper.set(catalog.wallpapers[0].css.clone());
                        }
                        self.wallpapers.set(catalog.wallpapers);
                    }
                    if !catalog.apps.is_empty() {
                        self.catalog.set(catalog.apps);
                    }
                }
                Err(err) => self.health.set(Some(Err(err))),
            }

            match crate::api::fetch_health().await {
                Ok(health) => self.health.set(Some(Ok(health))),
                Err(err) => self.health.set(Some(Err(err))),
            }
        });
    }

    /// Human-readable backend status for the tray and panels.
    pub fn status_line(self) -> String {
        match self.health.get() {
            None => "connecting…".to_owned(),
            Some(Ok(h)) => format!("{} v{} · up {}s", h.name, h.version, h.uptime_secs),
            Some(Err(e)) => format!("offline ({e})"),
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
