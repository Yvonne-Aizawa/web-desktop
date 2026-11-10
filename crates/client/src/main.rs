//! Web Desktop - a small window manager compiled from Rust to WebAssembly.
//!
//! Build/run:
//!   cargo run -p server                        # backend + API on :8080
//!   cd crates/client && trunk serve             # frontend on :8081, proxies /api
//!
//! Or single port:
//!   cd crates/client && trunk build --release && cargo run -p server --release

mod api;
mod apps;
mod format;
mod login;
mod runtime;
mod start_menu;
mod state;
mod taskbar;
mod window;

use leptos::prelude::*;

use crate::login::LoginScreen;
use crate::start_menu::StartMenu;
use crate::state::Desktop;
use crate::taskbar::Taskbar;
use crate::window::WindowFrame;

#[component]
fn App() -> impl IntoView {
    let desktop = Desktop::new();
    provide_context(desktop);

    // Ask whether this server wants a password before fetching anything, then keep
    // going whether or not it answered.
    Effect::new(move |_| desktop.bootstrap());

    let background = move || format!("background:{};", desktop.wallpaper.get());

    view! {
        <div class="desktop" style=background>
            <Show
                when=move || !desktop.auth.get().is_locked()
                fallback=move || view! { <LoginScreen /> }
            >
                <main class="window-layer">
                    <For each=move || desktop.windows.get() key=|w| w.id let:item>
                        {
                            let live = desktop.live(item.id, item);
                            view! { <WindowFrame id=item.id app=item.app live=live /> }
                        }
                    </For>

                    <Show when=move || desktop.windows.get().is_empty()>
                        <div class="empty-hint">
                            <div class="hint-mark">"▚"</div>
                            <h1>"Web Desktop"</h1>
                            <p>"Rust all the way down: axum serves the API, leptos renders this shell."</p>
                            <p class="muted">"Open the Start menu to launch a window."</p>
                        </div>
                    </Show>
                </main>

                <StartMenu />
                <Taskbar />
            </Show>
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
