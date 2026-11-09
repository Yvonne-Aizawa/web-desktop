//! The Files app: a real directory on the machine running the backend.
//!
//! Nothing here touches a disk. Every listing, preview and change is a request to
//! `/api/fs/*`, which is fenced inside one root directory on the server. Text and
//! binary previews arrive as JSON; images are rendered by pointing an `<img>` at
//! `/api/fs/raw`.
//!
//! This app deliberately does not *edit*: the preview is read-only, and a file is
//! handed to the Text Editor with "Open with…". Creating, renaming, uploading and
//! deleting are here, since those are about the folder; changing contents belongs to
//! the app that owns content. Management is offered only when the server says the root
//! is writable, and the reason it is not is shown rather than a dead button.
//!
//! All of a window's state lives in [`FsView`], a `Copy` handle provided to this
//! component's subtree, so the toolbar, the listing and the preview pane reach the
//! same signals without threading closures through props. Each Files window creates
//! its own, so two of them do not share a directory, a selection or an editor.

use gloo_timers::callback::Timeout;
use leptos::prelude::*;
use shared::fs::{
    DirListing, EntryKind, FilePreview, FsEntry, PreviewBody, TrashEntry, TrashListing, image_mime,
    looks_like_image,
};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::api;
use crate::format::{format_millis, format_short_millis, human_size};
use crate::state::Desktop;

/// Thumbnails are the whole file: there is no server-side decoding, so a 40 MB
/// photo would be pulled down for a 20px box. Above this size the row keeps its
/// icon; the preview pane still shows the image.
const THUMBNAIL_MAX_BYTES: u64 = 12 * 1024 * 1024;

/// State of the current directory listing.
#[derive(Clone, PartialEq)]
enum Load {
    Loading,
    Ready(DirListing),
    Failed(String),
}

/// Which pane the window is showing.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Browse,
    Trash,
}

/// What the toolbar's inline prompt is asking for.
#[derive(Clone, PartialEq)]
enum Prompt {
    NewFolder,
    NewFile,
    Rename { path: String, current: String },
}

impl Prompt {
    fn title(&self) -> &'static str {
        match self {
            Self::NewFolder => "New folder:",
            Self::NewFile => "New file:",
            Self::Rename { .. } => "Rename to:",
        }
    }
}

/// Everything one Files window is and can do.
#[derive(Clone, Copy)]
struct FsView {
    desktop: Desktop,
    /// Root-relative path on screen; `""` is the root.
    cwd: RwSignal<String>,
    load: RwSignal<Load>,
    selected: RwSignal<Option<FsEntry>>,
    preview: RwSignal<Option<Result<FilePreview, String>>>,
    show_hidden: RwSignal<bool>,
    /// Bumped after a mutation, to re-list without disturbing the selection.
    refresh: RwSignal<u32>,
    /// Guards against a slow listing overwriting a newer one.
    seq: RwSignal<u64>,
    /// What the last listing was for, to tell a refresh from a move.
    loaded_path: RwSignal<Option<String>>,
    prompt: RwSignal<Option<Prompt>>,
    prompt_value: RwSignal<String>,
    confirming: RwSignal<bool>,
    busy: RwSignal<bool>,
    /// `(is_error, message)` shown under the toolbar.
    notice: RwSignal<Option<(bool, String)>>,
    /// Browsing the sandbox, or looking at what was deleted.
    mode: RwSignal<Mode>,
    /// The trash listing, loaded on demand rather than with every directory.
    /// `None` means "not fetched yet", so an empty trash and an unasked question do
    /// not look the same.
    trash: RwSignal<Option<Result<TrashListing, String>>>,
    trash_refresh: RwSignal<u32>,
    /// The last thing deleted, offered back until something else happens.
    undo: RwSignal<Option<TrashEntry>>,
}

impl FsView {
    fn writable(self) -> bool {
        self.desktop.fs.get().writable
    }

    fn reload(self) {
        self.refresh.update(|n| *n += 1);
    }

    fn say(self, message: impl Into<String>) {
        self.notice.set(Some((false, message.into())));
    }

    fn warn(self, message: impl Into<String>) {
        self.notice.set(Some((true, message.into())));
    }

    /// Go to a folder. Also leaves the trash view: a place in the sidebar or a folder
    /// in the listing means browsing, and a pane that stayed on the trash while the
    /// path said otherwise would be lying about which of the two it is showing.
    fn navigate(self, path: String) {
        self.mode.set(Mode::Browse);
        self.cwd.set(path);
    }

