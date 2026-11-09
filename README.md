# web_desktop

A small browser window manager, written in Rust end to end: `axum` on the server,
`leptos` compiled to WebAssembly in the browser, and one `shared` crate so the
wire types are defined once.

Draggable and resizable windows with 8 resize handles, z-order focus, double-click
to maximize, a taskbar with per-window buttons, a start menu with search, six
wallpapers, a clock, and four built-in apps (Terminal, Files, Settings, About).
The Files app browses a **real folder on the machine running the backend**, fenced
inside a single sandbox directory and read-only.

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
| `tokio-util` | `ReaderStream`, so images and downloads stream instead of buffering |
| `tracing` + `tracing-subscriber` | Structured logs with `RUST_LOG` support |
| `serde` / `serde_json` | Wire format, shared with the frontend |

**Frontend**

| Crate | Why |
| --- | --- |
| `leptos` (0.8, `csr` + `delegation`) | Signals, `view!` macro, WASM CSR |
| `wasm-bindgen` / `web-sys` / `js-sys` | DOM, pointer events, `Date` for the clock |
| `sha2` | Verifying a served app's wasm before instantiating it |
| `wasm-bindgen-futures` | `spawn_local` to drive the fetch on mount |
| `gloo-net` | `fetch` wrapper for the two API calls |
| `gloo-timers` | 1 Hz `Interval` for the taskbar clock |
| `console_error_panic_hook` | Panics in the browser show a Rust message + stack |

**Build**: `trunk` for the frontend (wasm-bindgen glue, asset pipeline, dev server
with a `/api` proxy, wasm-opt, precompression hook), plain `cargo` for the backend,
and an `xtask` crate (`cargo xtask precompress | build-app`) for the rest.
`[profile.wasm-release]` in the root `Cargo.toml` is what release builds use, for the
shell and for served apps alike.

Alternatives if you outgrow these: `yew`/`dioxus` instead of `leptos`, `actix-web`
instead of `axum`, `wgpu`/`egui` instead of DOM widgets, `leptos_axum` server
functions if you want to skip the hand-written JSON layer.

## The file browser

The Files app is a view of one directory on the server. Nothing runs in the
browser's own filesystem sandbox and nothing reaches outside that root.

| Endpoint | Returns |
| --- | --- |
| `GET /api/fs/list?path=` | [`DirListing`](crates/shared/src/fs.rs): entries, kinds, sizes, mtimes |
| `GET /api/fs/preview?path=` | A text body (capped at 256 KiB), an image marker, or a binary notice with a guessed MIME type |
| `GET /api/fs/raw?path=` | Image bytes, inline, for an `<img src>` |
| `GET /api/fs/download?path=` | The raw bytes, capped at 64 MiB, as an attachment |

### Image previews

`/api/fs/raw` exists so the preview pane and the row thumbnails can point an
`<img>` at a file. `shared::fs::image_mime` is the one list of formats that get
that treatment — PNG, JPEG, GIF, WebP, BMP, ICO, AVIF and SVG — and it is used by
both sides: the server picks `PreviewBody::Image` and the response `Content-Type`
from it, the client decides which rows get a thumbnail from it.

Two things are worth knowing about the shape of this:

* **`preview` does not read images.** It returns `{"type": "image", "mime": ...}`
  and the bytes come from the second request, so asking about a 20 MB photo costs
  nothing. Because the two must agree, `preview` applies the same 64 MiB cap that
  `/raw` would, and reports the limit instead of handing back an `<img>` that can
  only fail.
* **Formats browsers cannot decode are not offered.** TIFF, HEIC and PSD are absent
  from `image_mime`, so they arrive as `PreviewBody::Binary`, keep the picture icon
  (`looks_like_image` answers "is this an image", which is a different question),
  and the pane says `Binary file (image/tiff, 300 kB)` rather than showing a broken
  image. A *misnamed* file — text with a `.png` name — does render, fails to
  decode, and says so; the browser is the decoder of last resort, and that state is
  reported honestly rather than hidden.

Row thumbnails are the whole file behind `loading="lazy"`, and only for images up
to 12 MiB: there is no server-side decoding, so a 40 MB photo would be pulled down
to fill a 20px box. Above that the row keeps its icon and the preview pane still
shows the file. Real thumbnailing would mean decoding server-side (`image` crate)
and caching; that is the natural next step.

### What `/api/fs/raw` is careful about

