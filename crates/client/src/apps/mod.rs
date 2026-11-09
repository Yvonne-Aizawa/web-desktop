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

use crate::state::{AppRef, Desktop};

/// Renders the program for a window's app handle.
#[component]
pub fn AppBody(app: AppRef) -> impl IntoView {
    let desktop = expect_context::<Desktop>();
    let manifest = desktop.manifest(app);

    match manifest.builtin_id() {
        Some(AppId::Terminal) => view! { <Terminal /> }.into_any(),
        Some(AppId::Files) => view! { <Files /> }.into_any(),
        Some(AppId::Settings) => view! { <Settings /> }.into_any(),
        Some(AppId::About) => view! { <About /> }.into_any(),
        None => view! { <NotInstalled manifest=manifest /> }.into_any(),
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
