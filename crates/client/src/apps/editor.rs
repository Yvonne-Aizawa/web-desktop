//! The Text Editor: several files open at once, in one window.
//!
//! It is a compiled-in app, so it shares the shell's wasm instance and can use the
//! same `api::*` calls the Files app does. What it adds over the Files preview pane
//! is *several* buffers: a tab per file, each with its own draft and dirty state, a
//! save-all, and a line-number gutter with a caret readout.
//!
//! # Keeping the caret
//!
//! A `<textarea>` whose `value` is bound reactively gets rewritten on every
//! keystroke, and browsers put the caret at the end when that happens — which is
//! unusable for an editor. So the box is *uncontrolled*: it is seeded once when a
//! tab becomes active, and edits flow one way, into the tab's draft. The body of the
//! window is rendered from a [`Memo`] of `(path, name, kind)`, which does not change
//! while typing, so the element is never rebuilt underneath the caret.

use leptos::prelude::*;
use shared::fs::{DirListing, EntryKind, FsEntry, PreviewBody, looks_like_image};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use crate::api;
use crate::format::human_size;
use crate::state::Desktop;

/// What a tab can hold: editable text, or a reason it is not.
#[derive(Clone, PartialEq)]
enum Body {
    Text { truncated: bool },
    Unsupported(String),
}

#[derive(Clone, PartialEq)]
struct Tab {
    path: String,
    name: String,
    body: Body,
    /// What is in the box, and what is on disk.
    draft: String,
    original: String,
    /// Waiting for a second click on the close button, because it is dirty.
    closing: bool,
}

impl Tab {
    fn dirty(&self) -> bool {
        self.body == Body::Text { truncated: false } && self.draft != self.original
    }
}

/// Everything one editor window is and can do.
#[derive(Clone, Copy)]
struct Ed {
    desktop: Desktop,
    /// Sidebar: which directory is listed, and what is in it.
    cwd: RwSignal<String>,
    entries: RwSignal<Option<Result<Vec<FsEntry>, String>>>,
    show_hidden: RwSignal<bool>,
    /// Sidebar request guard, and the reload button.
    seq: RwSignal<u64>,
    reload: RwSignal<u32>,
    tabs: RwSignal<Vec<Tab>>,
    /// Path of the tab on screen.
    active: RwSignal<Option<String>>,
    caret: RwSignal<(usize, usize)>,
    /// The inline "new file" prompt.
    prompt: RwSignal<bool>,
    prompt_value: RwSignal<String>,
    notice: RwSignal<Option<(bool, String)>>,
    busy: RwSignal<bool>,
}

impl Ed {
    fn writable(self) -> bool {
        self.desktop.fs.get().writable
    }

    fn say(self, message: impl Into<String>) {
        self.notice.set(Some((false, message.into())));
    }

    fn warn(self, message: impl Into<String>) {
        self.notice.set(Some((true, message.into())));
    }

    fn tab(self, path: &str) -> Option<Tab> {
        self.tabs
            .with(|tabs| tabs.iter().find(|t| t.path == path).cloned())
    }

    fn update_tab(self, path: &str, change: impl FnOnce(&mut Tab)) {
        self.tabs.update(|tabs| {
            if let Some(tab) = tabs.iter_mut().find(|t| t.path == path) {
                change(tab);
            }
        });
    }

    /// The draft of a tab, without subscribing.
    fn draft(self, path: &str) -> String {
        self.tabs
            .with_untracked(|tabs| {
                tabs.iter()
                    .find(|t| t.path == path)
                    .map(|t| t.draft.clone())
            })
            .unwrap_or_default()
    }

    fn navigate(self, path: String) {
        self.cwd.set(path);
    }

    /// Open a path the shell handed over, without an `FsEntry` to go with it.
    fn open_path(self, path: String) {
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        self.open(FsEntry {
            name,
            path,
            kind: EntryKind::File,
            size: 0,
            modified: None,
            hidden: false,
            readonly: false,
            blocked: false,
        });
    }

