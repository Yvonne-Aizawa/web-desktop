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
| `PUT /api/fs/content?path=` | Write the body as the file's contents (text save or binary upload, atomically) |
| `POST /api/fs/dir` | Create a directory |
| `PATCH /api/fs/entry` | Rename an entry within its folder |
| `DELETE /api/fs/entry?path=` | Delete a file, or a directory with `&recursive=true` |

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

### Editing

Files manages the *folder* — create file or folder, rename, upload, delete — and
previews contents read-only. **Open in …** hands a file to the app that handles its
type: images to the Gallery, everything else to the Text Editor. Double-clicking a row
does the same. Handing a file over is a read, so it is offered even when the root is
read-only (the target app then turns read-only itself).

Every mutation goes through the same fence as the reads, and writes are gated on
configuration first:

| Setting | Effect |
| --- | --- |
| *(default)* | The sandbox at `~/web-desktop` is read-write |
| `WEB_DESKTOP_ROOT=/somewhere` | That root is **read-only** until you also set `WEB_DESKTOP_FS_WRITE=on` |
| `WEB_DESKTOP_FS_WRITE=on` / `off` | Force either way |

`shared::FsInfo` carries `writable` and a `readonly_reason`, so the frontend hides
its write affordances and shows *why* rather than offering a button that fails.

What the write path is careful about:

* **Creation resolves the parent.** A path that does not exist cannot be
  canonicalized, so `FsRoot::resolve_parent` proves the parent is inside the root and
  appends one validated component. Names are checked for separators, `.`/`..`, NULs
  and length before anything is touched.
* **Saves are atomic.** Content is written to a temporary file in the same directory
  and renamed over the target, so a failure halfway cannot leave a half-written file.
* **Deletes ask twice.** A non-empty directory needs `recursive=true`; the UI turns
  the Delete button into a confirmation that names what is about to go. The root
  itself cannot be deleted or renamed.
* **Read-only is enforced server-side, not in the UI.** Every handler checks it
  before looking at the request.
* **Cross-site writes were already blocked** by the `same_origin` middleware, which
  is why reads never needed it but a write path does.

Still open here: no trash or undo (a delete is a delete), no optimistic locking, so
two windows editing one file last-write-wins, and no PDF or binary in-place editing
— those are uploads.

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

### Deleting, and the trash

Delete moves things into `<root>/.trash/` — a rename, so it costs nothing and works on a
folder of any size — and the Files app offers an **Undo** straight afterwards, plus a
trash view where anything can be put back or removed for good. The one confirmation
dialog left in the file browser is emptying the trash, because that is the only step
that cannot be taken back.

What that buys, and what it costs:

* **A delete you cannot regret.** The bytes stay until you empty the trash, and restoring
  puts an entry back at its original path, in its original folder (recreating it if it
  has gone), under a new name if something has taken the old one.
* **The trash is the browser's, not yours.** It is the one folder the file API will not
  list, read or modify; it is reached through its own endpoints, so it can neither be
  deleted by accident nor get in the way of your own files. It is never listed, not even
  with hidden files shown.
* **`?permanent=true` still exists** on the delete endpoint for a script that means it;
  the desktop itself always trashes.

### Keeping something read-only

`WEB_DESKTOP_FS_READONLY=Lumina,archive` keeps those paths untouchable *inside* a
writable root: reading, previewing and copying out all work, while writing, creating,
renaming, moving and deleting are refused by the server with the rule that stopped them.
Deleting a folder that *holds* a protected path is refused too, which is the case a check
on the path alone would miss.

This is the difference between a convention and a guarantee. Telling an agent "do not
touch `Lumina`" is a convention; putting it in the config is a guarantee, and it is also
enforced against the file browser itself, which until now would happily delete it.

### The browser suite

`cargo xtask e2e` builds everything and drives a real Firefox through the app: the window
manager, the file browser, image previews, the editor, the gallery, the served apps, the
proxy, the trash and the read-only fence. It is the only test here that answers "does it
work when a person clicks it?"; see `crates/e2e/README.md` for what it covers and how to
run one scenario while working on it.

### Configuration

The server reads its settings from the environment, and loads a `.env` file first if
there is one — searched for in the working directory and its parents, so a `.env` at the
repository root is found wherever you run from. **A value already in your shell wins over
the file**, and only the path is logged, never the values, because a `.env` may hold an
API key. `.env` is gitignored; `.env.example` lists every variable with a note.