    /// Run a mutation, then re-list. The selection survives, so an open editor stays
    /// open and a rename keeps its file in view.
    fn run<F>(self, message: String, task: F)
    where
        F: std::future::Future<Output = Result<(), String>> + 'static,
    {
        self.busy.set(true);
        self.notice.set(None);
        spawn_local(async move {
            match task.await {
                Ok(()) => {
                    self.say(message);
                    self.reload();
                }
                Err(err) => self.warn(err),
            }
            self.busy.set(false);
        });
    }

    fn select(self, entry: FsEntry) {
        match entry.kind {
            // A symlink pointing outside the root is listed, but opening it would
            // only earn a 403, so explain it instead.
            EntryKind::Dir if !entry.blocked => {
                self.navigate(entry.path.clone());
                return;
            }
            EntryKind::File if !entry.blocked => {}
            _ => {
                let reason = if entry.blocked {
                    "This link points outside the file browser root, so it is not readable."
                } else {
                    "Not a regular file, so there is nothing to preview."
                };
                self.selected.set(Some(entry));
                self.preview.set(Some(Err(reason.to_owned())));
                return;
            }
        }

        self.selected.set(Some(entry.clone()));
        self.preview.set(None);
        let mine = self.seq.get_untracked();
        let path = entry.path.clone();
        spawn_local(async move {
            let result = api::preview_file(&path).await;
            if self.seq.get_untracked() != mine {
                return;
            }
            self.preview.set(Some(result));
        });
    }

    fn confirm_prompt(self) {
        let Some(kind) = self.prompt.get_untracked() else {
            return;
        };
        let name = self.prompt_value.get_untracked().trim().to_owned();
        if name.is_empty() {
            self.warn("a name is required");
            return;
        }
        let here = self.cwd.get_untracked();
        self.prompt.set(None);
        match kind {
            Prompt::NewFolder => self.run(format!("Created {name}/"), async move {
                api::create_dir(&here, &name).await.map(|_| ())
            }),
            Prompt::NewFile => self.run(format!("Created {name}"), async move {
                api::write_file(&join(&here, &name), b"").await.map(|_| ())
            }),
            Prompt::Rename { path, .. } => self.run(format!("Renamed to {name}"), async move {
                api::rename_entry(&path, &name).await.map(|_| ())
            }),
        }
    }

    /// Move something to the trash.
    ///
    /// No confirmation: the point of a trash is that this is reversible, and a dialog
    /// that says "really delete?" while nothing is really deleted teaches the user not
    /// to believe dialogs. The undo bar is the honest version of that confirmation.
    fn delete(self, path: String) {
        self.busy.set(true);
        self.notice.set(None);
        spawn_local(async move {
            match api::delete_entry(&path).await {
                Ok(entry) => {
                    self.say(format!("Moved {} to the trash", entry.name));
                    self.undo.set(Some(entry));
                    self.reload();
                    self.load_trash();
                }
                Err(err) => self.warn(err),
            }
            self.busy.set(false);
        });
    }

    /// Put back what was just deleted.
    fn undo_delete(self) {
        let Some(entry) = self.undo.get_untracked() else {
            return;
        };
        self.undo.set(None);
        self.busy.set(true);
        spawn_local(async move {
            match api::restore_trash(&entry.id).await {
                Ok(back) => {
                    self.say(format!("Put {} back", back.path));
                    self.reload();
                    self.load_trash();
                }
                Err(err) => self.warn(err),
            }
            self.busy.set(false);
        });
    }

    fn restore(self, entry: TrashEntry) {
        self.busy.set(true);
        self.notice.set(None);
        spawn_local(async move {
            match api::restore_trash(&entry.id).await {
                Ok(back) => {
                    self.say(format!("Put {} back", back.path));
                    self.reload();
                    self.load_trash();
                }
                Err(err) => self.warn(err),
            }
            self.busy.set(false);
        });
    }

    /// Delete from the trash, which is the irreversible one.
    fn purge(self, id: String) {
        self.busy.set(true);
        self.notice.set(None);
        spawn_local(async move {
            match api::purge_trash(&id).await {
                Ok(()) => {
                    self.say("Deleted for good");
                    self.load_trash();
                }
                Err(err) => self.warn(err),
            }
            self.busy.set(false);
        });
    }

    fn empty_trash(self) {
        self.busy.set(true);
        self.confirming.set(false);
        self.notice.set(None);
        spawn_local(async move {
            match api::purge_trash("").await {
                Ok(()) => {
                    self.say("Emptied the trash");
                    self.load_trash();
                }
                Err(err) => self.warn(err),
            }
            self.busy.set(false);
        });
    }