    /// Open a file in a tab, or raise the tab that already has it.
    fn open(self, entry: FsEntry) {
        if matches!(entry.kind, EntryKind::Dir) {
            self.navigate(entry.path.clone());
            return;
        }
        if let Some(existing) = self.tabs.with_untracked(|tabs| {
            tabs.iter()
                .find(|t| t.path == entry.path)
                .map(|t| t.path.clone())
        }) {
            self.active.set(Some(existing));
            return;
        }

        let path = entry.path.clone();
        let name = entry.name.clone();
        // Reserve the tab immediately so a slow read cannot open it twice.
        self.tabs.update(|tabs| {
            tabs.push(Tab {
                path: path.clone(),
                name,
                body: Body::Unsupported("reading…".to_owned()),
                draft: String::new(),
                original: String::new(),
                closing: false,
            })
        });
        self.active.set(Some(path.clone()));

        // No request guard here on purpose: the tab is reserved above, and
        // `update_tab` looks it up by path, so a reply that arrives after the tab was
        // closed simply finds nothing. Sharing the sidebar's counter instead would let
        // an unrelated listing cancel this read.
        spawn_local(async move {
            let result = api::preview_file(&path).await;
            match result {
                Ok(file) => {
                    let body = match file.body {
                        PreviewBody::Text { content, truncated } => {
                            self.update_tab(&path, |tab| {
                                tab.draft = content.clone();
                                tab.original = content;
                                tab.body = Body::Text { truncated };
                            });
                            return;
                        }
                        PreviewBody::Image { mime } => Body::Unsupported(format!(
                            "an image ({mime}) — open it in Files to look at it"
                        )),
                        PreviewBody::Binary { mime } => {
                            Body::Unsupported(format!("binary ({mime})"))
                        }
                    };
                    self.update_tab(&path, |tab| tab.body = body);
                }
                Err(err) => {
                    self.update_tab(&path, |tab| tab.body = Body::Unsupported(err));
                }
            }
        });
    }

    /// Close a tab, asking once first if it has unsaved changes.
    fn close(self, path: String) {
        let dirty = self.tab(&path).map(|tab| tab.dirty()).unwrap_or(false);
        let closing = self.tab(&path).map(|tab| tab.closing).unwrap_or(false);

        if dirty && !closing {
            self.update_tab(&path, |tab| tab.closing = true);
            self.warn("that file has unsaved changes — click ✕ again to discard them");
            return;
        }

        self.tabs.update(|tabs| tabs.retain(|t| t.path != path));
        if self.active.get_untracked().as_deref() == Some(path.as_str()) {
            let next = self
                .tabs
                .with_untracked(|tabs| tabs.last().map(|t| t.path.clone()));
            self.active.set(next);
        }
    }

    fn activate(self, path: String) {
        self.active.set(Some(path));
        self.caret.set((1, 1));
    }

    /// Save one tab.
    fn save(self, path: String) {
        let Some(tab) = self.tab(&path) else {
            return;
        };
        if !matches!(tab.body, Body::Text { truncated: false }) {
            self.warn("this tab cannot be saved");
            return;
        }
        if !self.writable() {
            self.warn("this file browser is read-only");
            return;
        }
        if tab.draft.len() as u64 > self.desktop.fs.get().max_write_bytes {
            self.warn(format!(
                "too large to save (limit {})",
                human_size(self.desktop.fs.get().max_write_bytes)
            ));
            return;
        }
        if !tab.dirty() {
            self.say("nothing to save");
            return;
        }

        let text = tab.draft.clone();
        self.busy.set(true);
        self.update_tab(&path, |tab| tab.original = text.clone());
        spawn_local(async move {
            match api::write_file(&path, text.as_bytes()).await {
                Ok(_) => self.say(format!("Saved {path}")),
                Err(err) => self.warn(err),
            }
            self.busy.set(false);
        });
    }

    fn save_all(self) {
        let dirty: Vec<String> = self.tabs.with_untracked(|tabs| {
            tabs.iter()
                .filter(|t| t.dirty())
                .map(|t| t.path.clone())
                .collect()
        });
        if dirty.is_empty() {
            self.say("nothing to save");
            return;
        }
        let count = dirty.len();
        self.busy.set(true);
        self.notice.set(None);
        let writable = self.writable();
        let limit = self.desktop.fs.get().max_write_bytes;
        spawn_local(async move {
            if !writable {
                self.warn("this file browser is read-only");
                self.busy.set(false);
                return;
            }
            let mut saved = 0;
            let mut failure = None;
            for path in dirty {
                let Some(tab) = self.tab(&path) else { continue };
                if tab.draft.len() as u64 > limit {
                    failure = Some(format!("{path} is too large to save"));
                    continue;
                }
                match api::write_file(&path, tab.draft.as_bytes()).await {
                    Ok(_) => {
                        let text = tab.draft.clone();
                        self.update_tab(&path, |tab| tab.original = text);
                        saved += 1;
                    }
                    Err(err) => failure = Some(err),
                }
            }
            self.busy.set(false);
            match failure {
                Some(err) => self.warn(err),
                None => self.say(format!("Saved {saved} of {count} file(s)")),
            }
        });
    }