```bash
cp .env.example .env      # then edit
```

| Variable | Default | Meaning |
| --- | --- | --- |
| `WEB_DESKTOP_ADDR` | `127.0.0.1:8080` | Listen address |
| `WEB_DESKTOP_ROOT` | `~/web-desktop` | The one directory the file browser may touch |
| `WEB_DESKTOP_FS` | on | `off` disables the file API |
| `WEB_DESKTOP_FS_WRITE` | on for the sandbox, off for a chosen root | `on`/`off` for editing |
| `WEB_DESKTOP_FS_READONLY` | empty | Paths inside the root that stay read-only, e.g. `Lumina,archive` |
| `WEB_DESKTOP_PROXY_HOSTS` | empty (off) | Hosts the outbound proxy may reach |
| `WEB_DESKTOP_PROXY_ALLOW_PRIVATE` | off | Also allow private and loopback addresses |
| `WEB_DESKTOP_AI_URL` | empty (off) | An OpenAI-compatible provider the chat gateway may call |
| `WEB_DESKTOP_AI_KEY` | empty | The provider's key. Read from here, never sent to the browser |
| `WEB_DESKTOP_AI_MODEL` | empty | The model a request gets when it names none |
| `WEB_DESKTOP_AI_MODELS` | just `WEB_DESKTOP_AI_MODEL` | Models an app may ask for |
| `WEB_DESKTOP_IMAGE_URL` | empty (off) | An OpenAI-compatible image provider the image gateway may call. Separate from `WEB_DESKTOP_AI_URL` |
| `WEB_DESKTOP_IMAGE_KEY` | empty | The image provider's key. Read from here, never sent to the browser |
| `WEB_DESKTOP_IMAGE_MODEL` | empty | The image model a request gets when it names none |
| `WEB_DESKTOP_IMAGE_MODELS` | just `WEB_DESKTOP_IMAGE_MODEL` | Image models an app may ask for |
| `WEB_DESKTOP_IMAGE_SIZES` | the provider's documented set | `WxH` sizes an app may ask for |
| `WEB_DESKTOP_APPS` | `apps` | Where built app bundles live |
| `WEB_DESKTOP_DIST` | `crates/client/dist` | Where the client bundle is served from |
| `RUST_LOG` | `server=info` | Log filter |

Point the file browser at something other than the default sandbox:

```bash
WEB_DESKTOP_ROOT=~/Documents cargo run -p server          # read-only
WEB_DESKTOP_ROOT=~/Documents WEB_DESKTOP_FS_WRITE=on cargo run -p server
```

A root you choose is read-only unless you add the second variable. The sidebar says
so and explains how to change it, rather than offering buttons that would fail.

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
| first load (wasm + js, brotli) | ~2.8 MB | **~286 KB** |

wasm-opt needs *both* halves of the configuration: `[tools] wasm_opt = "version_131"`
in `Trunk.toml` only pins which binaryen to use — trunk then skips the step unless a
link asks for it, so `index.html` carries
`<link data-trunk rel="rust" data-wasm-opt="z" />`. Compression is
`CompressionLayer`, whose default predicate compresses everything except images
(SVG excepted), gRPC, SSE and bodies under 32 bytes — so the image endpoint does not
pay for it.

### Text Editor

`apps/editor.rs` is the third built-in that talks to the file API, and the most
opinionated about DOM behaviour: several files open at once, one tab each with its own
draft and dirty state, Save / Save all / Ctrl+S, a line-number gutter, a caret readout,
and a close that asks before discarding. Files hands a file to it with **Open with…**,
through a one-shot slot on `Desktop`, so the editor lands on that file whether it was
already running or not.

Two details are load-bearing:

* **The textarea is uncontrolled.** Binding `value` reactively rewrites the box on
  every keystroke, and browsers move the caret to the end when that happens. The box is
  seeded once when a tab becomes active and edits flow one way, into the tab's draft.
  The body renders from a `Memo` of `(path, name, kind)`, which does not change while
  typing, so the element is never rebuilt under the caret.
* **The tab bar reads live.** Learning this the hard way again: a keyed `For` row is not
  re-rendered when its *item* changes, so dirty markers, the closing state and the
  "unsupported" style all have to be read from the signal. Built from the row snapshot
  they simply never update.

Files that are not editable text still open — a tab that says whether it is binary or
an image, with a download link — and a truncated file (>256 KiB) opens read-only,
because saving it would drop the rest.

### Gallery

