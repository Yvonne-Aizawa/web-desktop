//! The applications that can run inside windows.

mod about;
mod files;
mod settings;
mod terminal;

pub use about::About;
pub use files::Files;
pub use settings::Settings;
pub use terminal::Terminal;

use leptos::prelude::*;
use shared::AppId;

/// Renders the right app for a window's [`AppId`].
#[component]
pub fn AppBody(#[prop(into)] app: AppId) -> impl IntoView {
    match app {
        AppId::Terminal => view! { <Terminal /> }.into_any(),
        AppId::Files => view! { <Files /> }.into_any(),
        AppId::Settings => view! { <Settings /> }.into_any(),
        AppId::About => view! { <About /> }.into_any(),
    }
}