    fn create_file(self, name: String) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            self.warn("a name is required");
            return;
        }
        let parent = self.cwd.get_untracked();
        let path = if parent.is_empty() {
            name.clone()
        } else {
            format!("{parent}/{name}")
        };
        self.prompt.set(false);
        self.busy.set(true);
        self.notice.set(None);
        spawn_local(async move {
            match api::write_file(&path, b"").await {
                Ok(entry) => {
                    self.busy.set(false);
                    self.say(format!("Created {name}"));
                    self.open(entry);
                }
                Err(err) => {
                    self.busy.set(false);
                    self.warn(err);
                }
            }
        });
    }
}

#[component]
pub fn Editor() -> impl IntoView {
    let desktop = expect_context::<Desktop>();

    let state = Ed {
        desktop,
        cwd: RwSignal::new(String::new()),
        entries: RwSignal::new(None),
        show_hidden: RwSignal::new(false),
        seq: RwSignal::new(0),
        reload: RwSignal::new(0),
        tabs: RwSignal::new(Vec::new()),
        active: RwSignal::new(None),
        caret: RwSignal::new((1, 1)),
        prompt: RwSignal::new(false),
        prompt_value: RwSignal::new(String::new()),
        notice: RwSignal::new(None),
        busy: RwSignal::new(false),
    };
    provide_context(state);

    // Take a file handed over by another app. Reading the signal means this works
    // whether the editor was just opened or was already on screen, and asking for
    // requests addressed to *this* app leaves another app's request for it to take.
    Effect::new(move |_| {
        if let Some(path) = state.desktop.take_open_request(shared::AppId::Editor) {
            state.open_path(path);
        }
    });

    // List the sidebar directory whenever it changes.
    Effect::new(move |_| {
        let path = state.cwd.get();
        state.show_hidden.track();
        state.reload.track();
        let mine = state.seq.get_untracked() + 1;
        state.seq.set(mine);
        state.entries.set(None);
        spawn_local(async move {
            let result = api::list_dir(&path).await;
            if state.seq.get_untracked() != mine {
                return;
            }
            state.entries.set(Some(result.map(|listing: DirListing| {
                // The server canonicalizes; adopting its path keeps the breadcrumb
                // honest when a folder is reached through a symlink.
                if listing.path != path {
                    state.cwd.set(listing.path.clone());
                }
                listing.entries
            })));
        });
    });

    view! {
        <div class="app editor">
            <Show
                when=move || desktop.fs.get().enabled
                fallback=move || {
                    view! {
                        <div class="files-status error">
                            <strong>"The editor needs the file browser."</strong>
                            <span>
                                "It is switched off on this backend; start the server with `WEB_DESKTOP_ROOT=/some/folder`."
                            </span>
                        </div>
                    }
                }
            >
                <Browser />
                <div class="editor-main">
                    <Actions />
                    <TabBar />
                    <Body />
                    <StatusBar />
                </div>
            </Show>
        </div>
    }
}