`apps/gallery.rs` shows the photos in a folder as a contact sheet, with a viewer that
steps through them (arrow keys, Escape) and reports each one's pixel size, byte size and
date. Folders are tiles too, so it doubles as a browser; hidden files are a toggle.

Nothing about it is photo-specific on the server: it lists a folder, filters to the
formats `shared::fs::image_mime` says the browser can decode, and points tiles at
`/api/fs/raw`. Files that *look* like images but are in a format no browser decodes
(TIFF, HEIC, PSD) are counted and mentioned rather than shown as broken tiles, and a tile
whose bytes fail to decode marks itself "unreadable" instead of leaving a broken icon.

One shell interaction: **Set as wallpaper** puts the photo behind the whole desktop. The
wallpaper is a CSS `background` value, so `url('/api/fs/raw?path=…')` is as valid as a
gradient — which is also why the catalog refresh no longer replaces a wallpaper it does
not recognise (it used to, and would have wiped the photo on the next reconnect).

### Letting an app fetch a third-party URL

A served app runs in the browser, so a `fetch` to a host without CORS headers is
unreadable no matter who asks. `GET /api/proxy?url=<absolute>` makes the request on the
app's behalf:

```toml
app-sdk = { workspace = true, features = ["fetch"] }
```

```rust
#[derive(serde::Deserialize)]
struct Thing { id: u32 }

let things: Vec<Thing> = app_sdk::fetch::json("https://api.example.com/things").await?;
```

`fetch` is an opt-in feature so an app that never makes a request does not carry an HTTP
client and JSON parsing in its bundle.

**The policy is the feature.** An unrestricted `?url=` is the classic SSRF hole: cloud
metadata at `169.254.169.254`, other services on loopback, everything on the LAN,
reachable from this machine. So:

| Setting | Effect |
| --- | --- |
| *(default)* | **Off.** Nothing is proxied. |
| `WEB_DESKTOP_PROXY_HOSTS=api.example.com,example.org` | Those hosts, and their subdomains. |
| `WEB_DESKTOP_PROXY_ALLOW_PRIVATE=on` | Also allow private, loopback and link-local addresses — for proxying to something of your own. |

What it refuses, and where: only `http`/`https`; the host must be on the list; the host is
**resolved and every address checked** against the private, loopback, link-local,
carrier-grade-NAT, benchmarking and reserved ranges (so an internal DNS name cannot stand
in for a private address); and **every redirect hop is checked again**, because an allowed
host can redirect anywhere. Requests are `GET` only, carry no caller headers or cookies,
and are bounded by a 2 MiB body cap, a 10s timeout and 3 redirects. The same
`Sec-Fetch-Site` guard that covers the file API covers this, so a random page cannot spend
your network position.

Not defended against, deliberately: DNS rebinding between the address check and the
connection. The server binds to loopback and the allow-list is explicit. Not included
either: POST and other methods (the shape is one `enum Request`, so widening it is
deliberate), per-host secret headers, and any caching. Settings shows whether the proxy is
on and for which hosts.

Not included *on purpose*, and the reason the next section exists: a `POST` with a body
the caller composes, and a response that streams for minutes. Both are what a chat needs
and both would blunt the checks above, so they live in a second module with a different
(and much smaller) policy problem. Settings tells the two apart — they are separate
panes, because they are separate decisions.

### Talking to a chat model

A chat app needs two things a browser cannot give it: the provider's key, which must
never be shipped to the client, and a long-lived streamed answer, which is the opposite
of what the proxy above is shaped for. So there is a second gateway, `server::ai`, and the
app talks to that:

```text
app ──POST /api/ai/chat──▶ server ──POST {WEB_DESKTOP_AI_URL}/chat/completions──▶ provider
```

`crates/apps/chat` is the app that uses it. What makes this gateway much simpler to fence
than the proxy is that **the caller never names an address**:

| Setting | Effect |
| --- | --- |
| *(default)* | **Off.** `/api/ai/info` answers with an empty model list, and the app says so. |
| `WEB_DESKTOP_AI_URL` | The provider's base URL, e.g. `https://api.openai.com/v1`. Any OpenAI-compatible server will do, including one on `127.0.0.1`. |
| `WEB_DESKTOP_AI_KEY` | The provider's key. Never logged, never sent to the browser, never in the catalog. |
| `WEB_DESKTOP_AI_MODEL` | What a request gets when it names no model. |
| `WEB_DESKTOP_AI_MODELS` | The models an app may ask for. Defaults to just the one above. |

