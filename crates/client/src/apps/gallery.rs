//! The Gallery: the photos in a folder, as a contact sheet with a viewer.
//!
//! It browses the same file API the Files app does, and shows only what the browser
//! can actually decode — using `image_mime` for that, the shared list the server also
//! uses to decide what `/api/fs/raw` will serve. Files that *look* like images but are
//! in a format no browser decodes (TIFF, HEIC, PSD) are counted and reported rather
//! than shown as broken tiles.
//!
//! A photo can be made the desktop wallpaper. That is the shell's own state, so it is
//! one line here and the rest of the desktop follows: the wallpaper is a CSS
//! `background` value, and `url(...)` is as valid as a gradient.

use leptos::prelude::*;
use shared::fs::{DirListing, EntryKind, FsEntry, image_mime, looks_like_image};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use crate::api;
use crate::format::{format_millis, human_size};
use crate::state::Desktop;

/// Tiles are the whole file, so a huge photo is a waste of a 140px box. Above this
/// the tile says so and the viewer still opens it.
const TILE_MAX_BYTES: u64 = 12 * 1024 * 1024;

/// Everything one Gallery window is and can do.
#[derive(Clone, Copy)]
struct GalleryView {
    desktop: Desktop,
    /// Root-relative folder on screen.
    cwd: RwSignal<String>,
    entries: RwSignal<Option<Result<Vec<FsEntry>, String>>>,
    /// Listing request guard, and the reload button.
    seq: RwSignal<u64>,
    reload: RwSignal<u32>,
    show_hidden: RwSignal<bool>,
    /// Path of the photo in the viewer.
    viewing: RwSignal<Option<String>>,
    /// Photos whose bytes the browser could not decode.
    broken: RwSignal<Vec<String>>,
    /// Natural size of the photo in the viewer, once it has loaded.
    size: RwSignal<Option<(u32, u32)>>,
    notice: RwSignal<Option<String>>,
}

