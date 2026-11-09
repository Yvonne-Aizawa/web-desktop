//! Window chrome: title bar, drag, eight resize handles, maximize/close.
//!
//! Dragging and resizing both use pointer capture, so the browser keeps sending
//! `pointermove` to the element the gesture started on even when the pointer
//! leaves it. That avoids document-level listener bookkeeping entirely.
//!
//! Two details worth knowing:
//!
//! * Element lookup uses `ev.target()`, not `ev.current_target()`. With the
//!   `delegation` feature (see `Cargo.toml`) the real listener lives on
//!   `window`, so `currentTarget` is the window, not the element with the
//!   handler. The title bar's children are `pointer-events: none` so the target
//!   of a title-bar gesture is always the title bar itself.
//! * Geometry is read through `live` (a `Memo` for this row) but written through
//!   `Desktop::edit`, because a `Memo` is read-only.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, PointerEvent};

use crate::apps::AppBody;
use crate::state::{Desktop, WinData};

const MIN_W: f64 = 260.0;
const MIN_H: f64 = 150.0;

/// Which edge or corner a handle controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    N,
    S,
    E,
    W,
    Ne,
    Nw,
    Se,
    Sw,
}

const EDGES: [Edge; 8] = [
    Edge::N,
    Edge::S,
    Edge::E,
    Edge::W,
    Edge::Ne,
    Edge::Nw,
    Edge::Se,
    Edge::Sw,
];

impl Edge {
    const fn class(self) -> &'static str {
        match self {
            Self::N => "rz rz-n",
            Self::S => "rz rz-s",
            Self::E => "rz rz-e",
            Self::W => "rz rz-w",
            Self::Ne => "rz rz-ne",
            Self::Nw => "rz rz-nw",
            Self::Se => "rz rz-se",
            Self::Sw => "rz rz-sw",
        }
    }

    const fn key(self) -> u8 {
        self as u8
    }

    /// Apply a pointer delta, honoring the minimum size by moving the origin
    /// rather than shrinking past it.
    fn apply(self, d: &mut WinData, dx: f64, dy: f64) {
        match self {
            Self::E => d.w = (d.w + dx).max(MIN_W),
            Self::S => d.h = (d.h + dy).max(MIN_H),
            Self::W => {
                let nw = (d.w - dx).max(MIN_W);
                d.x += d.w - nw;
                d.w = nw;
            }
            Self::N => {
                let nh = (d.h - dy).max(MIN_H);
                d.y += d.h - nh;
                d.h = nh;
            }
            Self::Se => {
                d.w = (d.w + dx).max(MIN_W);
                d.h = (d.h + dy).max(MIN_H);
            }
            Self::Sw => {
                let nw = (d.w - dx).max(MIN_W);
                d.x += d.w - nw;
                d.w = nw;
                d.h = (d.h + dy).max(MIN_H);
            }
            Self::Ne => {
                d.w = (d.w + dx).max(MIN_W);
                let nh = (d.h - dy).max(MIN_H);
                d.y += d.h - nh;
                d.h = nh;
            }
            Self::Nw => {
                let nw = (d.w - dx).max(MIN_W);
                d.x += d.w - nw;
                d.w = nw;
                let nh = (d.h - dy).max(MIN_H);
                d.y += d.h - nh;
                d.h = nh;
            }
        }
    }
}

fn capture(ev: &PointerEvent) {
    if let Some(el) = ev.target().and_then(|t| t.dyn_into::<Element>().ok()) {
        let _ = el.set_pointer_capture(ev.pointer_id());
    }
}

fn release(ev: &PointerEvent) {
    if let Some(el) = ev.target().and_then(|t| t.dyn_into::<Element>().ok()) {
        let _ = el.release_pointer_capture(ev.pointer_id());
    }
}

