//! Drawing something, with an image model the server talks to.
//!
//! The third served app to use a gateway, and the second to keep its results as files:
//!
//! * **The key and the address are the server's.** The app posts a prompt to
//!   `/api/images/generate`; the server holds the provider's key, calls it, and returns
//!   base64. Which models and which sizes the app may ask for is the server's list, not
//!   its own — both are a bill, so both are an allow-list.
//! * **A generated image is a file.** Each one is written under `images/` in the same
//!   sandbox the Files app browses, named from the prompt with a timestamp so it sorts.
//!   That is what makes the Gallery able to show it, Files able to rename or download
//!   it, and the trash able to bring it back.
//! * **An image can be the start of the next one.** A source picked from the sandbox is
//!   read back, base64'd and sent as `input_images`, which is what makes this img2img
//!   rather than text-to-image. The sidebar doubles as the picker, so a source can be one
//!   of the pictures already here or anything else the user has put in their files.
//!
//! Two details carry over from the Chat app and are worth not rediscovering:
//!
//! * **The prompt box is uncontrolled.** Binding `value` reactively rewrites the box on
//!   every keystroke and browsers move the caret to the end when they do. `has_text`
//!   tracks the button's look; the textarea's own value is the truth.
//! * **The window's state is one `Copy` struct of signals, with no `Rc` anywhere.** The
//!   same `Rc<dyn Fn(..)>` that is fine in an `on:click` is refused in a `{move || …}`
//!   child, which must be `Send + Sync`; signals are `Copy` and `Send`, so a bundle of
//!   them is both, and the actions are plain functions taking one.

use app_sdk::fs::{self, EntryKind, image_mime};
use app_sdk::image::{ImageInfo, ImageRequest, MAX_INPUT_IMAGES, accepts_input_mime};
use app_sdk::prelude::*;
use leptos::html::{Input, Textarea};
use leptos::task::spawn_local;
use wasm_bindgen::JsValue;

/// Where generated images are kept, root-relative like every other path in the app.
const DIR: &str = "images";

/// How many saved images the sidebar reads and shows.
const LIST_LIMIT: usize = 60;

/// Characters of the prompt that become a filename.
const STEM_CHARS: usize = 40;

/// Counts the app offers. The server accepts up to `MAX_COUNT`; a bigger batch is a
/// bigger bill, so the button does not invite one.
const COUNTS: [u32; 4] = [1, 2, 3, 4];

/// One generated image, and what became of it.
#[derive(Debug, Clone, PartialEq)]
struct Shot {
    /// A `data:` URL, so the picture is on screen before — and whether or not — it
    /// reaches disk. The only copy outside the sandbox.
    data_url: String,
    /// Whether a browser can draw [`Self::data_url`]; an unrecognised format is saved
    /// and linked, but not shown as a broken tile.
    previewable: bool,
    /// Where it was saved, when the write succeeded.
    saved: Option<String>,
    /// Why it could not be saved: a read-only root, or a name the fence refused.
    error: Option<String>,
    /// Some models rewrite the prompt; the revised version is worth showing.
    revised_prompt: Option<String>,
}

/// One row of the sidebar.
#[derive(Debug, Clone, PartialEq)]
struct Kept {
    /// Root-relative path, which is also how it is opened, downloaded and removed.
    path: String,
    /// The file name, for a title and the download attribute.
    name: String,
}

/// A reference image for img2img, already read and encoded.
#[derive(Debug, Clone, PartialEq)]
struct Source {
    path: String,
    name: String,
    /// Base64 of the file's bytes, in the shape the gateway wants.
    b64: String,
}

/// One row of the sidebar while it is browsing for a source.
#[derive(Debug, Clone, PartialEq)]
struct Pick {
    name: String,
    /// Root-relative path, which is also what opening it navigates to.
    path: String,
    dir: bool,
}

/// What the app is doing right now.
#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Idle,
    /// A request is out. The window stays usable; Generate does not fire twice.
    Waiting,
    /// The last request failed, with the server's own words.
    Failed(String),
}

/// The window's state in one copyable handle.
#[derive(Clone, Copy)]
struct State {
    info: RwSignal<ImageInfo>,
    model: RwSignal<String>,
    size: RwSignal<String>,
    count: RwSignal<u32>,
    /// `""` means "whatever the model does by default".
    quality: RwSignal<String>,
    phase: RwSignal<Phase>,
    /// The batch on screen.
    shots: RwSignal<Vec<Shot>>,
    /// What is already in `images/`, newest first.
    kept: RwSignal<Vec<Kept>>,
    /// A saved image being looked at, which replaces the batch on screen.
    viewing: RwSignal<Option<Kept>>,
    /// Reference images for the next request, in the order they were picked.
    sources: RwSignal<Vec<Source>>,
    /// True while the sidebar is picking a source rather than listing `images/`.
    browsing: RwSignal<bool>,
    /// The directory the picker is showing, root-relative; `""` is the sandbox root.
    dir: RwSignal<String>,
    /// What that directory holds, folders first.
    entries: RwSignal<Vec<Pick>>,
    /// One line of "that did not work", from the server or from the file API.
    notice: RwSignal<Option<String>>,
    credits: RwSignal<Option<f64>>,
    /// Only drives the Generate button's look. The textarea's own value is the truth.
    has_text: RwSignal<bool>,
}

