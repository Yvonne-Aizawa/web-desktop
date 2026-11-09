//! A small built-in shell. Not a real shell: it knows a few commands and can
//! open other windows, which demonstrates app-to-app calls through `Desktop`.

use std::sync::atomic::{AtomicU32, Ordering};

use leptos::prelude::*;
use shared::AppId;
use wasm_bindgen::JsCast;

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

    fn echo(text: impl Into<String>) -> Self {
        Self::new("t-cmd", format!("guest@web-desktop:~$ {}", text.into()))
    }
}

const FILES: [(&str, &str); 4] = [
    (
        "readme.txt",
        "Everything here is Rust: axum on the server, Leptos in the browser.",
    ),
    (
        "notes.md",
        "# notes\n\n- windows hold RwSignal<WinData>\n- a geometry change repaints one window",
    ),
    (
        "todo.txt",
        "[x] window manager\n[x] taskbar\n[ ] persistence",
    ),
    ("hosts", "127.0.0.1 localhost\n127.0.0.1 web-desktop"),
];

#[component]
pub fn Terminal() -> impl IntoView {
    let desktop = expect_context::<Desktop>();
    let lines = RwSignal::new(vec![
        Line::out("web-desktop shell 0.1.0 (wasm32-unknown-unknown)"),
        Line::out("type `help` for built-ins, `open <app>` to launch a window"),
    ]);
    let input = NodeRef::<leptos::html::Input>::new();

    let focus_input = move || {
        if let Some(el) = input.get() {
            let _ = el.focus();
        }
    };

    let run = move |raw: String| {
        let cmd = raw.trim().to_owned();
        lines.update(|l| l.push(Line::echo(cmd.as_str())));
        if cmd.is_empty() {
            return;
        }

        let mut parts = cmd.split_whitespace();
        let prog = parts.next().unwrap_or_default();
        let args: Vec<&str> = parts.collect();
        let joined = args.join(" ");

        let result: Vec<Line> = match prog {
            "help" => vec![
                Line::out("help  clear  echo  ls  cat  open  neofetch  date  whoami  uname  pwd"),
                Line::out("`open` takes an app slug: terminal, files, settings, about"),
            ],
            "clear" => {
                lines.set(Vec::new());
                return;
            }
            "echo" => vec![Line::out(joined)],
            "pwd" => vec![Line::out("/home/guest")],
            "whoami" => vec![Line::out("guest")],
            "uname" => vec![Line::out("WebDesktop 0.1.0 wasm32-unknown-unknown rustc")],
            "date" => vec![Line::out(format!(
                "{} {}",
                crate::state::clock_strings().1,
                crate::state::clock_strings().0
            ))],
            "ls" => vec![Line::out(
                FILES
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join("  "),
            )],
            "cat" => {
                if joined.is_empty() {
                    vec![Line::err("cat: missing operand")]
                } else {
                    match FILES.iter().find(|(name, _)| *name == joined) {
                        Some((_, body)) => {
                            body.split('\n').map(|l| Line::out(l.to_owned())).collect()
                        }
                        None => vec![Line::err(format!("cat: {joined}: no such file"))],
                    }
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
            "neofetch" => {
                let open = desktop.windows.with(|w| w.len());
                vec![
                    Line::out("        ▄▄▄▄▄        guest@web-desktop"),
                    Line::out("     ▄█████████▄     ----------------"),
                    Line::out("    █████████████    OS: WebDesktop 0.1.0"),
                    Line::out("    ███  ▀▀▀  ███    Shell: wasm32 + leptos"),
                    Line::out("    █████████████    Backend: axum"),
                    Line::out(format!("     ▀█████████▀     Windows open: {open}")),
                    Line::out(format!(
                        "    Apps: {}",
                        AppId::ALL
                            .iter()
                            .map(|a| a.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                ]
            }
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
                <span class="term-ps1">"guest@web-desktop:~$"</span>
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
