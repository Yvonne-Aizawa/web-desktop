//! The applications that can run inside windows.
//!
//! A window records only an [`AppRef`]; this module turns that handle into a
//! component. Two lookups happen here: the registry resolves the handle to a
//! manifest (which supplies the label and size), and the manifest's slug resolves
//! to a built-in via [`shared::AppId`]. When the second lookup fails the app came
//! from a newer server than this shell, which is a normal state to be in and gets
//! an explanatory pane rather than an error.

mod about;
mod files;
mod settings;
mod terminal;

pub use about::About;
pub use files::Files;
pub use settings::Settings;
pub use terminal::Terminal;

use leptos::prelude::*;
use shared::{AppId, AppManifest};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use crate::runtime::Load;
use crate::state::{AppRef, Desktop};

/// Renders the program for a window's app handle: a compiled-in component, a served
/// bundle, or an explanation.
#[component]
pub fn AppBody(app: AppRef, window: u32) -> impl IntoView {
    let desktop = expect_context::<Desktop>();
    let manifest = desktop.manifest(app);

    match manifest.builtin_id() {
        Some(AppId::Terminal) => view! { <Terminal /> }.into_any(),
        Some(AppId::Files) => view! { <Files /> }.into_any(),
        Some(AppId::Settings) => view! { <Settings /> }.into_any(),
        Some(AppId::About) => view! { <About /> }.into_any(),
        None if manifest.served().is_some() => {
            view! { <ServedApp manifest=manifest window=window /> }.into_any()
        }
        None => view! { <NotInstalled manifest=manifest /> }.into_any(),
    }
}

/// Hosts an app the server serves: a container for its DOM, and the load states
/// around it.
#[component]
fn ServedApp(manifest: AppManifest, window: u32) -> impl IntoView {
    let desktop = expect_context::<Desktop>();
    let container = NodeRef::<leptos::html::Div>::new();
    let state = RwSignal::new(Load::Loading);
    let mounted = RwSignal::new(None::<crate::runtime::Mounted>);

    // Loads once: the effect reads only props.
    Effect::new(move |_| {
        let manifest = manifest.clone();
        spawn_local(async move {
            let Some(element) = container.get() else {
                state.set(Load::Failed("no container for the app".to_owned()));
                return;
            };
            let element: web_sys::HtmlElement = element.unchecked_into();
            match crate::runtime::mount(&manifest, window, element, desktop).await {
                Ok(handle) => {
                    mounted.set(Some(handle));
                    state.set(Load::Ready);
                }
                Err(err) => state.set(Load::Failed(err)),
            }
        });
    });

    // A closed window must take the app's reactive owner with it.
    on_cleanup(move || {
        if let Some(handle) = mounted.get_untracked() {
            crate::runtime::unmount(&handle);
        }
    });

    view! {
        <div class="served-app" node_ref=container></div>
        <Show when=move || state.get() != Load::Ready>
            <div class="served-status">
                {move || match state.get() {
                    Load::Failed(err) => view! {
                        <div class="served-error">
                            <strong>"This app could not be loaded"</strong>
                            <span>{err}</span>
                        </div>
                    }
                    .into_any(),
                    _ => view! { <span class="muted">"Loading app…"</span> }.into_any(),
                }}
            </div>
        </Show>
    }
}

/// Shown for a catalog entry this build has no program for.
///
/// The window manager, taskbar and start menu all work normally around it: the app
/// simply is not installed here. This is the version-skew pane, and it is the
/// reason `AppManifest::id` is a slug rather than a closed enum.
#[component]
fn NotInstalled(manifest: AppManifest) -> impl IntoView {
    let glyph = manifest.glyph.clone();
    let name = manifest.name.clone();
    let blurb = manifest.blurb.clone();
    let slug = manifest.id.clone();

    view! {
        <div class="app unavailable">
            <div class="unavailable-mark">{glyph}</div>
            <h3>{name}</h3>
            <p class="muted">{blurb}</p>
            <p class="unavailable-note">
                "This desktop build has no program registered for `" {slug}
                "`. The catalog offers it, so the shell and the server are out of step: a newer catalog, an older shell."
            </p>
        </div>
    }
}