/// The prompt, reduced to something a filename can hold.
///
/// Anything that is not a letter or a digit becomes a dash, so the result can never
/// contain a separator the file API would refuse; letters outside ASCII are kept,
/// because a prompt in another alphabet deserves a filename in it.
fn stem(prompt: &str) -> String {
    let mut out = String::new();
    let mut dash = true; // no leading dash
    for ch in prompt.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
        if out.chars().count() >= STEM_CHARS {
            break;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "image".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `2026-02-14-093012`, from milliseconds: the sort key at the front of every name, so a
/// folder of generations is also a history.
fn stamp(milliseconds: f64) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(milliseconds));
    format!(
        "{:04}-{:02}-{:02}-{:02}{:02}{:02}",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date(),
        date.get_hours(),
        date.get_minutes(),
        date.get_seconds(),
    )
}

/// Every picture in `images/`, newest first.
///
/// A name whose extension is not a format a browser decodes is skipped rather than shown
/// as a broken tile, which is the same rule the Gallery follows.
async fn load_kept() -> Result<Vec<Kept>, String> {
    let listing = fs::list(DIR).await?;

    let mut names: Vec<String> = listing
        .entries
        .into_iter()
        .filter(|entry| entry.kind == EntryKind::File && image_mime(&entry.name).is_some())
        .map(|entry| entry.name)
        .collect();
    // The name starts with a timestamp, so name order is time order.
    names.sort();
    names.reverse();
    names.truncate(LIST_LIMIT);

    Ok(names
        .into_iter()
        .map(|name| Kept {
            path: format!("{DIR}/{name}"),
            name,
        })
        .collect())
}

/// Put the folder's contents back into the sidebar, best-effort.
async fn refresh(state: State) {
    match load_kept().await {
        Ok(found) => state.kept.set(found),
        Err(err) => state.notice.set(Some(format!("cannot read {DIR}/: {err}"))),
    }
}

