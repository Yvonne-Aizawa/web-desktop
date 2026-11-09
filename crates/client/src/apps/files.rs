//! The Files app: a real directory on the machine running the backend.
//!
//! Nothing here touches a disk. Every listing and preview is a request to
//! `/api/fs/*`, which is fenced inside one root directory on the server. The app
//! is read-only, which is why there is no rename/delete UI: the backend
//! advertises `FsInfo::writable == false`.

use leptos::prelude::*;
use shared::fs::{DirListing, EntryKind, FilePreview, FsEntry, PreviewBody};
use wasm_bindgen_futures::spawn_local;

use crate::api;
use crate::format::{format_millis, human_size};
use crate::state::Desktop;

/// State of the current directory listing.
#[derive(Clone, PartialEq)]
enum Load {
    Loading,
    Ready(DirListing),
    Failed(String),
}

#[component]
pub fn Files() -> impl IntoView {
    let desktop = expect_context::<Desktop>();
    let fs = move || desktop.fs.get();

    // Root-relative path of the directory on screen; `""` is the root.
    let cwd = RwSignal::new(String::new());
    let load = RwSignal::new(Load::Loading);
    let selected = RwSignal::new(None::<FsEntry>);
    let preview = RwSignal::new(None::<Result<FilePreview, String>>);
    let show_hidden = RwSignal::new(false);
    // Bumped on every navigation so a slow reply cannot overwrite a newer one.
    let seq = RwSignal::new(0u64);
    // Bumped by the reload button.
    let reload = RwSignal::new(0u64);

    // Reload whenever the directory changes, or when a reload is asked for.
    Effect::new(move |_| {
        let path = cwd.get();
        reload.track();
        let mine = seq.get_untracked() + 1;
        seq.set(mine);
        load.set(Load::Loading);
        selected.set(None);
        preview.set(None);

        spawn_local(async move {
            let result = api::list_dir(&path).await;
            if seq.get_untracked() != mine {
                return;
            }
            load.set(match result {
                Ok(listing) => {
                    // The server canonicalizes paths, so a folder reached through
                    // a symlink reports its real location. Adopt it, otherwise the
                    // path bar and the Up button would describe a different
                    // directory than the one being shown. The refetch this
                    // triggers settles immediately: the second listing's path is
                    // already canonical.
                    if listing.path != path {
                        cwd.set(listing.path.clone());
                    }
                    Load::Ready(listing)
                }
                Err(err) => Load::Failed(err),
            });
        });
    });

    let navigate = move |path: String| cwd.set(path);

    let select = move |entry: FsEntry| {
        match entry.kind {
            // A symlink pointing outside the root is listed, but opening it would
            // only earn a 403, so explain it instead.
            EntryKind::Dir if !entry.blocked => {
                navigate(entry.path.clone());
                return;
            }
            EntryKind::File if !entry.blocked => {}
            _ => {
                let reason = if entry.blocked {
                    "This link points outside the file browser root, so it is not readable."
                } else {
                    "Not a regular file, so there is nothing to preview."
                };
                selected.set(Some(entry));
                preview.set(Some(Err(reason.to_owned())));
                return;
            }
        }

        selected.set(Some(entry.clone()));
        preview.set(None);
        let path = entry.path.clone();
        let mine = seq.get_untracked();
        spawn_local(async move {
            let result = api::preview_file(&path).await;
            if seq.get_untracked() != mine {
                return;
            }
            preview.set(Some(result));
        });
    };

    // "Sandbox / notes / work" as (label, path) pairs.
    let crumbs = move || {
        let Load::Ready(listing) = load.get() else {
            return Vec::new();
        };
        let mut out = vec![("Sandbox".to_owned(), String::new())];
        let mut acc = String::new();
        for part in listing.path.split('/').filter(|p| !p.is_empty()) {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            out.push((part.to_owned(), acc.clone()));
        }
        out
    };

    let visible = move || match load.get() {
        Load::Ready(listing) => listing
            .entries
            .iter()
            .filter(|e| show_hidden.get() || !e.hidden)
            .cloned()
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    let summary = move || match load.get() {
        Load::Ready(listing) => {
            let hidden = listing.entries.iter().filter(|e| e.hidden).count();
            let shown = visible().len();
            let mut text = format!("{shown} item{}", if shown == 1 { "" } else { "s" });
            if hidden > 0 && !show_hidden.get() {
                text.push_str(&format!(" · {hidden} hidden"));
            }
            if listing.truncated {
                text.push_str(" · truncated");
            }
            text
        }
        Load::Loading => "loading…".to_owned(),
        Load::Failed(_) => "unavailable".to_owned(),
    };

    view! {
        <div class="app files">
            <Show
                when=move || fs().enabled
                fallback=move || {
                    view! {
                        <div class="files-status error">
                            <strong>"The file browser is switched off on the backend."</strong>
                            <span>
                                "Start the server with `WEB_DESKTOP_ROOT=/some/folder`, or remove `WEB_DESKTOP_FS=off`."
                            </span>
                        </div>
                    }
                }
            >
                <aside class="files-side">
                    <div class="side-label">"Sandbox"</div>
                    <button
                        class="side-item"
                        class:active=move || cwd.get().is_empty()
                        on:click=move |_| navigate(String::new())
                    >
                        <span class="side-dot"></span>
                        "Root"
                    </button>

                    <div class="side-label">"Options"</div>
                    <label class="toggle">
                        <input
                            type="checkbox"
                            prop:checked=move || show_hidden.get()
                            on:change=move |_| show_hidden.update(|v| *v = !*v)
                        />
                        <span>"Show hidden files"</span>
                    </label>

                    <div class="side-label">"Serving"</div>
                    <p class="side-note" title=move || fs().root.clone()>
                        {move || fs().root.clone()}
                    </p>
                    <p class="side-fine">
                        "Read-only, fenced to this folder. Set WEB_DESKTOP_ROOT to change it."
                    </p>
                </aside>

                <div class="files-main">
                    <div class="files-bar">
                        <button
                            class="ghost-btn"
                            disabled=move || !matches!(load.get(), Load::Ready(l) if l.parent.is_some())
                            on:click=move |_| {
                                if let Load::Ready(listing) = load.get_untracked()
                                    && let Some(parent) = listing.parent
                                {
                                    navigate(parent);
                                }
                            }
                        >
                            "↑ Up"
                        </button>

                        <nav class="crumbs">
                            <For each=crumbs key=|(_, path)| path.clone() let:crumb>
                                {
                                    let target = crumb.1.clone();
                                    let current = crumb.1.clone();
                                    view! {
                                        <button
                                            class="crumb-seg"
                                            class:current=move || {
                                                matches!(load.get(), Load::Ready(l) if l.path == current)
                                            }
                                            on:click=move |_| navigate(target.clone())
                                        >
                                            {crumb.0.clone()}
                                        </button>
                                    }
                                }
                            </For>
                        </nav>

                        <span class="count">{summary}</span>

                        <button
                            class="ghost-btn"
                            title="Reload this folder"
                            on:click=move |_| reload.update(|n| *n += 1)
                        >
                            "⟳"
                        </button>
                    </div>

                    <div class="files-list">
                        {move || match load.get() {
                            Load::Loading => {
                                view! { <div class="files-status muted">"Loading…"</div> }
                                    .into_any()
                            }
                            Load::Failed(err) => {
                                view! {
                                    <div class="files-status error">
                                        <strong>"Cannot open this folder"</strong>
                                        <span>{err}</span>
                                    </div>
                                }
                                    .into_any()
                            }
                            Load::Ready(listing) if listing.entries.is_empty() => {
                                view! {
                                    <div class="files-status muted">"This folder is empty."</div>
                                }
                                    .into_any()
                            }
                            Load::Ready(_) => {
                                view! {
                                    <For each=visible key=|e| e.path.clone() let:entry>
                                        {
                                            let selected_path = entry.path.clone();
                                            let icon = icon_for(&entry);
                                            let size = match entry.kind {
                                                EntryKind::File if !entry.blocked => human_size(entry.size),
                                                _ => String::new(),
                                            };
                                            let when = entry
                                                .modified
                                                .map(format_millis)
                                                .unwrap_or_default();
                                            let tooltip = entry.path.clone();
                                            let target = entry.clone();
                                            let is_dir = entry.kind == EntryKind::Dir;
                                            let blocked = entry.blocked;
                                            let name = entry.name.clone();
                                            view! {
                                                <button
                                                    class="row"
                                                    class:dir=is_dir
                                                    class:blocked=blocked
                                                    class:selected=move || {
                                                        selected
                                                            .get()
                                                            .is_some_and(|s| s.path == selected_path)
                                                    }
                                                    title=tooltip
                                                    on:click=move |_| select(target.clone())
                                                >
                                                    <span class="row-icon">{icon}</span>
                                                    <span class="row-name">{name}</span>
                                                    <span class="row-size">{size}</span>
                                                    <span class="row-when">{when}</span>
                                                </button>
                                            }
                                        }
                                    </For>
                                }
                                    .into_any()
                            }
                        }}
                    </div>

                    <div class="files-preview">{move || preview_pane(selected.get(), preview.get())}</div>
                </div>
            </Show>
        </div>
    }
}

fn icon_for(entry: &FsEntry) -> &'static str {
    match (entry.blocked, entry.kind) {
        (true, _) => "⛔",
        (_, EntryKind::Dir) => "📁",
        (_, EntryKind::File) if entry.hidden => "🗒",
        (_, EntryKind::File) => "📄",
        _ => "◦",
    }
}

