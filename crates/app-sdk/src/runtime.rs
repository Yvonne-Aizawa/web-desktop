//! The mount contract: instantiate the view per window and keep it alive until the
//! shell says otherwise.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::HashMap,
};

use leptos::prelude::*;
use wasm_bindgen::JsValue;

use crate::Context;

thread_local! {
    /// Mounted instances, keyed by the id handed back to the shell. The handles are
    /// boxed as `Any` because [`leptos::mount::mount_to`] returns a type parameterised
    /// by the view, which is not nameable here. Dropping the box unmounts the view
    /// and disposes its reactive owner, so a closed window leaves nothing behind.
    static MOUNTED: RefCell<HashMap<u32, Box<dyn Any>>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
}

/// Called by the generated `mount` export.
pub fn mount<V>(element: web_sys::HtmlElement, host: JsValue, app: fn(Context) -> V) -> u32
where
    V: IntoView + 'static,
{
    // Each app instance is its own wasm instance, so it needs its own panic hook;
    // without it a panic in an app is an unreadable wasm trap.
    console_error_panic_hook::set_once();

    let window_id = js_sys::Reflect::get(&host, &JsValue::from_str("windowId"))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u32;

    let context = Context::new(window_id, host);
    let handle = leptos::mount::mount_to(element, move || app(context));

    let id = NEXT_ID.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1));
        id
    });
    MOUNTED.with(|mounted| mounted.borrow_mut().insert(id, Box::new(handle)));
    id
}

/// Called by the generated `unmount` export.
pub fn unmount(id: u32) {
    MOUNTED.with(|mounted| {
        mounted.borrow_mut().remove(&id);
    });
}