/// Ask for a batch, show it, and write each image to the sandbox.
///
/// The write is best-effort and per image: the picture is already the user's, and a
/// read-only root has to show up in the footer rather than swallow it. A batch that
/// cannot be saved still shows every image, because they were still paid for.
///
/// The three `NodeRef`s are the advanced inputs, read here rather than bound to signals
/// so typing in them cannot move the caret; they are optional, and an empty one means the
/// model's own default.
fn generate(
    state: State,
    ctx: Context,
    composer: NodeRef<Textarea>,
    negative: NodeRef<Input>,
    steps: NodeRef<Input>,
    guidance: NodeRef<Input>,
) {
    if state.phase.get_untracked() == Phase::Waiting || !state.info.get_untracked().enabled() {
        return;
    }
    let asked = composer
        .get()
        .map(|area| area.value())
        .unwrap_or_default()
        .trim()
        .to_owned();
    if asked.is_empty() {
        return;
    }

    if let Some(area) = composer.get() {
        area.set_value("");
        let _ = area.focus();
    }
    state.has_text.set(false);
    state.phase.set(Phase::Waiting);
    state.notice.set(None);
    state.credits.set(None);
    state.shots.set(Vec::new());
    state.viewing.set(None);

    let mut request = ImageRequest::new(asked.clone()).with_count(state.count.get_untracked());
    for (slot, value) in [
        (&mut request.model, state.model.get_untracked()),
        (&mut request.size, state.size.get_untracked()),
        (&mut request.quality, state.quality.get_untracked()),
    ] {
        if !value.is_empty() {
            *slot = Some(value);
        }
    }

    // The sources are what make this img2img. They are kept after the request, so
    // iterating on one picture is a prompt change and another click.
    let sources = state.sources.get_untracked();
    if !sources.is_empty() {
        request.input_images = sources.into_iter().map(|source| source.b64).collect();
    }

    let text_of = |field: NodeRef<Input>| {
        field
            .get()
            .map(|input| input.value())
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let negative = text_of(negative);
    if !negative.is_empty() {
        request.negative_prompt = Some(negative);
    }
    if let Ok(value) = text_of(steps).parse::<u32>() {
        request.steps = Some(value);
    }
    if let Ok(value) = text_of(guidance).parse::<f32>() {
        request.guidance = Some(value);
    }

    spawn_local(async move {
        let answer = match app_sdk::image::generate(&request).await {
            Ok(answer) => answer,
            Err(err) => {
                state.phase.set(Phase::Failed(err));
                return;
            }
        };

        let total = answer.images.len();
        let stem = stem(&asked);
        let stamp = stamp(js_sys::Date::now());
        let mut shots = Vec::new();

        for (index, image) in answer.images.into_iter().enumerate() {
            // One image gets the plain name; several get a numbered suffix so they sit
            // together in the folder.
            let name = if total == 1 {
                format!("{stamp}-{stem}.{}", image.extension())
            } else {
                format!("{stamp}-{stem}-{}.{}", index + 1, image.extension())
            };
            let path = format!("{DIR}/{name}");

            let mut shot = Shot {
                data_url: format!("data:{};base64,{}", image.mime, image.b64_json),
                previewable: image.is_previewable(),
                saved: None,
                error: None,
                revised_prompt: image.revised_prompt,
            };

            match app_sdk::image::decode_base64(&image.b64_json) {
                Ok(bytes) => match fs::write_bytes(&path, &bytes, &image.mime).await {
                    Ok(_) => {
                        ctx.log(&format!("saved {path}"));
                        shot.saved = Some(path);
                    }
                    Err(err) => shot.error = Some(err),
                },
                Err(err) => shot.error = Some(err),
            }
            shots.push(shot);
        }

        state.credits.set(answer.credits_cost);
        state.shots.set(shots);
        state.phase.set(Phase::Idle);
        refresh(state).await;
    });
}

/// Look at a saved image instead of the batch on screen.
fn open_kept(state: State, kept: Kept) {
    state.viewing.set(Some(kept));
}

/// The directory above `dir`, as a root-relative path. `""` is the root, so the root has
/// no parent to go to.
fn parent(dir: &str) -> String {
    dir.rsplit_once('/')
        .map(|(above, _)| above.to_owned())
        .unwrap_or_default()
}

/// Fill the sidebar's picker with one directory.
///
/// Directories come first so the navigation is at the top, and only the formats the
/// gateway accepts as a reference are offered: a TIFF would be read, uploaded and then
/// refused by the server, which is a worse way to find out than not being listed.
async fn browse(state: State) {
    let dir = state.dir.get_untracked();
    match fs::list(&dir).await {
        Ok(listing) => {
            let mut entries: Vec<Pick> = listing
                .entries
                .into_iter()
                .filter(|entry| match entry.kind {
                    EntryKind::Dir => true,
                    EntryKind::File => image_mime(&entry.name).is_some_and(accepts_input_mime),
                    _ => false,
                })
                .map(|entry| Pick {
                    path: if dir.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{dir}/{}", entry.name)
                    },
                    name: entry.name,
                    dir: entry.kind == EntryKind::Dir,
                })
                .collect();
            entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.cmp(&b.name)));
            state.entries.set(entries);
        }
        Err(err) => state.notice.set(Some(format!("cannot read {dir}: {err}"))),
    }
}

/// Open the picker, or close it and go back to the folder listing.
fn toggle_browsing(state: State) {
    let open = !state.browsing.get_untracked();
    state.browsing.set(open);
    if open {
        state.dir.set(String::new());
        spawn_local(browse(state));
    }
}

/// Read a file and keep it as a reference image.
///
/// The read is what the picker's click costs, so it happens here rather than at Generate:
/// a file that cannot be read says so while the user is still looking at the picker, not
/// after they have written a prompt.
fn add_source(state: State, path: String, name: String) {
    // Refused at the click so the answer is immediate; checked again below, where the
    // list is read fresh, because reading a file takes a moment and two quick clicks
    // would otherwise both decide they fit.
    let sources = state.sources.get_untracked();
    if sources.iter().any(|source| source.path == path) {
        state
            .notice
            .set(Some(format!("{name} is already a source")));
        return;
    }
    if sources.len() >= MAX_INPUT_IMAGES {
        state
            .notice
            .set(Some(format!("at most {MAX_INPUT_IMAGES} sources")));
        return;
    }

    spawn_local(async move {
        let bytes = match fs::read_bytes(&path).await {
            Ok(bytes) => bytes,
            Err(err) => {
                state
                    .notice
                    .set(Some(format!("could not read {name}: {err}")));
                return;
            }
        };
        let b64 = match app_sdk::image::encode_base64(&bytes) {
            Ok(b64) => b64,
            Err(err) => {
                state.notice.set(Some(err));
                return;
            }
        };

        let mut sources = state.sources.get_untracked();
        if sources.iter().any(|source| source.path == path) {
            return;
        }
        if sources.len() >= MAX_INPUT_IMAGES {
            state
                .notice
                .set(Some(format!("at most {MAX_INPUT_IMAGES} sources")));
            return;
        }
        sources.push(Source { path, name, b64 });
        state.sources.set(sources);
    });
}

/// Drop one reference image from the next request.
fn remove_source(state: State, path: String) {
    state
        .sources
        .update(|sources| sources.retain(|source| source.path != path));
}

/// A saved image becomes a reference for the next one.
fn source_from_kept(state: State, kept: Kept) {
    add_source(state, kept.path, kept.name);
}