Serving files inline is how a file browser turns into an XSS vector, so this
endpoint is deliberately narrow and explicit:

* **Images only.** A path whose extension is not in `image_mime` is refused with a
  message, so the endpoint can never be used to hand out arbitrary content under a
  chosen `Content-Type`. Anything else is still reachable through `/download`,
  which always sends `application/octet-stream` as an attachment.
* **`X-Content-Type-Options: nosniff`**, so a misnamed file fails to render instead
  of being reinterpreted as something else.
* **SVG gets `Content-Security-Policy: default-src 'none'; style-src
  'unsafe-inline'; sandbox`.** An SVG is a document; if it is ever opened directly
  rather than through an `<img>`, it must not be able to run script or fetch
  anything. Rendering inside an `<img>` is unaffected (verified).
* **`Cache-Control: no-store`**, so an edit on disk is not shadowed by a stale
  thumbnail. ETags would be the better answer and are not implemented.

Paths on the wire are always root-relative and `/`-separated, with `""` meaning
the root, so an absolute path never appears in a request.

| Setting | Default | Meaning |
| --- | --- | --- |
| `WEB_DESKTOP_ROOT` | `~/web-desktop` | The only directory the API will read |
| `WEB_DESKTOP_FS` | on | `off` disables the API; the app then says so |

The sandbox is created on first run with a few sample files (a README, `notes/`,
and one dotfile) and is safe to empty out or delete.

### What the fence does

`FsRoot::resolve` is the only place client input becomes a filesystem path. It
rejects `..` and NUL outright, then **canonicalizes** the joined path and requires
it to still be inside the root. Because canonicalization resolves symlinks, a link
pointing at `/etc` fails the same check as `../../etc`, and it is listed with a
`blocked` flag so the UI can explain itself instead of offering a dead end.

Verified against: `..`, `../../etc/passwd`, `testdir/../../etc/passwd`,
percent-encoded and double-encoded traversal, absolute paths, `..%00`, a symlink
to `/etc`, a path *through* that symlink, and the same tricks against preview,
raw and download. All fail closed.

The `/api/fs` routes also reject requests whose `Sec-Fetch-Site` is `cross-site`.
Reads were never readable cross-origin (no CORS headers) but they did still
execute, and that matters as soon as writes exist.

### Adding writes

Writes are deliberately not implemented. The shape is ready for them:

* `shared::FsInfo` advertises `writable: false`, so the frontend hides mutation UI
  rather than probing for it.
* `same_origin` is applied as router middleware to everything under `/api/fs`.
* `FsRoot::resolve_parent` is the fence for paths that do not exist yet, which is
  the piece creation needs (a missing path cannot be canonicalized; its parent can).

The endpoints to add would be `POST /api/fs/dir`, `PATCH /api/fs/entry`,
`DELETE /api/fs/entry` and `PUT /api/fs/file`, all going through the same two
functions.

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
cd crates/client && trunk build --release     # precompresses via a post_build hook
cd ../.. && cargo run -p server --release     # serves crates/client/dist on :8080
```

Building an app to serve (see *Served apps*):

```bash
cargo xtask build-app crates/apps/calculator  # stages apps/calculator/<version>/
cargo xtask precompress apps                  # if the browser is to get it compressed
```

Point the file browser at something other than the default sandbox:

```bash
WEB_DESKTOP_ROOT=~/Documents cargo run -p server
```

Requires the `wasm32-unknown-unknown` target (`rustup target add
wasm32-unknown-unknown`) and `trunk` (`cargo install trunk`, or grab a release
binary).

The frontend works without the backend: it boots from
`shared::default_catalog()` and shows the backend as offline.

## Adding a program

Two ways: compile it in (below), or serve it as a bundle (see *Served apps*). A
compiled-in program is a compile-time variant, so adding one is five edits and the
compiler names every one of them:

1. `crates/shared/src/lib.rs`: add an `AppId` variant, extend `AppId::ALL` (the
   array *length* is part of the type), and fill in the six `const fn`s — `slug`,
   `name`, `glyph`, `accent`, `blurb`, `default_size`.
2. `crates/client/src/apps/<name>.rs`: an ordinary `#[component]`.
3. `crates/client/src/apps/mod.rs`: `mod`/`pub use`, plus a match arm in `AppBody`.
4. Rebuild. The server picks the app up from `AppManifest::from(AppId)`, which
   `default_catalog()` derives from `AppId::ALL`.

