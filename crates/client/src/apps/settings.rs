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
                        <span>"access"</span>
                        <Show
                            when=move || desktop.fs.get().writable
                            fallback=move || {
                                view! { <span class="badge badge-wait">"read-only"</span> }
                            }
                        >
                            <span class="badge badge-ok">"read-write"</span>
                        </Show>
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
                        "Everything happens on the server, fenced to that folder: `..`, absolute paths and links that leave it are refused. Saves are atomic, and deletes ask first."
                    </p>
                    <Show when=move || !desktop.fs.get().writable>
                        <p class="muted">
                            {move || {
                                desktop
                                    .fs
                                    .get()
                                    .readonly_reason
                                    .clone()
                                    .unwrap_or_default()
                            }}
                        </p>
                    </Show>
                    <button class="ghost-btn" on:click=move |_| desktop.open_builtin(AppId::Files)>
                        "Open Files"
                    </button>
                </Show>
            </section>

            <section class="pane">
                <h3>"Outbound proxy"</h3>
                <Show
                    when=move || desktop.proxy.get().enabled()
                    fallback=move || {
                        view! {
                            <div class="kv">
                                <span>"status"</span>
                                <span class="badge badge-wait">"off"</span>
                            </div>
                            <p class="muted">
                                "Apps cannot fetch a cross-origin URL. Start the server with `WEB_DESKTOP_PROXY_HOSTS=api.example.com` to allow it."
                            </p>
                        }
                    }
                >
                    <div class="kv">
                        <span>"status"</span>
                        <span class="badge badge-ok">"on"</span>
                    </div>
                    <div class="kv">
                        <span>"hosts"</span>
                        <span class="path-value">{move || desktop.proxy.get().hosts.join(", ")}</span>
                    </div>
                    <div class="kv">
                        <span>"private addresses"</span>
                        <span>{move || {
                            if desktop.proxy.get().allow_private { "allowed" } else { "refused" }
                        }}</span>
                    </div>
                    <p class="muted">
                        "Requests are made by the server, so CORS does not apply. Redirects are re-checked against the same list."
                    </p>
                </Show>
            </section>

            <section class="pane">
                <h3>"AI chat"</h3>
                <Show
                    when=move || desktop.ai.get().enabled()
                    fallback=move || {
                        view! {
                            <div class="kv">
                                <span>"status"</span>
                                <span class="badge badge-wait">"off"</span>
                            </div>
                            <p class="muted">
                                "Apps cannot hold a conversation with a model. Start the server with an OpenAI-compatible endpoint to enable it:"
                            </p>
                            <p class="path-value">
                                "WEB_DESKTOP_AI_URL=https://api.openai.com/v1 WEB_DESKTOP_AI_KEY=… WEB_DESKTOP_AI_MODEL=gpt-4o-mini"
                            </p>
                        }
                    }
                >
                    <div class="kv">
                        <span>"status"</span>
                        <span class="badge badge-ok">"on"</span>
                    </div>
                    <div class="kv">
                        <span>"default model"</span>
                        <span class="path-value">{move || desktop.ai.get().default_model}</span>
                    </div>
                    <div class="kv">
                        <span>"offered"</span>
                        <span class="path-value">{move || desktop.ai.get().models.join(", ")}</span>
                    </div>
                    <p class="muted">
                        "The server holds the provider's key and makes the call, so an app never sees it and cannot name another host. Which models an app may ask for is the list above — the server refuses anything else."
                    </p>
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