/// Move a saved image to the trash, like any other file.
fn remove_kept(state: State, path: String) {
    spawn_local(async move {
        match fs::trash(&path).await {
            Ok(_) => {
                if state.viewing.get_untracked().map(|kept| kept.path) == Some(path) {
                    state.viewing.set(None);
                }
                refresh(state).await;
            }
            Err(err) => state
                .notice
                .set(Some(format!("could not remove it: {err}"))),
        }
    });
}

const STYLES: &str = "
.images { display: flex; flex-direction: column; height: 100%; font-family: inherit;
          font-size: 13px; box-sizing: border-box; }
.img-head { display: flex; gap: 8px; align-items: center; padding: 9px 12px;
            border-bottom: 1px solid; flex: none; flex-wrap: wrap; }
.img-field { display: flex; align-items: center; gap: 6px; font-size: 11px; opacity: .8; }
.img-select { padding: 6px 8px; border-radius: 8px; font-family: inherit; font-size: 12px;
              background: rgba(255,255,255,.05); color: inherit;
              border: 1px solid rgba(255,255,255,.12); max-width: 190px; }
.img-btn { padding: 6px 11px; border-radius: 8px; font-family: inherit; font-size: 12px;
           font-weight: 600; cursor: pointer; border: 1px solid rgba(255,255,255,.14);
           background: rgba(255,255,255,.05); color: inherit; }
.img-btn:disabled { opacity: .45; cursor: default; }
.img-body { display: flex; flex: 1; min-height: 0; }
.img-side { width: 176px; flex: none; overflow-y: auto; padding: 8px;
            border-right: 1px solid; display: flex; flex-direction: column; gap: 4px; }
.img-empty { font-size: 11.5px; opacity: .55; margin: 4px; line-height: 1.5; }
.img-side-item { display: flex; align-items: stretch; border-radius: 8px; }
.img-side-item:hover { background: rgba(255,255,255,.05); }
.img-side-item.active { background: rgba(255,255,255,.09); }
.img-side-open { flex: 1; min-width: 0; text-align: left; background: none; border: 0;
                 color: inherit; font: inherit; cursor: pointer; padding: 5px;
                 display: flex; flex-direction: column; gap: 4px; }
.img-thumb { width: 100%; height: 72px; object-fit: cover; border-radius: 6px;
             background: rgba(255,255,255,.04); display: block; }
.img-side-name { font-size: 10.5px; opacity: .6; overflow: hidden;
                 text-overflow: ellipsis; white-space: nowrap;
                 font-family: ui-monospace, monospace; }
.img-side-x { background: none; border: 0; color: inherit; cursor: pointer; opacity: .35;
              font-size: 14px; line-height: 1; padding: 0 7px; }
.img-side-x:hover { opacity: .9; }
.img-main { flex: 1; min-width: 0; display: flex; flex-direction: column; }
.img-log { flex: 1; overflow-y: auto; padding: 14px; }
.img-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
            gap: 12px; }
.img-card { display: flex; flex-direction: column; gap: 6px; border-radius: 10px;
            border: 1px solid rgba(255,255,255,.09); overflow: hidden;
            background: rgba(255,255,255,.03); }
