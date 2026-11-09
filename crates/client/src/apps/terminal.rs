//! A small built-in shell over the same sandboxed folder the Files app browses.
//!
//! `ls`, `cat`, `cd` and `pwd` talk to `/api/fs/*`; the rest is local. Commands
//! that need the disk print their result when the reply arrives, so the prompt
//! stays usable.

use std::sync::atomic::{AtomicU32, Ordering};

use leptos::prelude::*;
use shared::AppId;
use shared::fs::{EntryKind, PreviewBody};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use crate::api;
use crate::format::human_size;
use crate::state::Desktop;

/// Stable, unique key for the keyed `For` over output lines.
static SEQ: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, PartialEq)]
struct Line {
    id: u32,
    class: &'static str,
    text: String,
}

impl Line {
    fn new(class: &'static str, text: impl Into<String>) -> Self {
        Self {
            id: SEQ.fetch_add(1, Ordering::Relaxed),
            class,
            text: text.into(),
        }
    }

    fn out(text: impl Into<String>) -> Self {
        Self::new("t-out", text)
    }

    fn err(text: impl Into<String>) -> Self {
        Self::new("t-err", text)
    }

    fn echo(cwd: &str, command: &str) -> Self {
        Self::new("t-cmd", format!("{} $ {}", prompt_path(cwd), command))
    }
}

/// `/` at the sandbox root, `/notes` below it.
fn prompt_path(cwd: &str) -> String {
    format!("guest@web-desktop:/{cwd}")
}