Because the URL is configuration rather than input, there is no SSRF surface here at all:
an app cannot ask the server to talk to something the operator did not point it at, which
is also why this endpoint needs no host list and no private-address check. What a caller
*can* choose is the model, and a model is a bill, so that choice is an allow-list — a
request naming anything else is refused, and the refusal names the variable to widen.

`Sec-Fetch-Site: cross-site` is rejected, the same guard the file API and the proxy carry.
The endpoint spends the operator's key, so a random page must not be able to cause a
request — a stronger reason for the guard than the file API's reads ever had.

#### One endpoint, two shapes

`POST /api/ai/chat` takes `{messages, model?, stream?}` and answers one of the two ways
the OpenAI contract allows — which is why the `shared::ai` types are shaped the way they
are, and what the app would be reading if it streamed:

* **`stream: false`** — one `ChatResponse`: which model answered, the assistant's message,
  and the token usage when the provider reports it. The whole answer, in one piece.
* **`stream: true`** — `text/event-stream`, the provider's own frames relayed as they
  arrive and ending in `data: [DONE]`. Deliberately a relay rather than a re-encoding: a
  field the provider adds is not silently dropped on the way through, and any OpenAI
  client could be pointed at this endpoint unchanged. This is what the Chat app uses.

The details that make streaming *work* rather than merely compile:

* **A failure before the first frame is JSON.** The upstream status is checked and a
  non-2xx body is read (capped) and turned into the shell's error shape **before** any of
  it is streamed, so a caller can never be halfway through a stream when bad news arrives.
* **No whole-request timeout on a stream.** A deadline that is right for a 10-second fetch
  would cut a long answer off mid-sentence, so a connect timeout and an idle timeout
  *between frames* are what apply instead.
* **`Cache-Control: no-store` and `X-Accel-Buffering: no`.** Anything in front of the
  server that decides to buffer would quietly turn streaming back into waiting.
* **Dropping the stream is Stop.** When the client goes away, axum drops the body, the
  upstream response goes with it, and the provider connection closes — so a cancelled
  answer stops costing tokens with no cancellation plumbing. A semaphore bounds how many
  answers are in flight, and the permit rides along inside the stream so that stays true
  for the whole of one.

A provider failure is remapped rather than passed through: a 401 from the provider becomes
this server's **502**, naming `WEB_DESKTOP_AI_KEY`, because it is the operator's
misconfiguration and telling the caller 401 would send them looking for a credential they
do not have. The provider's own words are kept, since they are what names the fix.

Verified against a stand-in provider rather than a real one — a real answer needs a key
and a network, and neither belongs in a test. A finished answer becomes a `ChatResponse`;
a streamed one arrives frame by frame with the provider's own `[DONE]`; a provider 401
reads as 502 with the provider's message; a model outside the list is refused *before*
anything is sent; and a gateway with no URL explains itself instead of 404ing.

The "dropping the stream is Stop" claim was checked against a provider that reports how
much it managed to write: pointing a client at a 40-frame stream and hanging up after
three frames left the provider with a broken pipe after four. Had the server buffered the
answer, the provider would have written all forty before anyone could hang up.

Still open: nothing caps what one answer may cost (no `max_tokens` is sent, because the
field name differs between providers and getting it wrong fails every request), and there
is no per-caller rate limit beyond the in-flight cap.

#### The Chat app

`crates/apps/chat` is a served app, and the first one that writes: each conversation is
one JSON document under `chats/` in the same sandbox Files browses. Nothing new was
needed on the server for that — `app-sdk`'s `fs` feature is a thin wrapper over the file
API that already existed — and it inherits what that API already promises: rename it,
copy it, open it in the Text Editor, and if you delete it, put it back from the trash. A
read-only root arrives as a sentence in the footer rather than a crash, and the chat in
memory is still there.

Two details are load-bearing: the first is the editor's lesson again, and the second is
new here.

* **The textarea is uncontrolled, as in the editor.** A `has_text` signal is kept in step
  by `on:input`, but it is never bound back to `value`: that rewrites the box on every
  keystroke, and browsers move the caret to the end when they do.
* **The window's state is one `Copy` struct of signals, with no `Rc` anywhere.** The same
  `Rc<dyn Fn(..)>` that is fine captured in an `on:click` handler is refused in a
  `{move || …}` child, which must be `Send + Sync` — and the error names the `Rc`, not the
  closure that captured it. Signals are `Copy` and `Send`, so a bundle of them is both,
  and the actions (`send`, `open_saved`, `remove_saved`) are plain functions taking one.

