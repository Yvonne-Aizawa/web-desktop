//! About: build identity, the crate stack, and a launcher for available apps.

use leptos::prelude::*;
use shared::AppId;

use crate::state::Desktop;

#[component]
pub fn About() -> impl IntoView {
    let desktop = expect_context::<Desktop>();

    view! {
        <div class="app about">
            <div class="about-hero">
                <div class="logo">"▚"</div>
                <div>
                    <h2>"Web Desktop"</h2>
                    <p class="muted">{move || desktop.status_line()}</p>
                </div>
            </div>

            <section class="pane">
                <h3>"Stack"</h3>
                <div class="kv"><span>"frontend"</span><span>"leptos 0.8 · wasm-bindgen · web-sys"</span></div>
                <div class="kv"><span>"backend"</span><span>"axum 0.8 · tokio · tower-http"</span></div>
                <div class="kv"><span>"shared"</span><span>"one serde type set for both sides"</span></div>
                <div class="kv"><span>"build"</span><span>"trunk + cargo"</span></div>
            </section>

            <section class="pane">
                <h3>"Launch"</h3>
                <div class="launch-grid">
                    <For each=move || desktop.catalog.get() key=|a| a.id let:app>
                        {
                            let accent = app.accent.clone();
                            let glyph = app.glyph.clone();
                            let name = app.name.clone();
                            let blurb = app.blurb.clone();
                            let id = app.id;
                            view! {
                                <button
                                    class="launch"
                                    style=move || format!("--accent:{accent};")
                                    on:click=move |_| desktop.open(id)
                                >
                                    <span class="launch-glyph">{glyph}</span>
                                    <span class="launch-meta">
                                        <strong>{name}</strong>
                                        <small>{blurb}</small>
                                    </span>
                                </button>
                            }
                        }
                    </For>
                </div>
                <p class="muted">
                    {move || format!(
                        "{} apps registered; {} window(s) open.",
                        AppId::ALL.len(),
                        desktop.windows.with(|w| w.len()),
                    )}
                </p>
            </section>
        </div>
    }
}