    fn show_trash(self, on: bool) {
        self.mode.set(if on { Mode::Trash } else { Mode::Browse });
        self.undo.set(None);
        if on {
            self.trash.set(None);
            self.load_trash();
        }
    }

    fn load_trash(self) {
        self.trash_refresh.update(|n| *n += 1);
    }

    /// Move the folder being viewed to the trash, then step out to its parent.
    ///
    /// Folders are entered by a single click rather than selected, so they have no
    /// preview pane to act from; the toolbar is where the folder you are inside can be
    /// renamed or deleted, which is also the only place it makes sense to offer it.
    fn delete_current(self) {
        let path = self.cwd.get_untracked();
        if path.is_empty() {
            return;
        }
        let parent = path
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_owned())
            .unwrap_or_default();
        self.busy.set(true);
        self.notice.set(None);
        spawn_local(async move {
            match api::delete_entry(&path).await {
                Ok(entry) => {
                    // Leave before the folder on screen stops existing.
                    self.navigate(parent);
                    self.say(format!("Moved {} to the trash", entry.name));
                    self.undo.set(Some(entry));
                    self.reload();
                    self.load_trash();
                }
                Err(err) => self.warn(err),
            }
            self.busy.set(false);
        });
    }

    /// Whether the folder on screen is one the server protects, so the toolbar can
    /// leave out the buttons that would only come back as a refusal.
    fn cwd_protected(self) -> bool {
        let fs = self.desktop.fs.get();
        shared::fs::protected_by(&self.cwd.get(), &fs.readonly_paths).is_some()
    }

    fn upload(self, file: web_sys::File) {
        let name = file.name();
        let target = join(&self.cwd.get_untracked(), &name);
        self.busy.set(true);
        self.notice.set(None);
        spawn_local(async move {
            let bytes = match JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => js_sys::Uint8Array::new(&buffer).to_vec(),
                Err(_) => {
                    self.warn(format!("could not read {name}"));
                    self.busy.set(false);
                    return;
                }
            };
            match api::write_file_bytes(&target, &bytes).await {
                Ok(entry) => {
                    self.say(format!("Uploaded {name} ({})", human_size(entry.size)));
                    self.reload();
                }
                Err(err) => self.warn(err),
            }
            self.busy.set(false);
        });
    }
}

/// `"folder" + "name"` -> `"folder/name"`, or just the name at the root.
fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

#[component]
pub fn Files() -> impl IntoView {
    let desktop = expect_context::<Desktop>();

    let state = FsView {
        desktop,
        cwd: RwSignal::new(String::new()),
        load: RwSignal::new(Load::Loading),
        selected: RwSignal::new(None),
        preview: RwSignal::new(None),
        show_hidden: RwSignal::new(false),
        refresh: RwSignal::new(0),
        seq: RwSignal::new(0),
        loaded_path: RwSignal::new(None),
        prompt: RwSignal::new(None),
        prompt_value: RwSignal::new(String::new()),
        confirming: RwSignal::new(false),
        busy: RwSignal::new(false),
        notice: RwSignal::new(None),
        mode: RwSignal::new(Mode::Browse),
        trash: RwSignal::new(None),
        trash_refresh: RwSignal::new(0),
        undo: RwSignal::new(None),
    };
    provide_context(state);

    // Reload when the directory changes, or when a mutation asks for it.
    Effect::new(move |_| {
        let path = state.cwd.get();
        state.refresh.track();
        let mine = state.seq.get_untracked() + 1;
        state.seq.set(mine);

        // A reload keeps the selection (the editor stays open); moving does not.
        let moving = state.loaded_path.get_untracked().as_deref() != Some(path.as_str());
        if moving {
            state.selected.set(None);
            state.preview.set(None);
        }
        state.loaded_path.set(Some(path.clone()));
        state.load.set(Load::Loading);

        spawn_local(async move {
            let result = api::list_dir(&path).await;
            if state.seq.get_untracked() != mine {
                return;
            }
            state.load.set(match result {
                Ok(listing) => {
                    // The server canonicalizes, so a folder reached through a symlink
                    // reports its real location. Adopt it, otherwise the path bar and
                    // the Up button would describe a different directory than the one
                    // being shown. The refetch this triggers settles immediately,
                    // because the second listing's path is already canonical.
                    if listing.path != path {
                        state.loaded_path.set(Some(listing.path.clone()));
                        state.cwd.set(listing.path.clone());
                    }
                    Load::Ready(listing)
                }
                Err(err) => Load::Failed(err),
            });
        });
    });

    // The trash is fetched when it is asked for, and again after any change to it.
    Effect::new(move |_| {
        if state.mode.get() != Mode::Trash {
            return;
        }
        state.trash_refresh.track();
        spawn_local(async move {
            let listing = api::list_trash().await;
            state.trash.set(Some(listing));
        });
    });

    view! {
        <div class="app files">
            <Show
                when=move || desktop.fs.get().enabled
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
                <Sidebar />
                <div class="files-main">
                    <UndoBar />
                    <Show when=move || state.mode.get() == Mode::Browse fallback=|| view! { <TrashView /> }>
                        <Toolbar />
                        <Listing />
                        <PreviewPane />
                    </Show>
                </div>
            </Show>
        </div>
    }
}