impl GalleryView {
    fn entries(self) -> Vec<FsEntry> {
        match self.entries.get() {
            Some(Ok(entries)) => entries
                .into_iter()
                .filter(|e| self.show_hidden.get() || !e.hidden)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Folders, so the gallery can be navigated like a file browser.
    fn folders(self) -> Vec<FsEntry> {
        self.entries()
            .into_iter()
            .filter(|e| e.kind == EntryKind::Dir && !e.blocked)
            .collect()
    }

    /// The photos: formats the browser decodes, in listing order.
    fn photos(self) -> Vec<FsEntry> {
        self.entries()
            .into_iter()
            .filter(|e| e.kind == EntryKind::File && !e.blocked && image_mime(&e.name).is_some())
            .collect()
    }

    /// Image-looking files that no browser will decode here.
    fn undecodable(self) -> usize {
        self.entries()
            .into_iter()
            .filter(|e| {
                e.kind == EntryKind::File
                    && !e.blocked
                    && image_mime(&e.name).is_none()
                    && looks_like_image(&e.name)
            })
            .count()
    }

    fn navigate(self, path: String) {
        self.viewing.set(None);
        self.broken.set(Vec::new());
        self.notice.set(None);
        self.cwd.set(path);
    }

    /// Step through the photos in the current folder.
    fn step(self, delta: isize) {
        let Some(current) = self.viewing.get_untracked() else {
            return;
        };
        let photos = self.photos();
        if photos.is_empty() {
            return;
        }
        let index = photos.iter().position(|p| p.path == current).unwrap_or(0) as isize;
        let next = (index + delta).rem_euclid(photos.len() as isize) as usize;
        self.size.set(None);
        self.viewing.set(Some(photos[next].path.clone()));
    }

    fn mark_broken(self, path: String) {
        self.broken.update(|broken| {
            if !broken.contains(&path) {
                broken.push(path);
            }
        });
    }

    /// Use a photo as the desktop background.
    fn set_wallpaper(self, path: String) {
        let url = api::raw_url(&path);
        self.desktop
            .wallpaper
            .set(format!("url('{url}') center / cover no-repeat"));
        self.notice
            .set(Some(format!("{path} is now the wallpaper")));
    }
}

#[component]
pub fn Gallery() -> impl IntoView {
    let desktop = expect_context::<Desktop>();

    let state = GalleryView {
        desktop,
        cwd: RwSignal::new(String::new()),
        entries: RwSignal::new(None),
        seq: RwSignal::new(0),
        reload: RwSignal::new(0),
        show_hidden: RwSignal::new(false),
        viewing: RwSignal::new(None),
        broken: RwSignal::new(Vec::new()),
        size: RwSignal::new(None),
        notice: RwSignal::new(None),
    };
    provide_context(state);

    // A photo handed over by another app: show its folder, then the photo. A format
    // this browser cannot decode is reported instead of opening a viewer that would
    // only fail.
    Effect::new(move |_| {
        let Some(path) = state.desktop.take_open_request(shared::AppId::Gallery) else {
            return;
        };
        let folder = match path.rsplit_once('/') {
            Some((head, _)) => head.to_owned(),
            None => String::new(),
        };
        state.navigate(folder);
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        if image_mime(&name).is_some() {
            state.size.set(None);
            state.viewing.set(Some(path));
        } else {
            state.notice.set(Some(format!(
                "{name} is an image format this browser cannot show"
            )));
        }
    });

    Effect::new(move |_| {
        let path = state.cwd.get();
        state.reload.track();
        state.show_hidden.track();
        let mine = state.seq.get_untracked() + 1;
        state.seq.set(mine);
        state.entries.set(None);
        spawn_local(async move {
            let result = api::list_dir(&path).await;
            if state.seq.get_untracked() != mine {
                return;
            }
            state.entries.set(Some(result.map(|listing: DirListing| {
                if listing.path != path {
                    state.cwd.set(listing.path.clone());
                }
                listing.entries
            })));
        });
    });

    view! {
        <div class="app gallery">
            <Show
                when=move || desktop.fs.get().enabled
                fallback=move || {
                    view! {
                        <div class="files-status error">
                            <strong>"The gallery needs the file browser."</strong>
                            <span>
                                "It is switched off on this backend; start the server with `WEB_DESKTOP_ROOT=/some/folder`."
                            </span>
                        </div>
                    }
                }
            >
                <Sheet />
                <Viewer />
            </Show>
        </div>
    }
}

/// The contact sheet: a toolbar, then folders and photos as tiles.
#[component]
fn Sheet() -> impl IntoView {
    let state = expect_context::<GalleryView>();

    view! {
        <div class="gallery-bar">
            <button
                class="ghost-btn"
                title="Up one folder"
                disabled=move || state.cwd.get().is_empty()
                on:click=move |_| {
                    let here = state.cwd.get_untracked();
                    let up = match here.rsplit_once('/') {
                        Some((head, _)) => head.to_owned(),
                        None => String::new(),
                    };
                    state.navigate(up);
                }
            >
                "↑ Up"
            </button>
            <span class="gallery-path" title=move || format!("/{}", state.cwd.get())>
                {move || format!("/{}", state.cwd.get())}
            </span>
            <span class="count">{move || {
                let photos = state.photos().len();
                let folders = state.folders().len();
                let mut text = format!("{photos} photo{}", if photos == 1 { "" } else { "s" });
                if folders > 0 {
                    text.push_str(&format!(" · {folders} folder{}", if folders == 1 { "" } else { "s" }));
                }
                text
            }}</span>
            <label class="toggle">
                <input
                    type="checkbox"
                    prop:checked=move || state.show_hidden.get()
                    on:change=move |_| state.show_hidden.update(|v| *v = !*v)
                />
                <span>"Hidden"</span>
            </label>
            <button
                class="ghost-btn"
                title="Reload this folder"
                on:click=move |_| state.reload.update(|n| *n += 1)
            >
                "⟳"
            </button>
        </div>

        <Show when=move || state.notice.get().is_some()>
            <div class="files-notice" on:click=move |_| state.notice.set(None)>
                {move || state.notice.get().unwrap_or_default()}
            </div>
        </Show>

        <div class="gallery-sheet">
            {move || match state.entries.get() {
                None => view! { <div class="files-status muted">"Looking…"</div> }.into_any(),
                Some(Err(err)) => {
                    view! {
                        <div class="files-status error">
                            <strong>"Cannot open this folder"</strong>
                            <span>{err}</span>
                        </div>
                    }
                        .into_any()
                }
                Some(Ok(_)) if state.photos().is_empty() && state.folders().is_empty() => {
                    view! {
                        <div class="files-status muted">
                            "No photos here. Folders on the way down are clickable, and hidden files can be shown above."
                        </div>
                    }
                        .into_any()
                }
                Some(Ok(_)) => {
                    view! {
                        <div class="gallery-grid">
                            <For each=move || state.folders() key=|e| e.path.clone() let:folder>
                                {
                                    let target = folder.clone();
                                    let name = folder.name.clone();
                                    let path = folder.path.clone();
                                    view! {
                                        <button
                                            class="gallery-tile folder"
                                            title=path
                                            on:click=move |_| state.navigate(target.path.clone())
                                        >
                                            <span class="tile-folder">"📁"</span>
                                            <span class="tile-name">{name}</span>
                                        </button>
                                    }
                                }
                            </For>

                            <For each=move || state.photos() key=|e| e.path.clone() let:photo>
                                <PhotoTile photo=photo />
                            </For>
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>

        <Show when=move || { state.undecodable() > 0 }>
            <p class="gallery-note">
                {move || {
                    let n = state.undecodable();
                    format!(
                        "{n} other file{} here look{} like an image, but in a format this browser cannot show.",
                        if n == 1 { "" } else { "s" },
                        if n == 1 { "s" } else { "" },
                    )
                }}
            </p>
        </Show>
    }
}

#[component]
fn PhotoTile(photo: FsEntry) -> impl IntoView {
    let state = expect_context::<GalleryView>();
    let path = photo.path.clone();
    let open_path = photo.path.clone();
    let failed_path = photo.path.clone();
    let name = photo.name.clone();
    let too_big = photo.size > TILE_MAX_BYTES;
    let url = if too_big {
        String::new()
    } else {
        api::raw_url(&photo.path)
    };
    let tooltip = format!("{} · {}", photo.name, human_size(photo.size));

    // A tile whose bytes will not decode says so instead of showing a broken icon.
    // Two closures, because each has to own the path it looks up.
    let class_path = photo.path.clone();
    let badge_path = photo.path.clone();

    view! {
        <button
            class="gallery-tile photo"
            class:broken=move || state.broken.get().contains(&class_path)
            title=tooltip
            on:click=move |_| {
                state.size.set(None);
                state.viewing.set(Some(open_path.clone()));
            }
        >
            <img
                class="tile-image"
                style=move || if too_big { "display:none" } else { "" }
                src=url
                loading="lazy"
                decoding="async"
                alt=""
                on:error=move |_| state.mark_broken(failed_path.clone())
            />
            <span
                class="tile-fallback"
                style=move || if too_big { "" } else { "display:none" }
            >
                "too large to preview"
            </span>
            <span class="tile-name">{name}</span>
            <span
                class="tile-badge"
                style=move || {
                    if state.broken.get().contains(&badge_path) { "" } else { "display:none" }
                }
            >
                "unreadable"
            </span>
            <span class="tile-hint">{path}</span>
        </button>
    }
}

/// The viewer: full size, with the metadata and the keys people expect.
#[component]
fn Viewer() -> impl IntoView {
    let state = expect_context::<GalleryView>();
    let focus = NodeRef::<leptos::html::Div>::new();

    let current = move || {
        let path = state.viewing.get()?;
        state.photos().into_iter().find(|p| p.path == path)
    };

    // The overlay takes focus so the arrow keys and Escape work without a click.
    Effect::new(move |_| {
        if state.viewing.get().is_some()
            && let Some(el) = focus.get()
        {
            let _ = el.focus();
        }
    });

    let on_load = move |ev: web_sys::Event| {
        if let Some(img) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlImageElement>().ok())
        {
            state
                .size
                .set(Some((img.natural_width(), img.natural_height())));
        }
    };

    view! {
        <Show when=move || state.viewing.get().is_some()>
            <div
                class="gallery-viewer"
                node_ref=focus
                tabindex="0"
                on:keydown=move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
                    "Escape" => state.viewing.set(None),
                    "ArrowRight" => state.step(1),
                    "ArrowLeft" => state.step(-1),
                    _ => {}
                }
            >
                <div class="viewer-frame">
                    {
                        let path = move || state.viewing.get().unwrap_or_default();
                        let raw = move || api::raw_url(&path());
                        view! {
                            <img class="viewer-image" src=raw alt="" on:load=on_load />
                        }
                    }
                </div>
                <div class="viewer-bar">
                    <button class="ghost-btn" title="Previous (←)" on:click=move |_| state.step(-1)>
                        "←"
                    </button>
                    <button class="ghost-btn" title="Next (→)" on:click=move |_| state.step(1)>
                        "→"
                    </button>
                    <span class="viewer-name">
                        {move || current().map(|p| p.name).unwrap_or_default()}
                    </span>
                    <span class="viewer-meta">{move || {
                        let Some(photo) = current() else { return String::new() };
                        let size = state
                            .size
                            .get()
                            .map(|(w, h)| format!("{w} × {h} px"))
                            .unwrap_or_else(|| "…".to_owned());
                        let when = photo.modified.map(format_millis).unwrap_or_default();
                        format!("{size} · {} · {when}", human_size(photo.size))
                    }}</span>
                    <span class="editor-spacer"></span>
                    <button
                        class="ghost-btn"
                        title="Use this photo as the desktop wallpaper"
                        on:click=move |_| {
                            if let Some(path) = state.viewing.get_untracked() {
                                state.set_wallpaper(path);
                            }
                        }
                    >
                        "Set as wallpaper"
                    </button>
                    <a
                        class="ghost-btn"
                        href=move || api::download_url(&state.viewing.get().unwrap_or_default())
                        download=move || current().map(|p| p.name).unwrap_or_default()
                    >
                        "⭳ Download"
                    </a>
                    <button class="ghost-btn" title="Close (Esc)" on:click=move |_| state.viewing.set(None)>
                        "✕"
                    </button>
                </div>
            </div>
        </Show>
    }
}
