//! The bottom bar: start button, one button per open window, tray + clock.

use gloo_timers::callback::Interval;
use leptos::prelude::*;

use crate::state::{Desktop, clock_strings};

#[component]
pub fn Taskbar() -> impl IntoView {
    let desktop = expect_context::<Desktop>();

    let time = RwSignal::new(String::new());
    let date = RwSignal::new(String::new());
    Effect::new(move |_| {
        let tick = move || {
            let (t, d) = clock_strings();
            time.set(t);
            date.set(d);
        };
        tick();
        // The clock lives as long as the app does, so the timer handle is
        // deliberately left running (gloo's `Interval` is not `Send + Sync`,
        // which `on_cleanup` requires).
        Interval::new(1_000, tick).forget();
    });

    view! {
        <footer class="taskbar">
            <button
                class="start-btn"
                class:open=move || desktop.start_open.get()
                on:click=move |_| desktop.start_open.update(|o| *o = !*o)
            >
                <span class="start-glyph">"▚"</span>
                <span class="start-label">"Start"</span>
            </button>

            <div class="task-buttons">
                <For each=move || desktop.windows.get() key=|w| w.id let:item>
                    {
                        let live = desktop.live(item.id, item);
                        // Labels come from the registry, not from geometry: the
                        // chrome must not re-render just because a window moved.
                        let manifest = desktop.manifest(item.app);
                        let accent = manifest.accent.clone();
                        let title_attr = manifest.name.clone();
                        let title_label = manifest.name.clone();
                        let glyph = manifest.glyph.clone();
                        let id = item.id;
                        view! {
                            <button
                                class="task-btn"
                                class:active=move || desktop.is_focused(id)
                                class:minimized=move || live.get().minimized
                                style=move || format!("--accent:{accent};")
                                title=title_attr
                                on:click=move |_| desktop.toggle_minimize(id)
                            >
                                <span class="task-glyph">{glyph}</span>
                                <span class="task-title">{title_label}</span>
                            </button>
                        }
                    }
                </For>
            </div>

            <div class="tray">
                <span
                    class="tray-dot"
                    class:online=move || desktop.is_online()
                    title=move || desktop.status_line()
                ></span>
                <div class="clock">
                    <span class="clock-time">{move || time.get()}</span>
                    <span class="clock-date">{move || date.get()}</span>
                </div>
            </div>
        </footer>
    }
}