Adding a variant produces exactly seven errors — the six const fns and the
`AppBody` arm — so a half-added app cannot compile, and no other file needs
touching. Everything generic comes for free:

| Wired up automatically | Because |
| --- | --- |
| Window, drag, 8 resize handles, z-order, minimize/maximize | `window.rs` only knows `WinData` |
| Taskbar button, title, accent stripe | labels are resolved from the app registry |
| Start-menu entry and search | the menu iterates `Desktop::catalog` |
| About's launcher grid | same catalog |
| `open <slug>` in the terminal | `Desktop::open_slug` |
| The server's `/api/catalog` | `AppManifest::from(AppId)` |

Two constraints are worth knowing before writing one:

* **Per-app state lives in the component**, never in the window record. `WinData`
  is `Copy` scalars on purpose (one signal for all windows is what makes a drag
  repaint a single window), so an app owns its own signals — the terminal's
  scrollback, the file browser's `cwd`. State therefore survives moves and resizes,
  because a row is never remounted. Read geometry through the row's `Memo<WinData>`
  and write it with `Desktop::edit`.
* **One window per app.** `Desktop::open` dedupes on the app handle and raises the
  existing window.

## Catalog tolerance and sizes

Before the app tier could exist, the shell had to survive being handed a catalog it
cannot fully run, and the bundle had to get small enough that a second wasm module
per app was not absurd. This part is in place; *Served apps* below is what it was
for.

**The catalog is data, the programs are code.** `AppManifest::id` is a slug
(`String`), not an `AppId`. Before this, a server advertising one app from a newer
build made `serde` reject the *whole* catalog: the desktop silently fell back to
its four built-ins with no explanation, because a successful `/api/health` then
overwrote the failed-catalog status. Now the entry survives, `builtin_id()` returns
`None`, and opening it shows a pane that names the slug and says the shell and the
server are out of step. A separate `catalog_error` signal means a failed catalog is
reported instead of masked.

