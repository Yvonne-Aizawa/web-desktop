//! A read-only virtual filesystem browser: places sidebar, table of entries,
//! and a preview pane.

use leptos::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
}

#[derive(Clone, Copy, PartialEq)]
struct Entry {
    name: &'static str,
    kind: Kind,
    size: &'static str,
    body: &'static str,
}

const fn dir(name: &'static str, size: &'static str) -> Entry {
    Entry {
        name,
        kind: Kind::Dir,
        size,
        body: "",
    }
}

const fn file(name: &'static str, size: &'static str, body: &'static str) -> Entry {
    Entry {
        name,
        kind: Kind::File,
        size,
        body,
    }
}

const PLACES: [(&str, &str); 4] = [
    ("/", "Computer"),
    ("/home/guest", "Home"),
    ("/etc", "Config"),
    ("/var/log", "Logs"),
];

fn read_dir(path: &str) -> Vec<Entry> {
    match path {
        "/" => vec![
            dir("home", "—"),
            dir("etc", "—"),
            dir("var", "—"),
            file(
                "README.txt",
                "412 B",
                "A desktop written in Rust.\n\nServer: axum\nClient: leptos + wasm-bindgen\nShared: crate `shared` with serde types.",
            ),
        ],
        "/home" => vec![dir("guest", "—")],
        "/home/guest" => vec![
            file(
                "notes.md",
                "1.2 kB",
                "# notes\n\n- window geometry lives in RwSignal<WinData>\n- pointer capture drives drag and resize\n- the taskbar reads the same window list",
            ),
            file(
                "todo.txt",
                "96 B",
                "[x] draggable windows\n[x] resizable windows\n[x] taskbar + start menu\n[ ] persist layout to the backend",
            ),
            dir("projects", "—"),
        ],
        "/home/guest/projects" => vec![
            dir("web_desktop", "—"),
            file(
                "scratch.rs",
                "238 B",
                "fn main() {\n    println!(\"hello from wasm\");\n}",
            ),
        ],
        "/home/guest/projects/web_desktop" => vec![
            file(
                "Cargo.toml",
                "612 B",
                "[workspace]\nmembers = [\"crates/shared\", \"crates/server\", \"crates/client\"]\nresolver = \"3\"",
            ),
            file(
                "README.md",
                "1.4 kB",
                "# web_desktop\n\nRun `cargo run -p server` and `cd crates/client && trunk serve`.",
            ),
        ],
        "/etc" => vec![
            file("hostname", "12 B", "web-desktop"),
            file(
                "motd",
                "88 B",
                "Welcome to web-desktop.\nType `help` in the terminal.",
            ),
            file(
                "os-release",
                "156 B",
                "NAME=WebDesktop\nVERSION=0.1.0\nID=web-desktop\nHOME_URL=https://example.invalid",
            ),
        ],
        "/var" => vec![dir("log", "—")],
        "/var/log" => vec![
            file(
                "server.log",
                "2.1 kB",
                "[info] listening on 127.0.0.1:8080\n[info] serving client bundle from crates/client/dist\n[info] GET /api/catalog 200",
            ),
            file(
                "auth.log",
                "740 B",
                "session opened for user guest\nsession closed for user guest",
            ),
        ],
        _ => Vec::new(),
    }
}

/// One level up, clamped at the root.
fn parent(path: &str) -> String {
    if path == "/" {
        return "/".to_owned();
    }
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(i) => path[..i].to_owned(),
    }
}

#[component]
pub fn Files() -> impl IntoView {
    let cwd = RwSignal::new("/home/guest".to_owned());
    let selected = RwSignal::new(None::<&'static str>);

    let entries = move || read_dir(&cwd.get());
    let selected_entry = move || {
        let name = selected.get()?;
        entries().into_iter().find(|e| e.name == name)
    };

    let navigate = move |path: String| {
        cwd.set(path);
        selected.set(None);
    };

    view! {
        <div class="app files">
            <aside class="files-side">
                <div class="side-label">"Places"</div>
                <For each=move || PLACES key=|(p, _)| *p let:place>
                    <button
                        class="side-item"
                        class:active=move || cwd.get() == place.0
                        on:click=move |_| navigate(place.0.to_owned())
                    >
                        <span class="side-dot"></span>
                        {place.1}
                        <span class="side-path">{place.0}</span>
                    </button>
                </For>
            </aside>

            <div class="files-main">
                <div class="files-bar">
                    <button
                        class="ghost-btn"
                        disabled=move || cwd.get() == "/"
                        on:click=move |_| navigate(parent(&cwd.get_untracked()))
                    >
                        "↑ Up"
                    </button>
                    <span class="crumb">{move || cwd.get()}</span>
                    <span class="count">{move || format!("{} items", entries().len())}</span>
                </div>

                <div class="files-list">
                    <For each=entries key=|e| e.name let:entry>
                        <button
                            class="row"
                            class:selected=move || selected.get() == Some(entry.name)
                            on:dblclick=move |_| {
                                if entry.kind == Kind::Dir {
                                    navigate(format!("{}/{}", cwd.get_untracked().trim_end_matches('/'), entry.name));
                                }
                            }
                            on:click=move |_| {
                                if entry.kind == Kind::Dir {
                                    navigate(format!("{}/{}", cwd.get_untracked().trim_end_matches('/'), entry.name));
                                } else {
                                    selected.set(Some(entry.name));
                                }
                            }
                        >
                            <span class="row-icon">{if entry.kind == Kind::Dir { "📁" } else { "📄" }}</span>
                            <span class="row-name">{entry.name}</span>
                            <span class="row-size">{entry.size}</span>
                        </button>
                    </For>
                </div>

                <div class="files-preview">
                    <Show
                        when=move || selected_entry().is_some()
                        fallback=|| view! { <span class="muted">"Select a file to preview it."</span> }
                    >
                        {move || {
                            selected_entry()
                                .map(|e| {
                                    view! {
                                        <div class="preview">
                                            <div class="preview-head">{e.name}</div>
                                            <pre>{e.body}</pre>
                                        </div>
                                    }
                                })
                        }}
                    </Show>
                </div>
            </div>
        </div>
    }
}