/// The file browser on the left: pick a file here to open it in a tab.
#[component]
fn Browser() -> impl IntoView {
    let state = expect_context::<Ed>();

    let rows = move || match state.entries.get() {
        None => Vec::new(),
        Some(Err(_)) => Vec::new(),
        Some(Ok(entries)) => entries
            .into_iter()
            .filter(|e| state.show_hidden.get() || !e.hidden)
            .collect::<Vec<_>>(),
    };

    view! {
        <aside class="files-side editor-side">
            <div class="side-label">"Files"</div>
            <div class="editor-cwd">
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
                    "↑"
                </button>
                <span class="editor-path" title=move || format!("/{}", state.cwd.get())>
                    {move || format!("/{}", state.cwd.get())}
                </span>
                <button
                    class="ghost-btn"
                    title="Reload this folder"
                    on:click=move |_| state.reload.update(|n| *n += 1)
                >
                    "⟳"
                </button>
            </div>

            <div class="side-label">"Show"</div>
            <label class="toggle">
                <input
                    type="checkbox"
                    prop:checked=move || state.show_hidden.get()
                    on:change=move |_| state.show_hidden.update(|v| *v = !*v)
                />
                <span>"Hidden files"</span>
            </label>

            <div class="editor-tree">
                {move || match state.entries.get() {
                    None => view! { <p class="side-fine">"Loading…"</p> }.into_any(),
                    Some(Err(err)) => {
                        view! { <p class="side-fine side-readonly">{err}</p> }.into_any()
                    }
                    Some(Ok(_)) if rows().is_empty() => {
                        view! { <p class="side-fine">"This folder is empty."</p> }.into_any()
                    }
                    Some(Ok(_)) => {
                        view! {
                            <For each=rows key=|e| e.path.clone() let:entry>
                                {
                                    let target = entry.clone();
                                    let name = entry.name.clone();
                                    let icon = if entry.kind == EntryKind::Dir {
                                        "📁"
                                    } else if looks_like_image(&entry.name) {
                                        "🖼"
                                    } else {
                                        "📄"
                                    };
                                    let is_dir = entry.kind == EntryKind::Dir;
                                    let open_path = entry.path.clone();
                                    let blocked = entry.blocked;
                                    view! {
                                        <button
                                            class="editor-node"
                                            class:dir=is_dir
                                            class:open=move || {
                                                state.active.get().as_deref() == Some(open_path.as_str())
                                            }
                                            disabled=blocked
                                            title=target.path.clone()
                                            on:click=move |_| state.open(target.clone())
                                        >
                                            <span class="editor-node-icon">{icon}</span>
                                            <span class="editor-node-name">{name}</span>
                                        </button>
                                    }
                                }
                            </For>
                        }
                            .into_any()
                    }
                }}
            </div>

            <p class="side-fine editor-hint">
                "Click a file to open it in a tab. A folder is shown here, not opened."
            </p>
        </aside>
    }
}

/// Save, save all, new file, and whatever just happened.
#[component]
fn Actions() -> impl IntoView {
    let state = expect_context::<Ed>();

    let any_dirty = move || state.tabs.with(|tabs| tabs.iter().any(|t| t.dirty()));

    view! {
        <div class="editor-bar">
            <Show when=move || state.writable()>
                <button
                    class="ghost-btn"
                    title="New file in this folder"
                    on:click=move |_| {
                        state.prompt_value.set(String::new());
                        state.prompt.set(true);
                    }
                >
                    "＋ New"
                </button>
                <button
                    class="ghost-btn primary"
                    disabled=move || {
                        !state
                            .active
                            .get()
                            .and_then(|path| state.tab(&path))
                            .map(|tab| tab.dirty())
                            .unwrap_or(false)
                    }
                    title="Save (Ctrl+S)"
                    on:click=move |_| {
                        if let Some(path) = state.active.get_untracked() {
                            state.save(path);
                        }
                    }
                >
                    "Save"
                </button>
                <button
                    class="ghost-btn"
                    disabled=move || !any_dirty()
                    title="Save every changed tab"
                    on:click=move |_| state.save_all()
                >
                    "Save all"
                </button>
            </Show>
            <Show when=move || !state.writable()>
                <span class="preview-meta">
                    "read-only: " {move || {
                        state
                            .desktop
                            .fs
                            .get()
                            .readonly_reason
                            .clone()
                            .unwrap_or_default()
                    }}
                </span>
            </Show>
            <span class="editor-spacer"></span>
            <Show when=move || state.busy.get()>
                <span class="preview-meta">"working…"</span>
            </Show>
            <Show when=move || state.notice.get().is_some()>
                <span
                    class="editor-notice"
                    class:error=move || state.notice.get().map(|(e, _)| e).unwrap_or(false)
                    title="Dismiss"
                    on:click=move |_| state.notice.set(None)
                >
                    {move || state.notice.get().map(|(_, m)| m).unwrap_or_default()}
                </span>
            </Show>
        </div>

        <Show when=move || state.prompt.get()>
            <div class="files-prompt">
                <span class="prompt-label">"New file:"</span>
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
                            let name = state.prompt_value.get_untracked();
                            state.create_file(name);
                        }
                        "Escape" => state.prompt.set(false),
                        _ => {}
                    }
                />
                <button
                    class="ghost-btn primary"
                    on:click=move |_| {
                        let name = state.prompt_value.get_untracked();
                        state.create_file(name);
                    }
                >
                    "Create"
                </button>
                <button class="ghost-btn" on:click=move |_| state.prompt.set(false)>
                    "Cancel"
                </button>
            </div>
        </Show>
    }
}

