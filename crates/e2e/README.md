# The browser suite

Every other test in this repo answers "does it compile?" or "does the API return the
right JSON?". This one answers "does it work when a person clicks it?" — a real Firefox,
driven by Selenium, against a real server, checking what is actually on the screen and
what is actually on disk.

    cargo xtask e2e                     # build everything, then run every scenario
    cargo xtask e2e --jobs 8            # ... eight at a time, instead of the default four
    cargo xtask e2e --list              # what there is
    cargo xtask e2e --only trash,files  # a couple, while working on them
    cargo xtask e2e --no-build          # skip the Rust and trunk builds
    cargo xtask e2e --keep              # keep fixtures, screenshots and logs
    cargo xtask e2e --skip-network      # skip the probe that needs pokeapi.co

`xtask e2e` builds the server, the shell and the served apps, precompresses both, makes
a throwaway virtualenv in `target/e2e/venv`, and runs `run.py`. Everything it leaves
behind is under `target/e2e/`, so nothing here touches your checkout or your `~/web-desktop`.

## Why Python

The app is Rust all the way down; the *test driver* is not. Selenium's Python bindings
are the shortest path to a real browser, and a probe that is 40 lines of Python is a probe
someone will actually read and extend. The checks are plain `PASS`/`FAIL` lines, so a probe
can be run by hand against a server you already have up:

    PROBE_URL=http://127.0.0.1:8096 PROBE_ROOT=/tmp/trashtest \
      python3 crates/e2e/probes/probe_trash.py

## Layout

    run.py            scenarios: fixtures + servers + a probe, then teardown
    fixtures.py       the throwaway trees the probes browse (built per scenario)
    requirements.txt  selenium, pillow, brotli — pinned
    probes/           one file per area; each is standalone and prints PASS/FAIL
    serve/            stand-in servers used as fixtures: a skewed catalog, a
                      broken catalog, an offline dist, and a third-party API

Each scenario gets its own ports (handed out from 8200 at run time) and its own fixture
root under `target/e2e/work/<scenario>`, and its servers are stopped afterwards —
including on failure — so scenarios are independent and several run at once (four by
default, `--jobs` to change), and a run leaves no stray processes and no state behind
for the next one. The server is started from its scenario's work directory, where there
is no `.env`, so your local configuration cannot make the suite pass or fail.

## Scenarios

| scenario | probe | what it drives | checks |
| --- | --- | --- | --- |
| `shell` | `probe.py` | window manager: drag, 8 resize handles, maximize, z-order, taskbar, start menu and search, wallpapers | 30 |
| `files` | `probe_fs.py` | the file browser: listing, breadcrumbs, previews, hidden files, symlink fence, downloads | 40 |
| `images` | `probe_img.py` | image previews: real sizes, dimension labels, misnamed files, the size cap | 33 |
| `editing` | `probe_edit.py` | create, upload, rename, delete; the handoff to the editor | 33 |
| `editor` | `probe_editor.py` | the Text Editor: tabs, dirty state, Ctrl+S, gallery of tabs | 32 |
| `gallery` | `probe_gallery.py` | the Gallery app: albums, tiles, navigation | 20 |
| `openwith` | `probe_openwith.py` | type-based "open in": which app a file is handed to | 10 |
| `served` | `probe_served.py` | a served wasm app: manifest, hash check, version skew | 17 |
| `compression` | `probe_compression.py` | what the shell costs over the wire: `br`, `gzip`, identity | 8 |
| `paint` | `probe_paint.py` | the wallpaper actually paints (reads pixels out of a screenshot) | 9 |
| `terminal` | `probe_term.py` | no console errors from a normal session | — |
| `offline` | `probe_offline.py` | the shell boots with no backend and falls back to the built-in catalog | 6 |
| `skew` | `probe_skew.py` | a catalog naming an app the shell cannot run | 14 |
| `broken` | `probe_broken.py` | a catalog the shell cannot decode: reported, not swallowed | 3 |
| `proxy` | `probe_proxy.py` | the outbound proxy: allowed host, CORS-free fetch, refusal | 4 |
| `trash` | `probe_trash.py` | delete, undo, trash view, restore, delete forever, empty | 28 |
| `readonly` | `probe_readonly.py` | a root served with writes switched off | 9 |
| `pokedex` | `probe_pokedex.py` | the Pokédex app fetching through the proxy (needs the internet) | 11 |

## Status, honestly

The suite was rebuilt after `/tmp` was wiped by a reboot, from the session transcripts
(so the probes are the real ones, recovered rather than rewritten). Against `HEAD`:

* **Every scenario is green**: 307 checks across 18 scenarios, a couple of minutes at the
  default `--jobs 4`. `pokedex` is the only one that needs the internet; `--skip-network`
  skips it rather than failing it.
* `run.py` reports a scenario as `crashed` rather than `ok` when a probe dies part-way,
  precisely so a half-finished run cannot look like a pass.