**Windows hold an `AppRef`, not an app.** `AppRef` is a `u32` index into an
append-only registry. That keeps `WinData` `Copy` — served apps will carry owned
metadata (entry URL, hash), so the window record can only hold a handle — and
because the registry is append-only and interned by slug, a catalog refresh can add
or relabel apps without a window's handle silently pointing somewhere else.
Labels, glyphs, accents and default sizes now come from the registry, so a server
can rename or recolor an app the shell implements (verified: a 520x360 window in
the catalog's accent colour).

### What the shell costs

Measured on this build, because it decides how much room the app tier has:

| | before | now |
| --- | --- | --- |
| `client_bg.wasm` raw | 2,766,794 | 805,763 (wasm-opt `-Oz`) |
| …over gzip | — | 284,965 |
| …over brotli | — | 262,680 |
| first load (wasm + js, brotli) | ~2.8 MB | **~264 KB** |

wasm-opt needs *both* halves of the configuration: `[tools] wasm_opt = "version_131"`
in `Trunk.toml` only pins which binaryen to use — trunk then skips the step unless a
link asks for it, so `index.html` carries
`<link data-trunk rel="rust" data-wasm-opt="z" />`. Compression is
`CompressionLayer`, whose default predicate compresses everything except images
(SVG excepted), gRPC, SSE and bodies under 32 bytes — so the image endpoint does not
pay for it.

## Served apps

The alternative to compiling a program in: the server hands over a bundle and the
shell loads it at runtime.

```text
apps/<id>/<version>/manifest.json   metadata, hash, size, ABI (written by xtask)
apps/<id>/<version>/app.js          ES module implementing the mount contract
apps/<id>/<version>/app_bg.wasm     the program
```

`GET /api/catalog` merges these into the compiled-in apps, so the start menu, the
taskbar and the window chrome treat them identically — a served app's label, glyph,
accent and default size all come from its manifest.

### The contract

Deliberately at the JS boundary rather than in wasm imports:

```text
init(wasm)                instantiate (generated by wasm-bindgen)
mount(el, host) -> id     render into `el`
unmount(id)               tear down
```

`host` is a plain object — `appId`, `windowId`, `size`, `theme`, `log`, `openApp`,
`close` — not a set of link-time imports. Two reasons: it avoids wasm-bindgen's
`snippets/` plumbing for host-provided JS, and it is the shape a sandboxed
(message-passing) implementation needs, so that path stays open.

### Writing one

```bash
cargo xtask build-app crates/apps/calculator    # build, wasm-bindgen, wasm-opt, hash, stage
```

An app crate declares itself in `app.json` and writes one function:

```rust
use app_sdk::prelude::*;

app_sdk::app! {
    fn my_app(ctx: Context) -> impl IntoView {
        let theme = ctx.theme();
        view! { <button on:click=move |_| ctx.open_app("files")>"open Files"</button> }
    }
}
```

It needs `crate-type = ["cdylib"]`, `app-sdk`, and — unavoidably — `leptos` and
`wasm-bindgen`: `view!` expands to absolute `leptos::` paths, and the `#[wasm_bindgen]`
export needs its crate in scope. The SDK covers everything else.

**An app must not enable `leptos/delegation`.** The shell enables it, which means the
shell has a `click` listener on `window` that dispatches by looking for a `$$$click`
*property* on each node from the event target upwards. That property is an ordinary JS
function, so a second wasm instance using the same property name is found by the
shell's walk and vice versa — every handler in the document fires twice. The symptom
is not a crash but a strange one: a click toggles twice, so the start menu opens and
immediately closes. `app-sdk` depends on `leptos` without `delegation` for this reason.

### What the shell checks first

* **ABI**: `manifest.abi` must equal `shared::APP_ABI`, otherwise the window says the
  app needs a newer shell. This is the version negotiation.
* **Size**: the manifest's `size` is refused above 16 MiB before anything is fetched.
* **Checksum**: the wasm is sha256'd with `sha2` in Rust and compared with the
  manifest before instantiation — and the *verified bytes* are what get instantiated,
  so there is no window between checking and running.

### Constraints to know

* **One instance, many windows.** A module is cached by `id@version` and `mount` is
  called once per window, so two Calculator windows share code but not state.
* **Each app is its own reactive world.** A separate wasm instance has its own memory
  and statics; an app cannot see the shell's signals. Cross-app effects go through the
  host object or the server.
* **Apps are trusted code.** The module is imported from our own origin, so it runs
  with full same-origin privileges, including the file API. Installing an app is
  trusting it. Untrusted apps would need a separate origin and a sandboxed frame, which
  is why the contract is shaped so that is a loader swap rather than an SDK rewrite.
* **A version directory is mutable during development**, so app bundles revalidate
  rather than being cached forever (the shell's content-hashed assets are immutable).
  Bump `version` in `app.json` to publish; the URL changes and the old copy ages out.
* **Precompressed siblings are build output.** `ServeDir` prefers a `.br` when the
  browser asks for one, so a bundle rebuilt without regenerating them would serve stale
  bytes. `trunk`'s `post_build` hook runs `cargo xtask precompress` for the shell, and
  the same command with `apps` covers bundles; `build-app` stages a fresh directory.
  Integrity checking does not save you here, because a stale sibling is self-consistent.

### Still to come

Per-window `setTitle` and resize notifications (both need a slot in `WinData`, which is
`Copy` on purpose), a sandboxed origin for untrusted apps, storage quotas, an install
or uninstall UI, and — if non-Rust apps matter — writing the ABI down in WIT and
transpiling components with `jco`.

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
- The server canonicalizes paths, so a folder opened *through* a symlink reports
  its real location. The Files app adopts that path, otherwise its path bar and Up
  button would describe a different directory than the list below them.
- `/api/fs` returns JSON errors (`{"error": "..."}`) with real status codes, and a
  file used as a directory maps to 400 rather than leaking a 500.
- The two bootstrap requests fail independently, so their status must not share one
  slot: a healthy backend used to hide a rejected catalog entirely.
- `Trunk.toml` alone does not enable wasm-opt — see the sizes section.
- `trunk serve` writes its development build into the same `dist/` directory that
  the single-port server serves, so re-run `trunk build --release` before serving
  "production" again (the dev bundle is ~7x larger).

## Not implemented

Window layout is in memory only (no persistence, no backend sync), the file
browser is read-only and there is no client-side router. Natural next steps: persist
geometry via `PUT /api/layout`, the file write endpoints above, a WebSocket on a
tokio `broadcast` channel to mirror windows across tabs, `leptos_router` for
in-window routing, real server-side thumbnailing with ETags, and precompressed
static assets so brotli is paid at build time rather than per request.