#[component]
fn TabBar() -> impl IntoView {
    let state = expect_context::<Ed>();

    view! {
        <div class="editor-tabs">
            <For each=move || state.tabs.get() key=|t| t.path.clone() let:tab>
                {
                    // A keyed row is *not* re-rendered when its item changes, so every
                    // value that can change (dirty, closing, kind) has to be read from
                    // the signal. The name is fixed once the tab is open.
                    let name = tab.name.clone();
                    let title = tab.path.clone();
                    let activate = tab.path.clone();
                    let close = tab.path.clone();
                    let class_path = tab.path.clone();
                    let kind_path = tab.path.clone();
                    let confirm_path = tab.path.clone();
                    let label_path = tab.path.clone();
                    let active_path = tab.path.clone();
                    view! {
                        <div
                            class="editor-tab"
                            class:active=move || {
                                state.active.get().as_deref() == Some(active_path.as_str())
                            }
                            class:dirty=move || {
                                state.tab(&class_path).map(|t| t.dirty()).unwrap_or(false)
                            }
                            class:unsupported=move || {
                                state
                                    .tab(&kind_path)
                                    .map(|t| matches!(t.body, Body::Unsupported(_)))
                                    .unwrap_or(false)
                            }
                        >
                            <button
                                class="tab-name"
                                title=title
                                on:click=move |_| state.activate(activate.clone())
                            >
                                {name}
                            </button>
                            <button
                                class="tab-close"
                                class:confirming=move || {
                                    state.tab(&confirm_path).map(|t| t.closing).unwrap_or(false)
                                }
                                title="Close"
                                on:click=move |_| state.close(close.clone())
                            >
                                {move || {
                                    let closing = state
                                        .tab(&label_path)
                                        .map(|t| t.closing)
                                        .unwrap_or(false);
                                    let dirty = state
                                        .tab(&label_path)
                                        .map(|t| t.dirty())
                                        .unwrap_or(false);
                                    if dirty && closing { "discard?" } else { "✕" }
                                }}
                            </button>
                        </div>
                    }
                }
            </For>
            <Show when=move || state.tabs.get().is_empty()>
                <span class="editor-tab-empty">"No file open — pick one on the left."</span>
            </Show>
        </div>
    }
}