/// The one-shot offer to undo, shown after a delete.
///
/// It carries the second half of "delete is safe now": the trash holds the file
/// whether or not you notice, and this is the shortcut for the common case where you
/// deleted the wrong thing a second ago.
#[component]
fn UndoBar() -> impl IntoView {
    let state = expect_context::<FsView>();

    // Clear itself if it is left alone, but only if it is still showing the same
    // entry: two deletes in a row must not have the first one's timer wipe the second.
    Effect::new(move |_| {
        let Some(entry) = state.undo.get() else {
            return;
        };
        let id = entry.id.clone();
        let state = state;
        Timeout::new(12_000, move || {
            if state.undo.get_untracked().map(|held| held.id) == Some(id.clone()) {
                state.undo.set(None);
            }
        })
        .forget();
    });

    view! {
        <Show when=move || state.undo.get().is_some()>
            <div class="undo-bar">
                <span class="undo-text">
                    {move || {
                        state
                            .undo
                            .get()
                            .map(|entry| format!("Moved {} to the trash", entry.name))
                            .unwrap_or_default()
                    }}
                </span>
                <button class="ghost-btn" on:click=move |_| state.undo_delete()>
                    "↩ Undo"
                </button>
                <button
                    class="icon-btn"
                    title="Dismiss"
                    on:click=move |_| state.undo.set(None)
                >
                    "✕"
                </button>
            </div>
        </Show>
    }
}

/// What has been deleted: put it back, or delete it for good.
#[component]
fn TrashView() -> impl IntoView {
    let state = expect_context::<FsView>();
    let deleting = move || state.confirming.get();

    let listing = move || state.trash.get().and_then(Result::ok);
    let failure = move || state.trash.get().and_then(Result::err);
    let empty = move || {
        listing()
            .map(|listing| listing.entries.is_empty())
            .unwrap_or(false)
    };

    view! {
        <div class="trash-view">
            <div class="trash-head">
                <span class="trash-title">"Trash"</span>
                <span class="preview-meta">
                    {move || {
                        listing()
                            .map(|listing| {
                                if listing.entries.is_empty() {
                                    String::new()
                                } else {
                                    format!(
                                        "{} ({} held)",
                                        human_size(listing.total_bytes),
                                        count(listing.entries.len()),
                                    )
                                }
                            })
                            .unwrap_or_default()
                    }}
                </span>
                <span class="spacer"></span>
                <button
                    class="ghost-btn"
                    on:click=move |_| state.show_trash(false)
                >
                    "← Back to files"
                </button>
                <button
                    class="ghost-btn danger"
                    class:confirming=deleting
                    style=move || if empty() { "display:none" } else { "" }
                    on:click=move |_| {
                        if state.confirming.get_untracked() {
                            state.empty_trash();
                        } else {
                            state.confirming.set(true);
                        }
                    }
                >
                    {move || {
                        if deleting() { "Really delete everything?" } else { "Empty trash" }
                    }}
                </button>
            </div>

            <Show when=move || failure().is_some()>
                <div class="files-status error">
                    {move || failure().unwrap_or_default()}
                </div>
            </Show>

            <Show when=move || empty() && failure().is_none()>
                <div class="files-status">
                    <strong>"The trash is empty."</strong>
                    <span>"Deleting a file moves it here, so it can be put back."</span>
                </div>
            </Show>

            <Show when=move || !empty() && failure().is_none()>
                <div class="trash-list">
                    <For
                        each=move || listing().map(|l| l.entries).unwrap_or_default()
                        key=|entry| entry.id.clone()
                        let:entry
                    >
                        <TrashRow entry=entry />
                    </For>
                </div>
            </Show>

            <Show when=move || state.trash.get().is_none()>
                <div class="files-status">"Reading the trash…"</div>
            </Show>
        </div>
    }
}