.img-card img { width: 100%; display: block; }
.img-card-tile { padding: 22px 10px; text-align: center; font-size: 11.5px; opacity: .6; }
.img-card-foot { padding: 6px 8px; display: flex; flex-direction: column; gap: 3px; }
.img-card-note { font-size: 11px; opacity: .65; line-height: 1.4; }
.img-card-warn { font-size: 11px; color: #ffc9a0; line-height: 1.4; }
.img-link { font-size: 11px; color: inherit; opacity: .8; }
.img-viewer { display: flex; flex-direction: column; gap: 10px; align-items: center; }
.img-viewer img { max-width: 100%; max-height: 62vh; border-radius: 10px; display: block; }
.img-viewer-bar { display: flex; gap: 10px; align-items: baseline; width: 100%; }
.img-viewer-name { font-family: ui-monospace, monospace; font-size: 11.5px; opacity: .7;
                   overflow-wrap: anywhere; }
.img-error { color: #ff9ea1; font-size: 12px; line-height: 1.5; padding: 8px 10px;
             border-radius: 10px; background: rgba(255,80,90,.08); }
.img-welcome { opacity: .6; font-size: 12.5px; line-height: 1.6; max-width: 52ch; }
.img-welcome code { font-family: ui-monospace, monospace; font-size: 11.5px; }
.img-composer { display: flex; gap: 8px; align-items: flex-end; padding: 10px 12px;
                border-top: 1px solid; flex: none; }
.img-input { flex: 1; resize: none; min-height: 44px; max-height: 150px; padding: 9px 10px;
             border-radius: 10px; border: 1px solid rgba(255,255,255,.12);
             background: rgba(255,255,255,.05); color: inherit; font-family: inherit;
             font-size: 12.5px; line-height: 1.45; }
.img-input:focus { outline: 1px solid rgba(255,255,255,.22); }
.img-foot { display: flex; gap: 10px; align-items: baseline; padding: 0 12px 9px; flex: none; }
.img-note { font-size: 11px; line-height: 1.45; color: #ffc9a0; }
.img-cost { font-size: 10.5px; opacity: .5; font-family: ui-monospace, monospace;
            margin-left: auto; }
/* The sources for the next request, and the picker's own header. */
.img-sources { display: flex; gap: 8px; align-items: center; padding: 8px 12px 0;
               flex: none; flex-wrap: wrap; }
.img-sources-label { font-size: 11px; opacity: .6; }
.img-source { position: relative; display: flex; flex-direction: column; gap: 2px;
              width: 62px; }
.img-source img { width: 62px; height: 46px; object-fit: cover; border-radius: 6px;
                  display: block; background: rgba(255,255,255,.04); }
.img-source-name { font-size: 9.5px; opacity: .55; overflow: hidden;
                   text-overflow: ellipsis; white-space: nowrap; }
.img-source-x { position: absolute; top: 2px; right: 2px; border: 0; cursor: pointer;
                border-radius: 4px; line-height: 1; font-size: 11px; padding: 1px 4px;
                background: rgba(0,0,0,.55); color: #fff; }
.img-browse-head { display: flex; gap: 6px; align-items: center; margin-bottom: 4px; }
.img-browse-path { flex: 1; min-width: 0; font-size: 10.5px; font-family: ui-monospace,
                   monospace; opacity: .65; overflow: hidden; text-overflow: ellipsis;
                   white-space: nowrap; direction: rtl; text-align: left; }
.img-browse-item { display: block; width: 100%; text-align: left; background: none;
                   border: 0; color: inherit; font: inherit; cursor: pointer;
                   padding: 5px 7px; border-radius: 8px; font-size: 12px;
                   overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.img-browse-item:hover { background: rgba(255,255,255,.06); }
.img-advanced { flex: none; padding: 0 12px 4px; font-size: 11.5px; }
.img-advanced summary { cursor: pointer; opacity: .6; }
.img-advanced-row { display: flex; gap: 10px; flex-wrap: wrap; padding-top: 6px; }
.img-advanced-field { display: flex; align-items: center; gap: 6px; opacity: .8; }
.img-advanced-field input { width: 84px; padding: 5px 7px; border-radius: 7px;
                            border: 1px solid rgba(255,255,255,.12);
                            background: rgba(255,255,255,.05); color: inherit;
                            font-family: inherit; font-size: 12px; }
.img-advanced-field.wide input { width: 240px; }
";

app_sdk::app! {
    fn images(ctx: Context) -> impl IntoView {
        let theme = ctx.theme();

        // Options are remembered between windows and visits. Read before the first
        // render so the signal's own effect cannot write a default over them.
        let remembered = |key: &str| ctx.storage_get(key).unwrap_or_default();
        let count = remembered("count")
            .parse::<u32>()
            .ok()
            .filter(|n| COUNTS.contains(n))
            .unwrap_or(1);

        let state = State {
            info: RwSignal::new(ImageInfo::default()),
            model: RwSignal::new(remembered("model")),
            size: RwSignal::new(remembered("size")),
            count: RwSignal::new(count),
            quality: RwSignal::new(remembered("quality")),
            phase: RwSignal::new(Phase::Idle),
            shots: RwSignal::new(Vec::new()),
            kept: RwSignal::new(Vec::new()),
            viewing: RwSignal::new(None),
            sources: RwSignal::new(Vec::new()),
            browsing: RwSignal::new(false),
            dir: RwSignal::new(String::new()),
            entries: RwSignal::new(Vec::new()),
            notice: RwSignal::new(None),
            credits: RwSignal::new(None),
            has_text: RwSignal::new(false),
        };

        let composer = NodeRef::<Textarea>::new();
        // The advanced inputs: uncontrolled like the prompt, so typing in them cannot
        // move the caret, and read once when a request goes out.
        let negative = NodeRef::<Input>::new();
        let steps = NodeRef::<Input>::new();
        let guidance = NodeRef::<Input>::new();

        // Bootstrap: what this server offers, and what is already in the folder. The
        // folder is created when it is missing, so a first run with a writable root just
        // works — and a read-only one is reported rather than pretended about.
        let boot_ctx = ctx.clone();
        Effect::new(move |_| {
            let boot_ctx = boot_ctx.clone();
            spawn_local(async move {
                if fs::list(DIR).await.is_err() {
                    let _ = fs::create_dir("", DIR).await;
                }
                match app_sdk::image::info().await {
                    Ok(found) => {
                        // A remembered choice that this server does not offer is dropped
                        // rather than shown as a selection that would be refused.
                        if !found.models.iter().any(|model| model == &state.model.get_untracked()) {
                            state.model.set(found.default_model.clone());
                        }
                        if !found.sizes.iter().any(|size| size == &state.size.get_untracked()) {
                            state.size.set(found.sizes.first().cloned().unwrap_or_default());
                        }
                        if !matches!(state.quality.get_untracked().as_str(), "" | "standard" | "hd") {
                            state.quality.set(String::new());
                        }
                        state.info.set(found);
                    }
                    Err(err) => state.notice.set(Some(err)),
                }
                refresh(state).await;
                boot_ctx.log("ready");
            });
        });

        // Remember the options, so reopening the app does not make the user pick the
        // same size again.
        let storage_ctx = ctx.clone();
        Effect::new(move |_| {
            storage_ctx.storage_set("model", &state.model.get());
            storage_ctx.storage_set("size", &state.size.get());
            storage_ctx.storage_set("count", &state.count.get().to_string());
            storage_ctx.storage_set("quality", &state.quality.get());
        });

        let ready = move || state.info.get().enabled();
        let text_colour = theme.text.clone();
        let muted = theme.muted.clone();
        let stroke = theme.stroke.clone();
        let accent = theme.accent.clone();

        // One `Context` per handler: each is moved into its closure, and a closure that
        // moved the only one out would leave nothing for the next.
        let key_ctx = ctx.clone();
        let click_ctx = ctx.clone();

        let generate_style = format!("border-color:{stroke};background:{accent};color:#0b0d12;");

        view! {
            <style>{STYLES}</style>
            <div class="images" style=format!("color:{text_colour};")>
                <div class="img-head" style=format!("border-color:{stroke};")>
                    <label class="img-field">
                        "model"
                        <select
                            class="img-select"
                            prop:value=move || state.model.get()
                            on:change=move |ev| state.model.set(event_target_value(&ev))
                        >
                            {move || {
                                let info = state.info.get();
                                if info.models.is_empty() {
                                    return view! {
                                        <option value="">"no image model configured"</option>
                                    }.into_any();
                                }
                                info.models.into_iter().map(|name| {
                                    let value = name.clone();
                                    let chosen = name.clone();
                                    view! {
                                        <option value=value prop:selected=move || state.model.get() == chosen>
                                            {name}
                                        </option>
                                    }
                                }).collect_view().into_any()
                            }}
                        </select>
                    </label>
                    <label class="img-field">
                        "size"
                        <select
                            class="img-select"
                            prop:value=move || state.size.get()
                            on:change=move |ev| state.size.set(event_target_value(&ev))
                        >
                            {move || state.info.get().sizes.into_iter().map(|size| {
                                let value = size.clone();
                                let chosen = size.clone();
                                view! {
                                    <option value=value prop:selected=move || state.size.get() == chosen>
                                        {size}
                                    </option>
                                }
                            }).collect_view()}
                        </select>
                    </label>
                    <label class="img-field">
                        "count"
                        <select
                            class="img-select"
                            prop:value=move || state.count.get().to_string()
                            on:change=move |ev| {
                                if let Ok(n) = event_target_value(&ev).parse::<u32>() {
                                    state.count.set(n);
                                }
                            }
                        >
                            {COUNTS.into_iter().map(|n| {
                                view! {
                                    <option value=n.to_string() prop:selected=move || state.count.get() == n>
                                        {n.to_string()}
                                    </option>
                                }
                            }).collect_view()}
                        </select>
                    </label>
                    <label class="img-field">
                        "quality"
                        <select
                            class="img-select"
                            prop:value=move || state.quality.get()
                            on:change=move |ev| state.quality.set(event_target_value(&ev))
                        >
                            <option value="" prop:selected=move || state.quality.get().is_empty()>
                                "model default"
                            </option>
                            <option value="standard" prop:selected=move || state.quality.get() == "standard">
                                "standard"
                            </option>
                            <option value="hd" prop:selected=move || state.quality.get() == "hd">
                                "hd"
                            </option>
                        </select>
                    </label>
                </div>

                <div class="img-body">
                    <div class="img-side" style=format!("border-color:{stroke};")>
                        {move || {
                            // The sidebar has one job at a time: show what is in `images/`,
                            // or walk the sandbox to pick a source for the next request.
                            if state.browsing.get() {
                                let dir = state.dir.get();
                                let up = parent(&dir);
                                let label = if dir.is_empty() {
                                    "Sandbox".to_owned()
                                } else {
                                    dir.clone()
                                };
                                return view! {
                                    <div class="img-browse-head">
                                        <button
                                            class="img-btn"
                                            title="Up one folder"
                                            disabled=dir.is_empty()
                                            on:click=move |_| {
                                                state.dir.set(up.clone());
                                                spawn_local(browse(state));
                                            }
                                        >
                                            "↑"
                                        </button>
                                        <span class="img-browse-path" title=label.clone()>{label.clone()}</span>
                                        <button class="img-btn" on:click=move |_| toggle_browsing(state)>
                                            "Done"
                                        </button>
                                    </div>
                                    {move || {
                                        let entries = state.entries.get();
                                        if entries.is_empty() {
                                            return view! {
                                                <p class="img-empty">"No folders and no PNG, JPEG or WebP files here."</p>
                                            }.into_any();
                                        }
                                        view! {
                                            <For each=move || state.entries.get() key=|pick| pick.path.clone() let:pick>
                                                {
                                                    let to_open = pick.path.clone();
                                                    let name = pick.name.clone();
                                                    let is_dir = pick.dir;
                                                    view! {
                                                        <button
                                                            class="img-browse-item"
                                                            title=name.clone()
                                                            on:click=move |_| {
                                                                if is_dir {
                                                                    state.dir.set(to_open.clone());
                                                                    spawn_local(browse(state));
                                                                } else {
                                                                    add_source(state, to_open.clone(), name.clone());
                                                                }
                                                            }
                                                        >
                                                            {if is_dir { format!("🗀 {name}") } else { name.clone() }}
                                                        </button>
                                                    }
                                                }
                                            </For>
                                        }.into_any()
                                    }}
                                }.into_any();
                            }

                            let list = state.kept.get();
                            if list.is_empty() {
                                return view! {
                                    <p class="img-empty">"Generated images are saved in " <code>"images/"</code> " and appear here."</p>
                                }.into_any();
                            }
                            view! {
                                <For each=move || state.kept.get() key=|kept| kept.path.clone() let:kept>
                                    {
                                        let is_open = kept.path.clone();
                                        let to_open = kept.clone();
                                        let to_remove = kept.path.clone();
                                        let title = kept.name.clone();
                                        view! {
                                            <div
                                                class="img-side-item"
                                                class:active=move || state.viewing.get().map(|k| k.path) == Some(is_open.clone())
                                            >
                                                <button
                                                    class="img-side-open"
                                                    title=title
                                                    on:click=move |_| open_kept(state, to_open.clone())
                                                >
                                                    <img class="img-thumb" loading="lazy" src=fs::raw_url(&kept.path) alt=""/>
                                                    <span class="img-side-name">{kept.name}</span>
                                                </button>
                                                <button
                                                    class="img-side-x"
                                                    title="Move this image to the trash"
                                                    on:click=move |_| remove_kept(state, to_remove.clone())
                                                >
                                                    "×"
                                                </button>
                                            </div>
                                        }
                                    }
                                </For>
                            }.into_any()
                        }}
                    </div>

                    <div class="img-main">
                        <div class="img-log">
                            {move || {
                                if let Some(kept) = state.viewing.get() {
                                    let name = kept.name.clone();
                                    let path = kept.path.clone();
                                    return view! {
                                        <div class="img-viewer">
                                            <div class="img-viewer-bar">
                                                <span class="img-viewer-name">{name.clone()}</span>
                                                <a class="img-link" href=fs::download_url(&path) download=name.clone()>
                                                    "⭳ Download"
                                                </a>
                                                <button
                                                    class="img-btn"
                                                    title="Use this picture as the start of the next one"
                                                    on:click=move |_| source_from_kept(state, kept.clone())
                                                >
                                                    "Use as source"
                                                </button>
                                                <button
                                                    class="img-btn"
                                                    on:click=move |_| state.viewing.set(None)
                                                >
                                                    "Close"
                                                </button>
                                            </div>
                                            <img src=fs::raw_url(&path) alt=""/>
                                        </div>
                                    }.into_any();
                                }

                                match state.phase.get() {
                                    Phase::Waiting => view! {
                                        <div class="img-welcome">"Drawing…"</div>
                                    }.into_any(),
                                    Phase::Failed(err) => view! {
                                        <div class="img-error">{err}</div>
                                    }.into_any(),
                                    Phase::Idle => {
                                        let shots = state.shots.get();
                                        if shots.is_empty() {
                                            return view! {
                                                <p class="img-welcome">
                                                    "Describe a picture. The server holds the provider's key, so nothing here can spend it on a host it was not pointed at."
                                                    <br/>
                                                    <br/>
                                                    "Every result is saved to "
                                                    <code>"images/"</code>
                                                    " in your files, so the Gallery can show it and the trash can bring it back. Add a "
                                                    <b>"source"</b>
                                                    " to start from a picture instead of a blank page — one from here, or any PNG, JPEG or WebP in your files."
                                                </p>
                                            }.into_any();
                                        }
                                        view! {
                                            <div class="img-grid">
                                                {shots.into_iter().map(|shot| {
                                                    // The saved file, as a link and the name it downloads as.
                                                    let saved = shot.saved.as_deref().map(|path| {
                                                        let name = path
                                                            .rsplit('/')
                                                            .next()
                                                            .unwrap_or("image")
                                                            .to_owned();
                                                        (fs::download_url(path), name)
                                                    });
                                                    view! {
                                                        <div class="img-card">
                                                            {if shot.previewable {
                                                                view! { <img src=shot.data_url.clone() alt=""/> }.into_any()
                                                            } else {
                                                                view! {
                                                                    <div class="img-card-tile">"an image format this browser cannot draw"</div>
                                                                }.into_any()
                                                            }}
                                                            <div class="img-card-foot">
                                                                {saved.map(|(href, name)| {
                                                                    let label = name.clone();
                                                                    view! {
                                                                        <a class="img-link" href=href download=name>
                                                                            "⭳ " {label}
                                                                        </a>
                                                                    }
                                                                })}
                                                                {shot.error.as_ref().map(|err| view! {
                                                                    <span class="img-card-warn">"not saved: " {err.clone()}</span>
                                                                })}
                                                                {shot.revised_prompt.as_ref().filter(|revised| !revised.is_empty()).map(|revised| view! {
                                                                    <span class="img-card-note">{revised.clone()}</span>
                                                                })}
                                                            </div>
                                                        </div>
                                                    }
                                                }).collect_view()}
                                            </div>
                                        }.into_any()
                                    }
                                }
                            }}
                        </div>

                        <div class="img-sources">
                            <span class="img-sources-label">"source"</span>
                            {move || state.sources.get().into_iter().map(|source| {
                                let remove = source.path.clone();
                                let title = source.name.clone();
                                view! {
                                    <div class="img-source" title=title>
                                        <img src=fs::raw_url(&source.path) alt=""/>
                                        <span class="img-source-name">{source.name}</span>
                                        <button
                                            class="img-source-x"
                                            title="Remove this source"
                                            on:click=move |_| remove_source(state, remove.clone())
                                        >
                                            "×"
                                        </button>
                                    </div>
                                }
                            }).collect_view()}
                            <button
                                class="img-btn"
                                title="Pick a picture from your files to start from"
                                on:click=move |_| toggle_browsing(state)
                            >
                                {move || if state.browsing.get() { "Cancel" } else { "+ source" }}
                            </button>
                            <Show when=move || !state.sources.get().is_empty()>
                                <button class="img-btn" on:click=move |_| state.sources.set(Vec::new())>
                                    "Clear"
                                </button>
                            </Show>
                        </div>

                        <details class="img-advanced">
                            <summary>"advanced"</summary>
                            <div class="img-advanced-row">
                                <label class="img-advanced-field wide">
                                    "avoid"
                                    <input node_ref=negative placeholder="what not to draw" autocomplete="off"/>
                                </label>
                                <label class="img-advanced-field">
                                    "steps"
                                    <input node_ref=steps inputmode="numeric" placeholder="model default" autocomplete="off"/>
                                </label>
                                <label class="img-advanced-field">
                                    "guidance"
                                    <input node_ref=guidance inputmode="numeric" placeholder="model default" autocomplete="off"/>
                                </label>
                            </div>
                        </details>

                        <div class="img-composer" style=format!("border-color:{stroke};")>
                            <textarea
                                class="img-input"
                                node_ref=composer
                                rows="2"
                                placeholder=move || if ready() {
                                    if state.sources.get().is_empty() {
                                        "Describe a picture — Ctrl+Enter to draw".to_owned()
                                    } else {
                                        "Describe what to make of the source — Ctrl+Enter to draw".to_owned()
                                    }
                                } else {
                                    "the server has no image provider configured".to_owned()
                                }
                                on:input=move |ev| state.has_text.set(!event_target_value(&ev).trim().is_empty())
                                on:keydown=move |ev: web_sys::KeyboardEvent| {
                                    if ev.key() == "Enter" && (ev.ctrl_key() || ev.meta_key()) {
                                        ev.prevent_default();
                                        generate(state, key_ctx.clone(), composer, negative, steps, guidance);
                                    }
                                }
                            ></textarea>
                            <button
                                class="img-btn"
                                style=generate_style.clone()
                                disabled=move || {
                                    state.phase.get() == Phase::Waiting
                                        || !ready()
                                        || !state.has_text.get()
                                }
                                on:click=move |_| generate(state, click_ctx.clone(), composer, negative, steps, guidance)
                            >
                                {move || if state.phase.get() == Phase::Waiting { "Drawing…" } else { "Generate" }}
                            </button>
                        </div>

                        <div class="img-foot">
                            {move || state.notice.get().map(|text| view! {
                                <span class="img-note">{text}</span>
                            })}
                            <Show when=move || !ready()>
                                <span class="img-note">
                                    "Set WEB_DESKTOP_IMAGE_URL (and WEB_DESKTOP_IMAGE_KEY) on the server to enable this."
                                </span>
                            </Show>
                            {move || state.credits.get().map(|credits| view! {
                                <span class="img-cost" style=format!("color:{muted};")>
                                    {format!("{credits:.2} credits")}
                                </span>
                            })}
                        </div>
                    </div>
                </div>
            </div>
        }
    }
}
