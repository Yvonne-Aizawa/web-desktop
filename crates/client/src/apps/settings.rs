//! Settings: wallpaper picker, backend status, layout actions.

use leptos::prelude::*;
use shared::AppId;

use crate::format::human_size;
use crate::state::Desktop;

#[component]
pub fn Settings() -> impl IntoView {
    let desktop = expect_context::<Desktop>();

    let status_class = move || {
        if desktop.is_online() {
            "badge badge-ok"
        } else if desktop.health.get().is_some() {
            "badge badge-bad"
        } else {
            "badge badge-wait"
        }
    };

    view! {
        <div class="app settings">
            <section class="pane">
                <h3>"Appearance"</h3>
                <p class="muted">"Wallpapers are served by the axum backend with `shared::Wallpaper`."</p>
                <div class="swatches">
                    <For each=move || desktop.wallpapers.get() key=|w| w.id.clone() let:w>
                        {
                            let css_paint = w.css.clone();
                            let css_check = w.css.clone();
                            let css_apply = w.css;
                            let name_title = w.name.clone();
                            let name = w.name;
                            view! {
                                <button
                                    class="swatch"
                                    class:active=move || desktop.wallpaper.get() == css_check
                                    style=move || format!("background:{css_paint};")
                                    title=name_title
                                    on:click=move |_| desktop.wallpaper.set(css_apply.clone())
                                >
                                    <span class="swatch-name">{name}</span>
                                </button>
                            }
                        }
                    </For>
                </div>
            </section>

            <section class="pane">
                <h3>"Backend"</h3>
                <div class="kv">
                    <span>"status"</span>
                    <span class=status_class>{move || desktop.status_line()}</span>
                </div>
                <p class="muted">
                    "The desktop boots from `shared::default_catalog()` and refreshes from `/api/catalog`."
                </p>
                <button class="ghost-btn" on:click=move |_| desktop.refresh()>"Reconnect"</button>
            </section>

            <section class="pane">
                <h3>"File browser"</h3>
                <Show
                    when=move || desktop.fs.get().enabled
                    fallback=move || {
                        view! {
                            <div class="kv">
                                <span>"status"</span>
                                <span class="badge badge-bad">"disabled"</span>
                            </div>
                            <p class="muted">
                                "The server rejected or was not given a root. Start it with WEB_DESKTOP_ROOT=/some/folder, and make sure WEB_DESKTOP_FS is not `off`."
                            </p>
                        }
                    }
                >
                    <div class="kv">
                        <span>"status"</span>
                        <span class="badge badge-wait">"read-only"</span>
                    </div>
                    <div class="kv">
                        <span>"root"</span>
                        <span class="path-value" title=move || desktop.fs.get().root>
                            {move || desktop.fs.get().root.clone()}
                        </span>
                    </div>
                    <div class="kv">
                        <span>"preview limit"</span>
                        <span>{move || human_size(desktop.fs.get().max_preview_bytes)}</span>
                    </div>
                    <p class="muted">
                        "Files are read on the server and fenced to that folder: `..`, absolute paths and links that leave it are refused."
                    </p>
                    <button class="ghost-btn" on:click=move |_| desktop.open(AppId::Files)>
                        "Open Files"
                    </button>
                </Show>
            </section>

            <section class="pane">
                <h3>"Windows"</h3>
                <div class="kv">
                    <span>"open"</span>
                    <span>{move || desktop.windows.with(|w| w.len()).to_string()}</span>
                </div>
                <button
                    class="ghost-btn danger"
                    disabled=move || desktop.windows.with(|w| w.is_empty())
                    on:click=move |_| desktop.close_all()
                >
                    "Close all windows"
                </button>
            </section>
        </div>
    }
}