/// Resolve a typed path against the current directory.
///
/// `..` is resolved here rather than sent to the server: the API rejects `..`
/// outright, so the shell has to walk up on its own. The result never contains
/// `..`, and a leading `/` means the sandbox root.
fn resolve(cwd: &str, arg: &str) -> String {
    let arg = arg.trim();
    if arg.is_empty() || arg == "~" || arg == "/" {
        return String::new();
    }

    let mut parts: Vec<&str> = if arg.starts_with('/') {
        Vec::new()
    } else {
        cwd.split('/').filter(|p| !p.is_empty()).collect()
    };

    for part in arg.split('/') {
        match part {
            "" | "." => {}
            // Walked up locally; the API refuses to see `..` at all.
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

#[component]
pub fn Terminal() -> impl IntoView {
    let desktop = expect_context::<Desktop>();
    let lines = RwSignal::new(vec![
        Line::out("web-desktop shell 0.1.0 (wasm32-unknown-unknown)"),
        Line::out("`ls`, `cat`, `cd` and `pwd` read the sandboxed folder on the server."),
        Line::out("type `help` for the other built-ins"),
    ]);
    let cwd = RwSignal::new(String::new());
    let input = NodeRef::<leptos::html::Input>::new();

    let focus_input = move || {
        if let Some(el) = input.get() {
            let _ = el.focus();
        }
    };

    let run = move |raw: String| {
        let started_in = cwd.get_untracked();
        let cmd = raw.trim().to_owned();
        lines.update(|l| l.push(Line::echo(&started_in, &cmd)));
        if cmd.is_empty() {
            return;
        }

        let mut parts = cmd.split_whitespace();
        let prog = parts.next().unwrap_or_default();
        let args: Vec<&str> = parts.collect();
        let joined = args.join(" ");

        let result: Vec<Line> = match prog {
            "help" => vec![
                Line::out("filesystem: ls [dir]  cd [dir]  cat <file>  pwd"),
                Line::out(
                    "built-in:   help  echo  open <app>  date  whoami  uname  neofetch  clear",
                ),
                Line::out("`open` takes an app slug: terminal, files, settings, about"),
            ],
            "clear" => {
                lines.set(Vec::new());
                return;
            }
            "echo" => vec![Line::out(joined)],
            "pwd" => vec![Line::out(format!("/{started_in}"))],
            "whoami" => vec![Line::out("guest")],
            "uname" => vec![Line::out("WebDesktop 0.1.0 wasm32-unknown-unknown rustc")],
            "date" => {
                let (time, date) = crate::state::clock_strings();
                vec![Line::out(format!("{date} {time}"))]
            }
            "neofetch" => {
                let open = desktop.windows.with(|w| w.len());
                vec![
                    Line::out("        ▄▄▄▄▄        guest@web-desktop"),
                    Line::out("     ▄█████████▄     ----------------"),
                    Line::out("    █████████████    OS: WebDesktop 0.1.0"),
                    Line::out("    ███  ▀▀▀  ███    Shell: wasm32 + leptos"),
                    Line::out("    █████████████    Backend: axum"),
                    Line::out(format!("     ▀█████████▀     Windows open: {open}")),
                    Line::out(format!("    Root: {}", desktop.fs.get().root)),
                ]
            }
            "ls" => {
                let dir = resolve(&started_in, args.first().copied().unwrap_or(""));
                spawn_local(async move {
                    let class;
                    let text = match api::list_dir(&dir).await {
                        Ok(listing) => {
                            class = "t-out";
                            let mut names: Vec<String> = listing
                                .entries
                                .iter()
                                .filter(|e| !e.hidden)
                                .map(|e| match e.kind {
                                    EntryKind::Dir => format!("{}/", e.name),
                                    _ if e.blocked => format!("{} (blocked link)", e.name),
                                    _ => e.name.clone(),
                                })
                                .collect();
                            names.sort();
                            if names.is_empty() {
                                "(empty)".to_owned()
                            } else {
                                names.join("  ")
                            }
                        }
                        Err(err) => {
                            class = "t-err";
                            format!("ls: {err}")
                        }
                    };
                    lines.update(|l| l.push(Line::new(class, text)));
                });
                Vec::new()
            }
            "cd" => {
                let target = resolve(&started_in, args.first().copied().unwrap_or("/"));
                if target.is_empty() {
                    cwd.set(String::new());
                    return;
                }
                spawn_local(async move {
                    match api::list_dir(&target).await {
                        Ok(_) => cwd.set(target),
                        Err(err) => lines.update(|l| l.push(Line::err(format!("cd: {err}")))),
                    }
                });
                Vec::new()
            }
            "cat" => {
                if joined.is_empty() {
                    vec![Line::err("cat: missing operand")]
                } else {
                    let target = resolve(&started_in, &joined);
                    spawn_local(async move {
                        let mut out: Vec<Line> = Vec::new();
                        match api::preview_file(&target).await {
                            Ok(file) => match file.body {
                                PreviewBody::Text { content, truncated } => {
                                    out.extend(content.lines().map(|l| Line::out(l.to_owned())));
                                    if truncated {
                                        out.push(Line::new(
                                            "t-dim",
                                            format!(
                                                "(truncated; file is {})",
                                                human_size(file.entry.size)
                                            ),
                                        ));
                                    }
                                }
                                PreviewBody::Binary { mime } => out.push(Line::err(format!(
                                    "cat: {}: binary file ({mime}, {})",
                                    file.entry.name,
                                    human_size(file.entry.size)
                                ))),
                                // A terminal cannot draw a picture; point at the app
                                // that can.
                                PreviewBody::Image { mime } => out.push(Line::err(format!(
                                    "cat: {}: image ({mime}, {}) — open it in Files to view it",
                                    file.entry.name,
                                    human_size(file.entry.size)
                                ))),
                            },
                            Err(err) => out.push(Line::err(format!("cat: {err}"))),
                        }
                        lines.update(|l| l.extend(out));
                    });
                    Vec::new()
                }
            }
            "open" => match AppId::from_slug(&joined) {
                Some(app) => {
                    desktop.open(app);
                    vec![Line::out(format!("launching {}...", app.name()))]
                }
                None => vec![Line::err(format!(
                    "open: unknown app `{joined}` (try: terminal, files, settings, about)"
                ))],
            },
            other => vec![Line::err(format!("{other}: command not found"))],
        };

        lines.update(|l| l.extend(result));
    };

    let on_key = move |ev: web_sys::KeyboardEvent| {
        if ev.key() != "Enter" {
            return;
        }
        ev.prevent_default();
        // `target()`, not `current_target()`: event delegation puts the real
        // listener on `window`.
        let Some(el) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        let value = el.value();
        el.set_value("");
        run(value);
    };

    Effect::new(move |_| focus_input());

    view! {
        <div class="app terminal" on:click=move |_| focus_input()>
            <div class="term-scroll">
                <For each=move || lines.get() key=|l| l.id let:line>
                    <div class=line.class>{line.text.clone()}</div>
                </For>
            </div>
            <div class="term-prompt">
                <span class="term-ps1">
                    {move || format!("{} $", prompt_path(&cwd.get()))}
                </span>
                <input
                    node_ref=input
                    class="term-input"
                    autocomplete="off"
                    spellcheck="false"
                    on:keydown=on_key
                />
            </div>
        </div>
    }
}
