//! The host services an app can reach, wrapped so the app sees Rust types.

use js_sys::{Function, Reflect};
use wasm_bindgen::{JsCast, JsValue};

/// The shell's palette, so an app can match the desktop it runs in.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub accent: String,
    pub text: String,
    pub muted: String,
    pub panel: String,
    pub stroke: String,
}

/// Size of the window body, as a snapshot taken when the app was mounted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

/// Handle to the shell, handed to the app's function.
///
/// Cheap to clone (it is a reference to the JS host object), so views can capture
/// it freely.
#[derive(Clone, Debug)]
pub struct Context {
    window_id: u32,
    host: JsValue,
}

impl Context {
    pub(crate) fn new(window_id: u32, host: JsValue) -> Self {
        Self { window_id, host }
    }

    /// Identifies this window, for logging and per-window keys.
    pub fn window_id(&self) -> u32 {
        self.window_id
    }

    /// Snapshot of the body size at mount time.
    ///
    /// There is no resize notification yet; an app that must reflow on resize
    /// should listen to its own element instead.
    pub fn size(&self) -> Size {
        let object = self.value("size").unwrap_or(JsValue::UNDEFINED);
        Size {
            width: number_field(&object, "width").unwrap_or(0.0),
            height: number_field(&object, "height").unwrap_or(0.0),
        }
    }

    /// The shell's palette, so an app can match the desktop it is running in.
    pub fn theme(&self) -> Theme {
        let object = self.value("theme").unwrap_or(JsValue::UNDEFINED);
        Theme {
            accent: string_field(&object, "accent").unwrap_or_else(|| "#60a5fa".to_owned()),
            text: string_field(&object, "text").unwrap_or_else(|| "#e7ebf3".to_owned()),
            muted: string_field(&object, "muted").unwrap_or_else(|| "#97a1b4".to_owned()),
            panel: string_field(&object, "panel").unwrap_or_else(|| "#12151c".to_owned()),
            stroke: string_field(&object, "stroke")
                .unwrap_or_else(|| "rgba(255,255,255,.09)".to_owned()),
        }
    }

    /// Writes to the shell's console, prefixed with the app's id.
    pub fn log(&self, message: &str) {
        self.call("log", &[JsValue::from_str(message)]);
    }

    /// Asks the shell to open (or raise) another app by slug.
    pub fn open_app(&self, slug: &str) {
        self.call("openApp", &[JsValue::from_str(slug)]);
    }

    /// Asks the shell to close this window.
    pub fn close(&self) {
        self.call("close", &[]);
    }

    /// `localStorage` namespaced to this app, so two apps cannot collide.
    pub fn storage_get(&self, key: &str) -> Option<String> {
        storage()?.get_item(&self.storage_key(key)).ok().flatten()
    }

    pub fn storage_set(&self, key: &str, value: &str) {
        if let Some(storage) = storage() {
            let _ = storage.set_item(&self.storage_key(key), value);
        }
    }

    pub fn storage_remove(&self, key: &str) {
        if let Some(storage) = storage() {
            let _ = storage.remove_item(&self.storage_key(key));
        }
    }

    fn storage_key(&self, key: &str) -> String {
        let app = string_field(&self.host, "appId").unwrap_or_else(|| "app".to_owned());
        format!("web-desktop:{app}:{key}")
    }

    fn value(&self, name: &str) -> Option<JsValue> {
        let value = Reflect::get(&self.host, &JsValue::from_str(name)).ok()?;
        (!value.is_undefined() && !value.is_null()).then_some(value)
    }

    fn call(&self, name: &str, args: &[JsValue]) {
        let Some(function) = self.value(name).and_then(|f| f.dyn_into::<Function>().ok()) else {
            return;
        };
        let _ = match args {
            [] => function.call0(&self.host),
            [one] => function.call1(&self.host, one),
            [a, b] => function.call2(&self.host, a, b),
            _ => return,
        };
    }
}

/// `localStorage`, if the browser gives us one.
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn string_field(object: &JsValue, name: &str) -> Option<String> {
    Reflect::get(object, &JsValue::from_str(name))
        .ok()?
        .as_string()
}

fn number_field(object: &JsValue, name: &str) -> Option<f64> {
    Reflect::get(object, &JsValue::from_str(name))
        .ok()?
        .as_f64()
}