#[component]
fn TrashRow(entry: TrashEntry) -> impl IntoView {
    let state = expect_context::<FsView>();
    let restore_id = entry.id.clone();
    let purge_id = entry.id.clone();
    let glyph = if entry.kind == EntryKind::Dir {
        "📁"
    } else {
        "📄"
    };

    view! {
        <div class="trash-row">
            <span class="trash-glyph">{glyph}</span>
            <span class="trash-name" title=entry.path.clone()>{entry.name.clone()}</span>
            <span class="preview-meta trash-where">{entry.path.clone()}</span>
            <span class="preview-meta">{human_size(entry.size)}</span>
            <span class="preview-meta">{format_millis(entry.deleted_at)}</span>
            <button
                class="ghost-btn"
                title="Put this back where it came from"
                on:click=move |_| state.restore(TrashEntry {
                    id: restore_id.clone(),
                    ..entry.clone()
                })
            >
                "↩ Restore"
            </button>
            <button
                class="ghost-btn danger"
                title="Delete for good: this cannot be undone"
                on:click=move |_| state.purge(purge_id.clone())
            >
                "Delete forever"
            </button>
        </div>
    }
}

/// "1 entry" / "3 entries", so the header does not read like a template.
fn count(n: usize) -> String {
    if n == 1 {
        "1 entry".to_owned()
    } else {
        format!("{n} entries")
    }
}

#[component]
fn Sidebar() -> impl IntoView {
    let state = expect_context::<FsView>();
    let fs = move || state.desktop.fs.get();

    view! {
        <aside class="files-side">
            <div class="side-label">"Sandbox"</div>
            <button
                class="side-item"
                class:active=move || state.cwd.get().is_empty()
                on:click=move |_| state.navigate(String::new())
            >
                <span class="side-dot"></span>
                "Root"
            </button>

            <button
                class="side-item"
                class:active=move || state.mode.get() == Mode::Trash
                title="What has been deleted, and can be put back"
                on:click=move |_| state.show_trash(true)
            >
                <span class="side-dot trash"></span>
                "Trash"
            </button>

            <div class="side-label">"Options"</div>
            <label class="toggle">
                <input
                    type="checkbox"
                    prop:checked=move || state.show_hidden.get()
                    on:change=move |_| state.show_hidden.update(|v| *v = !*v)
                />
                <span>"Show hidden files"</span>
            </label>

            <div class="side-label">"Access"</div>
            <Show
                when=move || fs().writable
                fallback=move || {
                    view! {
                        <p class="side-fine side-readonly" title="The backend decides this, not the UI">
                            <strong>"Read-only: "</strong>
                            <strong>"Read-only. "</strong>
                            {move || {
                                fs()
                                    .readonly_reason
                                    .clone()
                                    .unwrap_or_else(|| "editing is switched off".to_owned())
                            }}
                        </p>
                    }
                }
            >
                <p class="side-fine">
                    "Read-write: files here can be edited, renamed, uploaded and deleted."
                </p>
            </Show>

            <div class="side-label">"Serving"</div>
            <p class="side-note" title=move || fs().root.clone()>{move || fs().root.clone()}</p>
        </aside>
    }
}