#[component]
pub fn WindowFrame(id: u32, live: Memo<WinData>) -> impl IntoView {
    let desktop = expect_context::<Desktop>();

    // (pointer id, pointer x, pointer y, window x, window y)
    let drag = RwSignal::new(None::<(i32, f64, f64, f64, f64)>);
    // (pointer id, edge, pointer x, pointer y, x, y, w, h)
    let grip = RwSignal::new(None::<(i32, Edge, f64, f64, f64, f64, f64, f64)>);

    let style = move || live.get().css();
    let minimized = move || live.get().minimized;
    let maximized = move || live.get().maximized;
    let title = move || live.get().title();
    let glyph = move || live.get().glyph();

    let on_title_down = move |ev: PointerEvent| {
        if ev.button() != 0 {
            return;
        }
        desktop.focus(id);
        capture(&ev);

        // Dragging a maximized window restores it under the cursor, like most
        // desktop shells do.
        if live.get().maximized {
            let [_, _, rw, rh] = live.get().restore;
            let px = ev.client_x() as f64;
            let py = ev.client_y() as f64;
            desktop.edit(id, |d| {
                d.maximized = false;
                d.w = rw;
                d.h = rh;
                d.x = px - rw / 2.0;
                d.y = (py - 14.0).max(0.0);
            });
        }

        let d = live.get().clamped();
        desktop.edit(id, |w| *w = d);
        drag.set(Some((
            ev.pointer_id(),
            ev.client_x() as f64,
            ev.client_y() as f64,
            d.x,
            d.y,
        )));
        ev.prevent_default();
    };

    let on_title_move = move |ev: PointerEvent| {
        let Some((pid, px, py, x0, y0)) = drag.get_untracked() else {
            return;
        };
        if pid != ev.pointer_id() {
            return;
        }
        let dx = ev.client_x() as f64 - px;
        let dy = ev.client_y() as f64 - py;
        desktop.edit(id, |d| {
            d.x = x0 + dx;
            d.y = y0 + dy;
            *d = d.clamped();
        });
        ev.prevent_default();
    };

    let on_title_up = move |ev: PointerEvent| {
        if let Some((pid, ..)) = drag.get_untracked()
            && pid == ev.pointer_id()
        {
            drag.set(None);
            release(&ev);
        }
    };

    let grip_move = move |ev: PointerEvent| {
        let Some((pid, edge, px, py, x0, y0, w0, h0)) = grip.get_untracked() else {
            return;
        };
        if pid != ev.pointer_id() {
            return;
        }
        let dx = ev.client_x() as f64 - px;
        let dy = ev.client_y() as f64 - py;
        desktop.edit(id, |d| {
            (d.x, d.y, d.w, d.h) = (x0, y0, w0, h0);
            edge.apply(d, dx, dy);
        });
        ev.prevent_default();
    };

    let grip_up = move |ev: PointerEvent| {
        if let Some((pid, ..)) = grip.get_untracked()
            && pid == ev.pointer_id()
        {
            grip.set(None);
            release(&ev);
        }
    };

    view! {
        <section
            class="window"
            class:minimized=minimized
            class:maximized=maximized
            style=style
            on:pointerdown=move |_| desktop.focus(id)
        >
            <header
                class="titlebar"
                on:pointerdown=on_title_down
                on:pointermove=on_title_move
                on:pointerup=on_title_up
                on:pointercancel=on_title_up
                on:dblclick=move |_| desktop.toggle_maximize(id)
            >
                <span class="tb-glyph">{glyph}</span>
                <span class="tb-title">{title}</span>
                <div
                    class="tb-buttons"
                    on:pointerdown=|ev: PointerEvent| ev.stop_propagation()
                    on:dblclick=|ev: web_sys::MouseEvent| ev.stop_propagation()
                >
                    <button
                        class="tb-btn tb-min"
                        title="Minimize"
                        on:click=move |_| desktop.toggle_minimize(id)
                    >
                        "—"
                    </button>
                    <button
                        class="tb-btn tb-max"
                        title="Maximize"
                        on:click=move |_| desktop.toggle_maximize(id)
                    >
                        "▢"
                    </button>
                    <button
                        class="tb-btn tb-close"
                        title="Close"
                        on:click=move |_| desktop.close(id)
                    >
                        "✕"
                    </button>
                </div>
            </header>

            <div class="window-body">
                <AppBody app=live.get().app />
            </div>

            <For each=move || EDGES key=|e| e.key() let:edge>
                <div
                    class=edge.class()
                    on:pointerdown=move |ev: PointerEvent| {
                        if ev.button() != 0 {
                            return;
                        }
                        desktop.focus(id);
                        capture(&ev);
                        let d = live.get();
                        if d.maximized {
                            return;
                        }
                        grip
                            .set(
                                Some((
                                    ev.pointer_id(),
                                    edge,
                                    ev.client_x() as f64,
                                    ev.client_y() as f64,
                                    d.x,
                                    d.y,
                                    d.w,
                                    d.h,
                                )),
                            );
                        ev.prevent_default();
                    }
                    on:pointermove=grip_move
                    on:pointerup=grip_up
                    on:pointercancel=grip_up
                ></div>
            </For>
        </section>
    }
}
