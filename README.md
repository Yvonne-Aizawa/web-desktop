# web_desktop

A small browser window manager, written in Rust end to end: `axum` on the server,
`leptos` compiled to WebAssembly in the browser, and one `shared` crate so the
wire types are defined once.

Draggable and resizable windows with 8 resize handles, z-order focus, double-click
to maximize, a taskbar with per-window buttons, a start menu with search, six
wallpapers, a clock, and four built-in apps (Terminal, Files, Settings, About).

```
crates/
  shared/   serde types used by both sides (AppId, AppManifest, Catalog, Health, Wallpaper)
  server/   axum: JSON API + static hosting of the client bundle
  client/   leptos CSR app, compiled to wasm32-unknown-unknown by trunk
```

## Libraries

**Backend**

| Crate | Why |
| --- | --- |
| `axum` | Routing and handlers; the default choice on tokio/tower |
| `tokio` | Runtime, `TcpListener`, graceful shutdown |
| `tower-http` | `ServeDir` for the client bundle, `TraceLayer` for request logs |
| `tracing` + `tracing-subscriber` | Structured logs with `RUST_LOG` support |
| `serde` / `serde_json` | Wire format, shared with the frontend |

**Frontend**

| Crate | Why |
| --- | --- |
| `leptos` (0.8, `csr` + `delegation`) | Signals, `view!` macro, WASM CSR |
| `wasm-bindgen` / `web-sys` / `js-sys` | DOM, pointer events, `Date` for the clock |
| `wasm-bindgen-futures` | `spawn_local` to drive the fetch on mount |
| `gloo-net` | `fetch` wrapper for the two API calls |
| `gloo-timers` | 1 Hz `Interval` for the taskbar clock |
| `console_error_panic_hook` | Panics in the browser show a Rust message + stack |

**Build**: `trunk` for the frontend (wasm-bindgen glue, asset pipeline, dev server
with a `/api` proxy), plain `cargo` for the backend. `[profile.wasm-release]` in
the root `Cargo.toml` is what `trunk build --release` uses.

Alternatives if you outgrow these: `yew`/`dioxus` instead of `leptos`, `actix-web`
instead of `axum`, `wgpu`/`egui` instead of DOM widgets, `leptos_axum` server
functions if you want to skip the hand-written JSON layer.

## Quick start

Two terminals, hot reload on the frontend:

```bash
# 1) backend on :8080
cargo run -p server

# 2) frontend dev server on :8081, proxying /api -> :8080
cd crates/client && trunk serve
```

Open <http://127.0.0.1:8081>.

Single port (what you would deploy):

```bash
cd crates/client && trunk build --release
cd ../.. && cargo run -p server --release     # serves crates/client/dist on :8080
```

Requires the `wasm32-unknown-unknown` target (`rustup target add
wasm32-unknown-unknown`) and `trunk` (`cargo install trunk`, or grab a release
binary).

The frontend works without the backend: it boots from
`shared::default_catalog()` and shows the backend as offline.

## How the reactivity is arranged

All window state is one root-owned `RwSignal<Vec<WinData>>`, where `WinData` is
`Copy` (scalars only). Each keyed row derives a `Memo<WinData>` for its own
window, so dragging repaints one window rather than the whole list, and app state
(terminal scrollback, current directory) survives moves and resizes because the
row is never remounted.

That shape is load-bearing, so it is worth knowing why:

- Leptos keyed `<For>` only re-renders a row when its **key** changes, so row data
  has to be read through a signal.
- The obvious alternative, a signal per item, breaks here: windows are opened
  from a click handler, and a plain `RwSignal` is owned by the reactive owner that
  was current at creation — the *menu item's* owner. Closing the start menu then
  disposes the window's geometry signal and the next read panics with
  `Tried to access a reactive value that has already been disposed`.
  `ArcRwSignal` fixes the disposal but is not `Copy`, which the row views need.
  A `Memo` per row is both `Copy` and owned by the row itself.

## Notes from building this

- `leptos` 0.8 has **no default features**. Without `features = ["csr"]` the app
  renders once and then never updates — no click handlers, no effects, no clock.
- With the `delegation` feature, listeners live on `window`, so
  `event.current_target()` is not the element you attached to. Use
  `event.target()`. The title bar's children are `pointer-events: none` so the
  target of a title-bar gesture is always the title bar.
- Drag and resize use pointer capture (`set_pointer_capture`) so the gesture keeps
  working after the pointer leaves the element; nothing is attached to `document`.
- `ServeDir::not_found_service` forces the fallback's status to 404, which breaks
  SPA history fallbacks. Use `ServeDir::fallback` for that; this app has no router,
  so plain `ServeDir` is enough.
- Trunk's `[[proxy]]` forwards *every* path unless you add `rewrite`, so the config
  here uses `backend = ".../api"` + `rewrite = "/api"`.
- `trunk serve` writes its development build into the same `dist/` directory that
  the single-port server serves, so re-run `trunk build --release` before serving
  "production" again (the dev bundle is ~7x larger).

## Not implemented

Window layout is in memory only (no persistence, no backend sync), the Files app
is a read-only fake filesystem, and there is no client-side router. Natural next
steps: persist geometry via `PUT /api/layout`, a WebSocket on a tokio `broadcast`
channel to mirror windows across tabs, `leptos_router` for in-window routing, and
`wasm-opt` tuning for bundle size.
