//! The Files app: a real directory on the machine running the backend.
//!
//! Nothing here touches a disk. Every listing, preview and edit is a request to
//! `/api/fs/*`, which is fenced inside one root directory on the server. Text and
//! binary previews arrive as JSON; images are rendered by pointing an `<img>` at
//! `/api/fs/raw`. Editing is offered only when the server says the root is
//! writable, and the reason it is not is shown rather than a dead button.
//!
//! All of a window's state lives in [`FsView`], a `Copy` handle provided to this
//! component's subtree, so the toolbar, the listing and the preview pane reach the
//! same signals without threading closures through props. Each Files window creates
//! its own, so two of them do not share a directory, a selection or an editor.

use leptos::prelude::*;
use shared::fs::{
    DirListing, EntryKind, FilePreview, FsEntry, PreviewBody, image_mime, looks_like_image,
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
    /// Editor buffers: what is in the box, and what is on disk.
    draft: RwSignal<String>,
    original: RwSignal<String>,
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

    fn navigate(self, path: String) {
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
        self.draft.set(String::new());
        self.original.set(String::new());
        let mine = self.seq.get_untracked();
        let path = entry.path.clone();
        spawn_local(async move {
            let result = api::preview_file(&path).await;
            if self.seq.get_untracked() != mine {
                return;
            }
            if let Ok(file) = &result
                && let PreviewBody::Text { content, .. } = &file.body
            {
                self.draft.set(content.clone());
                self.original.set(content.clone());
            }
            self.preview.set(Some(result));
        });
    }

    fn save(self) {
        let Some(entry) = self.selected.get_untracked() else {
            return;
        };
        if !self.writable() {
            self.warn("this file browser is read-only");
            return;
        }
        let text = self.draft.get_untracked();
        if text.len() as u64 > self.desktop.fs.get().max_write_bytes {
            self.warn(format!(
                "too large to save (limit {})",
                human_size(self.desktop.fs.get().max_write_bytes)
            ));
            return;
        }
        // The editor is clean as soon as the write is requested; a failure replaces
        // this with a warning, and the buffer is untouched either way.
        self.original.set(text.clone());
        let path = entry.path.clone();
        self.run(format!("Saved {}", path), async move {
            api::write_file(&path, text.as_bytes()).await.map(|_| ())
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

    fn delete(self, path: String, recursive: bool) {
        self.run(format!("Deleted {path}"), async move {
            api::delete_entry(&path, recursive).await
        });
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
        draft: RwSignal::new(String::new()),
        original: RwSignal::new(String::new()),
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
            state.draft.set(String::new());
            state.original.set(String::new());
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
                    <Toolbar />
                    <Listing />
                    <PreviewPane />
                </div>
            </Show>
        </div>
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
                        <p class="side-fine side-readonly">
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

            <Show when=move || state.writable()>
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
                                            state
                                                .selected
                                                .get()
                                                .is_some_and(|s| s.path == selected_path)
                                        }
                                        title=tooltip
                                        on:click=move |_| state.select(target.clone())
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
                        PreviewBody::Text { truncated, .. } => {
                            view! { <Editor truncated=truncated /> }.into_any()
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
    let editable = state.writable() && !entry.blocked && !entry.path.is_empty();
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
    let editor_path = entry.path.clone();
    // Any regular file can be handed over; the editor decides whether it is text.
    let editable_file = !entry.blocked && !is_dir && !entry.path.is_empty();

    let actions_visible = move || editable;
    let rename_visible = move || editable && !is_dir;
    let deleting = move || state.confirming.get();

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
                title="Open in the Text Editor"
                style=move || if editable_file { "" } else { "display:none" }
                on:click=move |_| {
                    state.desktop.pending_editor_file.set(Some(editor_path.clone()));
                    state.desktop.open_builtin(shared::AppId::Editor);
                }
            >
                "✎ Edit"
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
                    class:confirming=deleting
                    on:click=move |_| {
                        if state.confirming.get_untracked() {
                            state.confirming.set(false);
                            state.delete(delete_path.clone(), is_dir);
                        } else {
                            state.confirming.set(true);
                        }
                    }
                >
                    {move || {
                        if !deleting() {
                            "🗑 Delete"
                        } else if is_dir {
                            "Really delete folder and contents?"
                        } else {
                            "Really delete?"
                        }
                    }}
                </button>
            </div>

            <span
                class="preview-meta"
                title="See the sidebar for why"
                style=move || if state.writable() { "display:none" } else { "" }
            >
                "read-only"
            </span>
        </div>
    }
    .into_any()
}

/// The text editor: a box bound to the draft, with save, revert and a dirty mark.
#[component]
fn Editor(truncated: bool) -> impl IntoView {
    let state = expect_context::<FsView>();

    let dirty = move || state.draft.get() != state.original.get();
    let too_big = move || state.draft.get().len() as u64 > state.desktop.fs.get().max_write_bytes;

    let save = move || {
        if too_big() {
            state.warn("this file is too large to save");
            return;
        }
        state.save();
    };

    view! {
        <div class="files-editor">
            <div class="editor-bar">
                <Show when=move || truncated>
                    <span class="preview-note">
                        "Only the first 256 KiB was loaded — saving would drop the rest."
                    </span>
                </Show>
                <Show when=move || dirty() && !truncated>
                    <span class="editor-dirty" title="Unsaved changes">"● unsaved"</span>
                </Show>
                <span class="editor-spacer"></span>
                <Show when=move || state.writable()>
                    <button
                        class="ghost-btn"
                        disabled=move || !dirty()
                        on:click=move |_| state.draft.set(state.original.get_untracked())
                    >
                        "Revert"
                    </button>
                    <button
                        class="ghost-btn primary"
                        disabled=move || !dirty() || truncated
                        title="Save (Ctrl+S)"
                        on:click=move |_| save()
                    >
                        "Save"
                    </button>
                </Show>
                <Show when=move || !state.writable()>
                    <span class="preview-meta">"read-only"</span>
                </Show>
            </div>
            <textarea
                class="editor-box"
                spellcheck="false"
                prop:value=move || state.draft.get()
                readonly=move || !state.writable()
                on:input=move |ev| {
                    state
                        .draft
                        .set(
                            ev
                                .target()
                                .and_then(|t| t.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                .map(|el| el.value())
                                .unwrap_or_default(),
                        )
                }
                on:keydown=move |ev: web_sys::KeyboardEvent| {
                    if ev.key().eq_ignore_ascii_case("s") && (ev.ctrl_key() || ev.meta_key()) {
                        ev.prevent_default();
                        save();
                    }
                }
            ></textarea>
        </div>
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