/// The editing area: gutter, box, and the status readouts under it.
#[component]
fn Body() -> impl IntoView {
    let state = expect_context::<Ed>();
    let area = NodeRef::<leptos::html::Textarea>::new();
    let gutter = NodeRef::<leptos::html::Div>::new();

    // Only the identity of the open file, so typing never rebuilds the box.
    let shown = Memo::new(move |_| {
        let path = state.active.get()?;
        let tab = state.tab(&path)?;
        Some((tab.path, tab.name, tab.body))
    });

    view! {
        <div class="editor-body">
            {move || match shown.get() {
                None => {
                    view! {
                        <div class="files-status muted">
                            "Open a file from the left to start editing."
                        </div>
                    }
                        .into_any()
                }
                Some((path, name, Body::Unsupported(reason))) => {
                    let download = api::download_url(&path);
                    view! {
                        <div class="files-status muted">
                            <strong>{name}</strong>
                            <span>"This file is " {reason} "."</span>
                            <a class="ghost-btn" href=download download=path.clone()>
                                "⭳ Download"
                            </a>
                        </div>
                    }
                        .into_any()
                }
                Some((path, _name, Body::Text { truncated })) => {
                    // Seeded once per activation from the tab's draft; from here the
                    // box owns its text and reports changes back.
                    let seed = state.draft(&path);
                    let editing = path.clone();
                    let caret_path = path.clone();
                    let save_path = path.clone();
                    view! {
                        <div class="editor-sheet">
                            <div class="editor-gutter" node_ref=gutter>
                                {move || {
                                    // Tracked on purpose: the numbers have to grow as
                                    // lines are added. This is a sibling of the box,
                                    // so re-rendering it never touches the caret.
                                    let lines = state
                                        .tabs
                                        .with(|tabs| {
                                            tabs.iter()
                                                .find(|t| t.path == caret_path)
                                                .map(|t| t.draft.split('\n').count())
                                                .unwrap_or(1)
                                        })
                                        .max(1);
                                    (1..=lines)
                                        .map(|n| n.to_string())
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                }}
                            </div>
                            <textarea
                                node_ref=area
                                class="editor-textarea"
                                class:truncated=truncated
                                spellcheck="false"
                                autocomplete="off"
                                readonly=move || !state.writable() || truncated
                                prop:value=seed
                                on:input=move |ev| {
                                    let Some(el) = ev
                                        .target()
                                        .and_then(|t| t.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                    else {
                                        return;
                                    };
                                    let text = el.value();
                                    state.update_tab(&editing, |tab| tab.draft = text.clone());
                                    state.caret.set(caret_position(&text, caret_offset(&el)));
                                }
                                on:keyup=move |ev: web_sys::KeyboardEvent| {
                                    if let Some(el) = ev
                                        .target()
                                        .and_then(|t| t.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                    {
                                        state.caret.set(caret_position(&el.value(), caret_offset(&el)));
                                    }
                                }
                                on:click=move |ev: web_sys::MouseEvent| {
                                    if let Some(el) = ev
                                        .target()
                                        .and_then(|t| t.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                                    {
                                        state.caret.set(caret_position(&el.value(), caret_offset(&el)));
                                    }
                                }
                                on:scroll=move |_| {
                                    // Keep the numbers beside their lines.
                                    if let (Some(area), Some(gutter)) = (area.get(), gutter.get()) {
                                        gutter.set_scroll_top(area.scroll_top());
                                    }
                                }
                                on:keydown=move |ev: web_sys::KeyboardEvent| {
                                    if ev.key().eq_ignore_ascii_case("s")
                                        && (ev.ctrl_key() || ev.meta_key())
                                    {
                                        ev.prevent_default();
                                        state.save(save_path.clone());
                                    }
                                }
                            ></textarea>
                        </div>
                        <Show when=move || truncated>
                            <p class="preview-note">
                                "Only the first 256 KiB was loaded, so this tab is read-only — saving would drop the rest."
                            </p>
                        </Show>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

#[component]
fn StatusBar() -> impl IntoView {
    let state = expect_context::<Ed>();

    let active = move || {
        state
            .active
            .get()
            .map(|path| format!("/{path}"))
            .unwrap_or_else(|| "no file".to_owned())
    };
    let size = move || {
        state
            .active
            .get()
            .and_then(|path| state.tab(&path))
            .map(|tab| match &tab.body {
                Body::Text { .. } => human_size(tab.draft.len() as u64),
                Body::Unsupported(_) => "—".to_owned(),
            })
            .unwrap_or_default()
    };
    let dirty = move || {
        state
            .active
            .get()
            .and_then(|path| state.tab(&path))
            .map(|tab| tab.dirty())
            .unwrap_or(false)
    };
    let totals = move || {
        state.tabs.with(|tabs| {
            let open = tabs.len();
            let changed = tabs.iter().filter(|t| t.dirty()).count();
            match (open, changed) {
                (0, _) => String::new(),
                (n, 0) => format!("{n} open"),
                (n, c) => format!("{n} open · {c} unsaved"),
            }
        })
    };

    view! {
        <div class="editor-status">
            <span class="status-path" title=active>{active}</span>
            <span class="editor-spacer"></span>
            <span>{totals}</span>
            <span>{move || {
                let (line, column) = state.caret.get();
                format!("Ln {line}, Col {column}")
            }}</span>
            <span>{size}</span>
            <span class=move || if dirty() { "status-dirty" } else { "status-saved" }>
                {move || if dirty() { "unsaved" } else { "saved" }}
            </span>
        </div>
    }
}

/// Where the caret is in a textarea, in the UTF-16 units the DOM reports. Zero when
/// the browser declines to say, which only costs a stale readout.
fn caret_offset(area: &web_sys::HtmlTextAreaElement) -> usize {
    area.selection_start().ok().flatten().unwrap_or(0) as usize
}

/// `(line, column)` for a caret offset, counted in the UTF-16 units the DOM reports.
///
/// Walking the string rather than slicing it keeps this safe for text that is not
/// ASCII: slicing at a code-unit offset could land inside a character.
fn caret_position(text: &str, offset: usize) -> (usize, usize) {
    let mut units = 0;
    let mut line = 1;
    let mut column = 1;
    for character in text.chars() {
        if units >= offset {
            break;
        }
        units += character.len_utf16();
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}