/// The bottom pane: nothing selected, loading, an error, or a file body.
fn preview_pane(
    selected: Option<FsEntry>,
    preview: Option<Result<FilePreview, String>>,
) -> AnyView {
    let Some(entry) = selected else {
        return view! {
            <span class="muted">
                "Select a file to preview it. Nothing leaves this folder: the backend reads it."
            </span>
        }
        .into_any();
    };

    match preview {
        None => view! {
            <div class="preview">
                {preview_head(entry)}
                <p class="muted">"Loading…"</p>
            </div>
        }
        .into_any(),
        Some(Err(err)) => view! {
            <div class="preview">
                {preview_head(entry)}
                <p class="preview-error">{err}</p>
            </div>
        }
        .into_any(),
        Some(Ok(file)) => {
            let size = human_size(file.entry.size);
            let head = preview_head(file.entry.clone());
            let body = match file.body {
                PreviewBody::Text { content, truncated } => view! {
                    <p class="preview-note" style=move || {
                        if truncated { "" } else { "display:none" }
                    }>
                        "Showing only the first 256 KiB of this file."
                    </p>
                    <pre>{content}</pre>
                }
                .into_any(),
                PreviewBody::Binary { mime } => view! {
                    <p class="muted">
                        "Binary file (" {mime} ", " {size} ") — no preview. Use Download to save it."
                    </p>
                }
                .into_any(),
            };
            view! {
                <div class="preview">
                    {head}
                    {body}
                </div>
            }
            .into_any()
        }
    }
}

/// Name, size, timestamp and a download link for the selected entry.
fn preview_head(entry: FsEntry) -> AnyView {
    let downloadable = entry.kind == EntryKind::File && !entry.blocked;
    let meta = if downloadable {
        human_size(entry.size)
    } else {
        String::new()
    };
    let when = entry.modified.map(format_millis).unwrap_or_default();
    let url = api::download_url(&entry.path);
    let download_name = entry.name.clone();

    view! {
        <div class="preview-head">
            <span class="preview-name">{entry.name.clone()}</span>
            <span class="preview-meta">{meta}</span>
            <span class="preview-meta">{when}</span>
            // A plain `style` toggle rather than `<Show>`: the children of `Show`
            // must be callable more than once, and an attribute that consumes an
            // owned `String` would make that closure `FnOnce`.
            <a
                class="ghost-btn"
                href=url
                download=download_name.clone()
                style=move || if downloadable { "" } else { "display:none" }
            >
                "⭳ Download"
            </a>
        </div>
    }
    .into_any()
}
