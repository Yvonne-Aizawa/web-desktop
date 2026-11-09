//! Start menu: search over the server-provided app catalog.

use leptos::prelude::*;
use shared::AppManifest;

use crate::state::Desktop;

#[component]
pub fn StartMenu() -> impl IntoView {
    let desktop = expect_context::<Desktop>();
    let query = RwSignal::new(String::new());

    let filtered = move || {
        let needle = query.get().trim().to_lowercase();
        desktop
            .catalog
            .get()
            .into_iter()
            .filter(|a| {
                needle.is_empty()
                    || a.name.to_lowercase().contains(&needle)
                    || a.blurb.to_lowercase().contains(&needle)
                    || a.id.slug().contains(&needle)
            })
            .collect::<Vec<AppManifest>>()
    };

    let open = move |id: shared::AppId| {
        desktop.open(id);
        desktop.start_open.set(false);
        query.set(String::new());
    };

    view! {
        <Show when=move || desktop.start_open.get()>
            <div class="menu-backdrop" on:click=move |_| desktop.start_open.set(false)></div>
            <div class="start-menu">
                <div class="menu-head">
                    <span class="menu-user">"guest"</span>
                    <input
                        class="menu-search"
                        placeholder="Search apps…"
                        autocomplete="off"
                        prop:value=move || query.get()
                        on:input=move |ev| {
                            query
                                .set(
                                    ev
                                        .target()
                                        .and_then(|t| {
                                            wasm_bindgen::JsCast::dyn_into::<
                                                web_sys::HtmlInputElement,
                                            >(t)
                                                .ok()
                                        })
                                        .map(|el| el.value())
                                        .unwrap_or_default(),
                                )
                        }
                    />
                </div>

                <div class="menu-grid">
                    <For each=filtered key=|a| a.id let:app>
                        {
                            let accent = app.accent.clone();
                            let glyph = app.glyph.clone();
                            let name = app.name.clone();
                            let blurb = app.blurb.clone();
                            let id = app.id;
                            view! {
                                <button
                                    class="menu-item"
                                    style=move || format!("--accent:{accent};")
                                    on:click=move |_| open(id)
                                >
                                    <span class="menu-glyph">{glyph}</span>
                                    <span class="menu-meta">
                                        <strong>{name}</strong>
                                        <small>{blurb}</small>
                                    </span>
                                </button>
                            }
                        }
                    </For>
                </div>

                <Show when=move || filtered().is_empty()>
                    <p class="menu-empty">"No apps match that search."</p>
                </Show>

                <div class="menu-foot">
                    <span class="tray-dot" class:online=move || desktop.is_online()></span>
                    <span>{move || desktop.status_line()}</span>
                </div>
            </div>
        </Show>
    }
}