#[component]
fn Toolbar() -> impl IntoView {
    let state = expect_context::<FsView>();
    let cwd_protected = move || state.cwd_protected();
    // The root itself is not a folder you can delete, and a protected one is not either.
    let folder_actions =
        move || state.writable() && !state.cwd.get().is_empty() && !cwd_protected();
    let upload_input = NodeRef::<leptos::html::Input>::new();

    let crumbs = move || {
        let Load::Ready(listing) = state.load.get() else {
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

    let summary = move || match state.load.get() {
        Load::Ready(listing) => {
            let hidden = listing.entries.iter().filter(|e| e.hidden).count();
            let shown = listing
                .entries
                .iter()
                .filter(|e| state.show_hidden.get() || !e.hidden)
                .count();
            let mut text = format!("{shown} item{}", if shown == 1 { "" } else { "s" });
            if hidden > 0 && !state.show_hidden.get() {
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
        <div class="files-bar">
            <button
                class="ghost-btn"
                disabled=move || !matches!(state.load.get(), Load::Ready(l) if l.parent.is_some())
                on:click=move |_| {
                    if let Load::Ready(listing) = state.load.get_untracked()
                        && let Some(parent) = listing.parent
                    {
                        state.navigate(parent);
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
                                    matches!(state.load.get(), Load::Ready(l) if l.path == current)
                                }
                                on:click=move |_| state.navigate(target.clone())
                            >
                                {crumb.0.clone()}
                            </button>
                        }
                    }
                </For>
            </nav>

            <span class="count">{summary}</span>

            <Show when=move || folder_actions()>
                <button
                    class="ghost-btn"
                    title="Rename this folder"
                    on:click=move |_| {
                        let current = state
                            .cwd
                            .get_untracked()
                            .rsplit_once('/')
                            .map(|(_, name)| name.to_owned())
                            .unwrap_or_else(|| state.cwd.get_untracked());
                        state.prompt_value.set(current.clone());
                        state.prompt.set(Some(Prompt::Rename {
                            path: state.cwd.get_untracked(),
                            current,
                        }));
                    }
                >
                    "✎ Rename this folder"
                </button>
                <button
                    class="ghost-btn danger"
                    title="Move this folder, and everything in it, to the trash"
                    on:click=move |_| state.delete_current()
                >
                    "🗑 Delete this folder"
                </button>
            </Show>

            <Show when=cwd_protected>
                <span
                    class="preview-meta locked"
                    title="The server keeps this folder read-only; remove it from WEB_DESKTOP_FS_READONLY to change that."
                >
                    "🔒 read-only folder"
                </span>
            </Show>

            <Show when=move || state.writable() && !cwd_protected()>
                <button
                    class="ghost-btn"
                    title="New folder"
                    on:click=move |_| {
                        state.prompt_value.set(String::new());
                        state.prompt.set(Some(Prompt::NewFolder));
                    }
                >
                    "＋ Folder"
                </button>
                <button
                    class="ghost-btn"
                    title="New file"
                    on:click=move |_| {
                        state.prompt_value.set(String::new());
                        state.prompt.set(Some(Prompt::NewFile));
                    }
                >
                    "＋ File"
                </button>
                <button
                    class="ghost-btn"
                    title="Upload files here"
                    on:click=move |_| {
                        if let Some(input) = upload_input.get() {
                            input.click();
                        }
                    }
                >
                    "⭱ Upload"
                </button>
                <input
                    node_ref=upload_input
                    type="file"
                    multiple
                    style="display:none"
                    on:change=move |_| {
                        let Some(input) = upload_input.get() else {
                            return;
                        };
                        let Some(files) = input.files() else {
                            return;
                        };
                        for index in 0..files.length() {
                            if let Some(file) = files.get(index) {
                                state.upload(file);
                            }
                        }
                        input.set_value("");
                    }
                />
            </Show>

            <button
                class="ghost-btn"
                title="Reload this folder"
                on:click=move |_| state.reload()
            >
                "⟳"
            </button>
        </div>

        <Show when=move || state.prompt.get().is_some()>
            <div class="files-prompt">
                <span class="prompt-label">
                    {move || state.prompt.get().map(|p| p.title()).unwrap_or_default()}
                </span>
                <input
                    class="prompt-input"
                    autocomplete="off"
                    prop:value=move || state.prompt_value.get()
                    on:input=move |ev| {
                        state
                            .prompt_value
                            .set(
                                ev
                                    .target()
                                    .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
                                    .map(|el| el.value())
                                    .unwrap_or_default(),
                            )
                    }
                    on:keydown=move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
                        "Enter" => {
                            ev.prevent_default();
                            state.confirm_prompt();
                        }
                        "Escape" => state.prompt.set(None),
                        _ => {}
                    }
                />
                <button class="ghost-btn primary" on:click=move |_| state.confirm_prompt()>
                    "OK"
                </button>
                <button class="ghost-btn" on:click=move |_| state.prompt.set(None)>
                    "Cancel"
                </button>
            </div>
        </Show>

        <Show when=move || state.notice.get().is_some()>
            <div
                class="files-notice"
                class:error=move || state.notice.get().map(|(e, _)| e).unwrap_or(false)
                title="Dismiss"
                on:click=move |_| state.notice.set(None)
            >
                {move || state.notice.get().map(|(_, m)| m).unwrap_or_default()}
            </div>
        </Show>
    }
}

#[component]
fn Listing() -> impl IntoView {
    let state = expect_context::<FsView>();

    let visible = move || match state.load.get() {
        Load::Ready(listing) => listing
            .entries
            .iter()
            .filter(|e| state.show_hidden.get() || !e.hidden)
            .cloned()
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    view! {
        <div class="files-list">
            {move || match state.load.get() {
                Load::Loading => {
                    view! { <div class="files-status muted">"Loading…"</div> }.into_any()
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
                    view! { <div class="files-status muted">"This folder is empty."</div> }.into_any()
                }
                Load::Ready(_) => {
                    view! {
                        <For each=visible key=|e| e.path.clone() let:entry>
                            {
                                let selected_path = entry.path.clone();
                                let icon = icon_for(&entry);
                                let thumb = thumbnail_url(&entry);
                                let size = match entry.kind {
                                    EntryKind::File if !entry.blocked => human_size(entry.size),
                                    _ => String::new(),
                                };
                                let when = entry
                                    .modified
                                    .map(format_short_millis)
                                    .unwrap_or_default();
                                let tooltip = entry.path.clone();
                                let row = entry.clone();
                                let row_for_dbl = entry.clone();
                                let open_app = open_target(&entry);
                                let is_dir = entry.kind == EntryKind::Dir;
                                let blocked = entry.blocked;
                                let name = entry.name.clone();
                                view! {
                                    <button
                                        class="row"
                                        class:dir=is_dir
                                        class:blocked=blocked
                                        class:selected=move || {
                                            state
                                                .selected
                                                .get()
                                                .is_some_and(|s| s.path == selected_path)
                                        }
                                        title=tooltip
                                        on:click=move |_| state.select(row.clone())
                                        on:dblclick=move |_| {
                                            if !is_dir {
                                                state
                                                    .desktop
                                                    .request_open(open_app, row_for_dbl.path.clone());
                                            }
                                        }
                                    >
                                        <span class="row-icon">
                                            {match thumb {
                                                Some(url) => {
                                                    view! {
                                                        <img
                                                            class="row-thumb"
                                                            src=url
                                                            loading="lazy"
                                                            decoding="async"
                                                            alt=""
                                                        />
                                                    }
                                                        .into_any()
                                                }
                                                None => view! { {icon} }.into_any(),
                                            }}
                                        </span>
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
    }
}

#[component]
fn PreviewPane() -> impl IntoView {
    let state = expect_context::<FsView>();

    view! {
        <div class="files-preview">
            {move || {
                let Some(entry) = state.selected.get() else {
                    return view! {
                        <span class="muted">"Select a file to open it."</span>
                    }
                        .into_any();
                };

                let head = preview_head(entry.clone());
                let body = match state.preview.get() {
                    None if entry.kind == EntryKind::File => {
                        view! { <p class="muted">"Loading…"</p> }.into_any()
                    }
                    Some(Err(err)) => view! { <p class="preview-error">{err}</p> }.into_any(),
                    Some(Ok(file)) => match file.body {
                        PreviewBody::Text { content, truncated } => {
                            view! {
                                <Show when=move || truncated>
                                    <p class="preview-note">
                                        "Showing only the first 256 KiB of this file."
                                    </p>
                                </Show>
                                <pre class="preview-text">{content}</pre>
                            }
                                .into_any()
                        }
                        PreviewBody::Image { mime } => {
                            view! {
                                <ImagePreview
                                    path=file.entry.path.clone()
                                    name=file.entry.name.clone()
                                    mime=mime
                                />
                            }
                                .into_any()
                        }
                        PreviewBody::Binary { mime } => {
                            view! {
                                <p class="muted">
                                    "Binary file (" {mime} ", " {human_size(file.entry.size)}
                                    ") — no preview. Use Download to save it."
                                </p>
                            }
                                .into_any()
                        }
                    },
                    None => view! { <p class="muted">"Nothing to show."</p> }.into_any(),
                };

                view! {
                    <div class="preview">
                        {head}
                        {body}
                    </div>
                }
                .into_any()
            }}
        </div>
    }
}

/// Name, size, timestamp, and the write actions for the selected entry.
fn preview_head(entry: FsEntry) -> AnyView {
    let state = expect_context::<FsView>();

    let downloadable = entry.kind == EntryKind::File && !entry.blocked;
    let is_dir = entry.kind == EntryKind::Dir;
    // Protected entries arrive with `readonly` set, which is the server's own answer
    // about whether it would refuse: the UI hides the affordance rather than letting
    // the user discover it through an error.
    let editable = state.writable() && !entry.blocked && !entry.readonly && !entry.path.is_empty();
    let meta = if downloadable {
        human_size(entry.size)
    } else {
        String::new()
    };
    let when = entry.modified.map(format_millis).unwrap_or_default();
    let url = api::download_url(&entry.path);
    let download_name = entry.name.clone();
    let rename_path = entry.path.clone();
    let rename_current = entry.name.clone();
    let delete_path = entry.path.clone();
    let target_path = entry.path.clone();
    // Any regular file can be handed over. The type decides the app: an image goes to
    // the Gallery, everything else to the Text Editor, which says what it is if it
    // turns out to be binary.
    let openable = !entry.blocked && !is_dir && !entry.path.is_empty();
    let target = open_target(&entry);

    let actions_visible = move || editable;
    let rename_visible = move || editable && !is_dir;
    let locked = entry.readonly;

    view! {
        <div class="preview-head">
            <span class="preview-name">{entry.name.clone()}</span>
            <span class="preview-meta">{meta}</span>
            <span class="preview-meta">{when}</span>

            <a
                class="ghost-btn"
                href=url
                download=download_name.clone()
                style=move || if downloadable { "" } else { "display:none" }
            >
                "⭳ Download"
            </a>

            <button
                class="ghost-btn"
                title="Open this file in the app that handles its type"
                style=move || if openable { "" } else { "display:none" }
                on:click=move |_| {
                    state.desktop.request_open(target, target_path.clone());
                }
            >
                {format!("Open in {}", target.name())}
            </button>

            <div class="preview-actions" style=move || {
                if actions_visible() { "" } else { "display:none" }
            }>
                <button
                    class="ghost-btn"
                    title="Rename this file"
                    style=move || if rename_visible() { "" } else { "display:none" }
                    on:click=move |_| {
                        state.prompt_value.set(rename_current.clone());
                        state
                            .prompt
                            .set(
                                Some(Prompt::Rename {
                                    path: rename_path.clone(),
                                    current: rename_current.clone(),
                                }),
                            );
                    }
                >
                    "✎ Rename"
                </button>
                <button
                    class="ghost-btn danger"
                    title="Move this to the trash; it can be put back from there"
                    on:click=move |_| state.delete(delete_path.clone())
                >
                    "🗑 Delete"
                </button>
            </div>

            <span
                class="preview-meta locked"
                title=move || {
                    if locked {
                        "This is inside a folder the server keeps read-only (WEB_DESKTOP_FS_READONLY)."
                    } else {
                        "See the sidebar for why."
                    }
                }
                style=move || if state.writable() && !locked { "display:none" } else { "" }
            >
                {if locked { "🔒 read-only" } else { "read-only" }}
            </span>
        </div>
    }
    .into_any()
}

/// Which built-in app should open a file, from its name alone.
///
/// `looks_like_image` rather than `image_mime` on purpose: a TIFF is still a picture,
/// and the Gallery is the app that can explain why the browser will not draw it.
fn open_target(entry: &FsEntry) -> shared::AppId {
    if looks_like_image(&entry.name) {
        shared::AppId::Gallery
    } else {
        shared::AppId::Editor
    }
}

fn icon_for(entry: &FsEntry) -> &'static str {
    match (entry.blocked, entry.kind) {
        (true, _) => "⛔",
        (_, EntryKind::Dir) => "📁",
        // Including images too large to thumbnail, and formats the browser cannot
        // decode: the preview pane explains those.
        (_, EntryKind::File) if looks_like_image(&entry.name) => "🖼",
        (_, EntryKind::File) if entry.hidden => "🗒",
        (_, EntryKind::File) => "📄",
        _ => "◦",
    }
}

/// URL for a lazy row thumbnail, or `None` to keep the plain icon.
fn thumbnail_url(entry: &FsEntry) -> Option<String> {
    if entry.kind != EntryKind::File || entry.blocked || entry.size > THUMBNAIL_MAX_BYTES {
        return None;
    }
    image_mime(&entry.name).map(|_| api::raw_url(&entry.path))
}

/// An image from [`api::raw_url`], plus the size the browser decoded it at and an
/// honest message when it cannot be decoded at all.
#[component]
fn ImagePreview(path: String, name: String, mime: String) -> impl IntoView {
    let src = api::raw_url(&path);
    let href = src.clone();
    let dims = RwSignal::new(None::<(u32, u32)>);
    let failed = RwSignal::new(false);

    let on_load = move |ev: web_sys::Event| {
        if let Some(img) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlImageElement>().ok())
        {
            dims.set(Some((img.natural_width(), img.natural_height())));
        }
    };

    view! {
        <div class="image-preview">
            <a
                class="image-frame"
                href=href
                target="_blank"
                rel="noopener"
                title="Open in a new tab"
                style=move || if failed.get() { "display:none" } else { "" }
            >
                <img
                    class="preview-image"
                    src=src
                    alt=""
                    on:load=on_load
                    on:error=move |_| failed.set(true)
                />
            </a>
            <p class="preview-error" style=move || if failed.get() { "" } else { "display:none" }>
                "The browser could not decode this file as " {mime}
                " — it may be misnamed or truncated."
            </p>
            <p class="preview-dims" style=move || if failed.get() { "display:none" } else { "" }>
                {move || match dims.get() {
                    Some((width, height)) => format!("{width} × {height} px · {name}"),
                    None => "decoding image…".to_owned(),
                }}
            </p>
        </div>
    }
}