Answers stream, because the server has the other half of it. The app turns on `ai-stream`
and `chat_stream` hands each piece to a closure that appends it to a `streaming` signal,
rendered as one more bubble until the answer is complete and becomes a message. Stop
aborts an `AbortController` rather than setting a flag the read loop would only notice at
the next word: a read that has not resolved yet cannot see a flag, so a model thinking for
five seconds would look like a dead button. The controller is a JS value, which means it
cannot go in a signal — it is kept in a `StoredValue<_, LocalStorage>`, whose handle is
`Copy` and `Send` even though the value inside is neither, and which is the reason the
`State` bundle can list it beside the signals.

The SSE reassembly is the one piece with real edge cases, and it is unit-tested on the
host rather than in a browser (`cargo test -p app-sdk --features ai-stream`): an event
split across chunks, several events in one chunk, `\r\n` line endings, a comment-only
keep-alive, `data:` with no space, and a multi-byte character whose bytes land on either
side of a chunk boundary. That last one is why the buffer is a `Vec<u8>` and the split is
on bytes: no UTF-8 continuation byte is `\n`, so slicing there can never cut a character
in half, which a chunk-at-a-time `from_utf8` would.

### Making pictures

The chat gateway's argument again, and a deliberately separate module rather than a mode
of it: an image provider is a different service with a different bill, its request and
response are their own shape, and an operator may well want one provider for words and
another for pictures. So `server::images` has its own configuration, its own allow-lists
and its own concurrency cap:

```text
app ──POST /api/images/generate──▶ server ──POST {WEB_DESKTOP_IMAGE_URL}/images/generations──▶ provider
```

| Setting | Effect |
| --- | --- |
| *(default)* | **Off.** `/api/images/info` answers with an empty model list, and the app says so. |
| `WEB_DESKTOP_IMAGE_URL` | The provider's base URL, e.g. `https://api.melious.ai/v1`. Any OpenAI-compatible image server will do. |
| `WEB_DESKTOP_IMAGE_KEY` | The provider's key. Never logged, never sent to the browser. |
| `WEB_DESKTOP_IMAGE_MODEL` | What a request gets when it names no model. The provider requires a model, so this is the one that matters. |
| `WEB_DESKTOP_IMAGE_MODELS` | The models an app may ask for. Defaults to just the one above. |
| `WEB_DESKTOP_IMAGE_SIZES` | The `WxH` sizes an app may ask for. Defaults to the provider's documented set. |

The fence is the chat gateway's, for the same reason — the caller never names an address
— with three additions specific to a picture: **the size is an allow-list too** (a larger
image is a larger bill), **the count is capped** at what the provider itself accepts, and an
img2img request's **reference images are bounded** — how many, how big each, how big in
total, that each is really base64, and that the bytes behind it really start like a PNG, a
JPEG or a WebP. That last one is the only place a caller supplies file *content* rather
than a name, so it is checked rather than trusted — and it is also where the shape is
settled: the provider wants a `data:` URI and the server writes the type from those bytes,
not from whatever a caller or a file extension claimed. The response ceiling is far higher
than a chat's, because one base64 PNG is hundreds of kilobytes and a batch is megabytes,
and the request ceiling is raised past axum's 2 MiB default for the same reason.

The one thing the server adds on the way through is a **sniffed MIME type**. The provider
returns `b64_json` and never says which format it produced — FLUX hands back JPEG unless
asked otherwise, other models hand back whatever their runtime emits — so the server reads
the file signature from the base64 *prefix* (base64 encodes three bytes at a time, so the
encoding of the first bytes is a prefix of the whole encoding) and reports `image/png`,
`image/jpeg`, `image/gif` or `image/webp`. That is what lets the app name the file
`ship.png`, and lets Files and the Gallery treat the result as the picture it is. An
unrecognised signature becomes `application/octet-stream`: saved and linked, but not drawn
as a broken tile.

#### The Images app

`crates/apps/images` is a served app, and it writes: every generated image is saved under
`images/` in the same sandbox Files browses, named from the prompt with a timestamp so it
sorts and the Gallery can show it. The batch stays on screen whether or not the write
succeeds — the picture was already paid for — and a read-only root shows up as
`not saved: …` on the card rather than a crash. The sidebar is a view of `images/`, so a
past generation is one click away, and removing one is the ordinary trash, so Files can put
it back. The app offers the model, size, count and quality the server allows, and remembers
what was chosen in `localStorage`.

**Image-to-image is the same app with a source attached.** *Use as source* in the viewer
adds the picture on screen to a strip above the composer, and *+ source* opens a small
browser over the sandbox — folders and the PNG/JPEG/WebP files in them — so a source can be
anything in the user's files, not only what this app made. Reading one is `fs::read_bytes`
and `image::encode_base64`, both of which are the browser's own `atob`/`btoa`; the sources
are then sent as `input_images` and stay attached, so iterating on a picture is a prompt
change and another click. The advanced row carries the other additions the provider
documents: a negative prompt, `steps` and `guidance`, all read uncontrolled at Generate
and all optional.

The remaining model-specific fields (`width`/`height` as free sizes, `output_quality`, and
the `lora_weights`/`lora_scale` pair) are the same kind of addition: the server builds its
provider body field by field, so each is one more explicit option with its own bound rather
than a passthrough map.

### Sample apps

Four apps live in the repository, built with `cargo xtask build-app`:

| App | Shows |
| --- | --- |
| `crates/apps/calculator` | A four-function calculator: signals, the host object, its own styles. |
| `crates/apps/pokedex` | A random Pokémon from <https://pokeapi.co>: the proxy, and the SDK's `fetch`. |
| `crates/apps/chat` | A conversation with a model the server talks to: the AI gateway, and the SDK's `ai` and `fs`. |
| `crates/apps/images` | A generated picture the server fetches: the image gateway, and the SDK's `image` and `fs`. |

The Pokédex is the one that exercises the network, and it needs the proxy to be told
about the host:

```bash
WEB_DESKTOP_PROXY_HOSTS=pokeapi.co cargo run -p server --release
```

It is a good illustration of the two halves of the rule: **reading** a response needs
permission, so the JSON goes through `/api/proxy`, while the **sprite** is an `<img>` —
an image needs no CORS at all, so it loads straight from the sprite CDN. With the proxy
off, the app says so and names the variable to set, because the proxy's refusals carry
the reason.

### How one app opens another's file

`Desktop::request_open(app, path)` puts an `OpenRequest` in a one-shot slot and raises
that app. Each app that accepts files watches the slot for requests addressed to it —
`take_open_request(AppId::Editor)` — so an app takes only its own and never has to know
anything about the caller. The type decides the app, and the *caller* decides the type:
Files routes with `looks_like_image`, which is why a TIFF goes to the Gallery (the app
that can explain why the browser will not draw it) rather than to the editor as
"binary". The shell keeps no table of who opens what.

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
export needs its crate in scope. The SDK covers the shell, one opt-in feature at a time
(`fetch`, `ai`, `ai-stream`, `fs`, so an app that never makes a request does not carry an
HTTP client, and one that waits for a finished answer does not carry a stream reader);
anything past that is the app's own business — `serde_json` for a document it keeps, or
`web-sys` for a DOM element it has to read by hand, as `crates/apps/chat` does for its
textarea.

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

* **No trash or undo.** A delete is a delete, and an overwrite is an overwrite.
* **No optimistic locking.** Two windows editing one file is last-write-wins; the
  editor does not notice that the file changed underneath it.
* **No cross-folder move.** Rename stays in the same directory, since the listing
  has no drag and drop.
* **No permissions or ownership UI.** Files are created with the server user's
  umask, and a replaced file takes the new default mode rather than keeping the old.
* **Window layout is in memory only** (no persistence, no backend sync), and there
  is no client-side router.
* **No markdown in Chat.** An answer is shown as plain text with its newlines kept, so a
  code block survives only in the sense that the model indented it. Rendering it would
  mean trusting model output as markup, which is its own decision.
* **No retry after a broken stream.** Text that arrived before a failure is kept and the
  failure is reported next to it, but the app does not try to pick the answer up where it
  stopped — the provider has no way to resume one.
* **No per-model control surface.** Images sends the fields the provider documents as
  common to its img2img models — a negative prompt, `steps`, `guidance` — and none of the
  model-specific rest (`width`/`height` free sizes, `output_quality`, LoRAs). A model that
  wants those is used with its own defaults.

Natural next steps: persist layout with `PUT /api/layout`, detect external edits by
comparing mtime before saving, a WebSocket on a tokio `broadcast` channel to mirror
windows across tabs, `leptos_router` for in-window routing, and server-side
thumbnailing with ETags.
