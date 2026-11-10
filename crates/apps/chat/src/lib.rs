//! A conversation with a model the server talks to.
//!
//! Two things make this app different from the other served apps:
//!
//! * **The provider's key is not here, and neither is its address.** The app posts a
//!   conversation to `/api/ai/chat`; the server holds the key and calls the provider.
//!   So the app has no credential to leak and no host to be talked into changing, and
//!   which model it may use is the server's list, not its own.
//! * **The conversation is a file.** Each chat is one JSON document under `chats/` in
//!   the same sandbox the Files app browses, so it can be renamed, copied, opened in
//!   the Text Editor, and — if it is deleted — put back from the trash. The app writes
//!   through the ordinary file API, which may refuse: a read-only root is a reason it
//!   reports rather than a crash.
//!
//! What it deliberately is not: a chat that keeps its own copy of the provider's state.
//! Answers stream — the words appear as the model produces them — and Stop aborts the
//! request rather than waiting for the answer to arrive and ignoring it. That matters on
//! both ends: aborting is what tells the server to stop paying for tokens nobody will
//! read, and it is what makes Stop immediate rather than "after the next word". The
//! conversation is saved after every turn, a partial one included.

use app_sdk::ai::{AiInfo, ChatMessage, ChatRequest, ChatRole, Usage};
use app_sdk::fs::{self, EntryKind};
use app_sdk::prelude::*;
use leptos::html::{Div, Textarea};
use leptos::task::spawn_local;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;

/// Where conversations are kept, root-relative like every other path in the app.
const DIR: &str = "chats";

/// How many saved conversations the sidebar reads and shows.
const LIST_LIMIT: usize = 40;

/// Characters of the first user message that become a title.
const TITLE_CHARS: usize = 48;

/// A conversation as it is stored on disk.
///
/// The same shape as the wire: `messages` is what would be posted back to the server,
/// which means reopening a chat is a decode and nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Saved {
    /// The first thing the user asked, shortened. Shown in the sidebar.
    title: String,
    /// Milliseconds since the epoch, for ordering and for the sidebar's date.
    updated: f64,
    model: String,
    messages: Vec<ChatMessage>,
}

/// One row of the sidebar.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    /// Root-relative path, which is also how it is deleted.
    path: String,
    title: String,
    updated: f64,
}

/// What the composer is doing right now.
#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Idle,
    /// A request is out. The window stays usable; the composer does not send twice.
    Waiting,
    /// The last request failed, with the server's own words.
    Failed(String),
}

/// The window's state in one copyable handle.
///
/// A bundle rather than a dozen locals because the view's child closures must be
/// `Send + Sync`, and a signal is; an `Rc<dyn Fn>` holding the same state is not. Since
/// signals are `Copy`, so is this, and every closure below can capture it freely —
/// including `abort`, whose `StoredValue` handle is `Copy` and `Send` even though the
/// `AbortController` inside it is a JS value that is neither.
#[derive(Clone, Copy)]
struct State {
    info: RwSignal<AiInfo>,
    model: RwSignal<String>,
    messages: RwSignal<Vec<ChatMessage>>,
    phase: RwSignal<Phase>,
    usage: RwSignal<Option<Usage>>,
    entries: RwSignal<Vec<Entry>>,
    /// One line of "that did not work", from the server or from the file API.
    notice: RwSignal<Option<String>>,
    /// The file this conversation is saved as. `None` until the first answer, which is
    /// also what makes a new chat new.
    saved_as: RwSignal<Option<String>>,
    /// When this conversation started, for its filename.
    started: RwSignal<f64>,
    /// Only drives the Send button's look. The textarea's own value is the truth,
    /// because binding it reactively would move the caret on every keystroke.
    has_text: RwSignal<bool>,
    /// The answer as it arrives, before it becomes a message.
    streaming: RwSignal<String>,
    /// Which request is current. An answer that arrives after the window has moved on —
    /// because another conversation was opened, or a new chat started — must not be
    /// committed into it, and this is how it knows to drop itself on the floor.
    attempt: RwSignal<u32>,
    /// The request in flight, so Stop can abort it. Not a signal, because an
    /// `AbortController` is a JS value: `StoredValue` with `LocalStorage` is how a
    /// non-`Send` value is kept behind a `Copy`, `Send` handle.
    abort: StoredValue<Option<web_sys::AbortController>, LocalStorage>,
}

/// A filename that reads well in Files and sorts by date.
///
/// `2026-02-14-093012-why-is-the-sky-blue.json`: the timestamp is the sort key, and the
/// title is there because a folder of nothing but numbers is unpleasant to browse.
fn file_name(title: &str, at: f64) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(at));
    format!(
        "{DIR}/{:04}-{:02}-{:02}-{:02}{:02}{:02}-{}.json",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date(),
        date.get_hours(),
        date.get_minutes(),
        date.get_seconds(),
        slug(title),
    )
}

/// The title, reduced to something a filename can hold.
///
/// Anything that is not a letter or a digit becomes a dash, so the result can never
/// contain a separator the file API would refuse; letters outside ASCII are kept,
/// because a title in another alphabet deserves a filename in it.
fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut dash = true; // no leading dash
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
        if out.chars().count() >= 32 {
            break;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "chat".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// "2026-02-14 09:30", from milliseconds.
fn when(milliseconds: f64) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(milliseconds));
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date(),
        date.get_hours(),
        date.get_minutes(),
    )
}

/// What to call a conversation: the first thing the user asked.
fn title_of(messages: &[ChatMessage]) -> String {
    let asked = messages
        .iter()
        .find(|message| message.role == ChatRole::User)
        .map(|message| message.content.trim())
        .unwrap_or_default();

    // One line, because the sidebar shows one line.
    let line = asked.lines().next().unwrap_or_default().trim();
    let mut title: String = line.chars().take(TITLE_CHARS).collect();
    if line.chars().count() > TITLE_CHARS {
        title.push('…');
    }
    if title.is_empty() {
        "New conversation".to_owned()
    } else {
        title
    }
}

/// Every conversation in `chats/`, newest first.
///
/// A file that will not decode is skipped rather than reported: the folder is the
/// user's, and something they put there themselves is not this app's business.
async fn load_index() -> Result<Vec<Entry>, String> {
    let listing = fs::list(DIR).await?;

    let mut names: Vec<String> = listing
        .entries
        .into_iter()
        .filter(|entry| entry.kind == EntryKind::File && entry.name.ends_with(".json"))
        .map(|entry| entry.name)
        .collect();
    // The name starts with a timestamp, so name order is time order.
    names.sort();
    names.reverse();
    names.truncate(LIST_LIMIT);

    let mut entries = Vec::new();
    for name in names {
        let path = format!("{DIR}/{name}");
        let Ok(text) = fs::read_text(&path).await else {
            continue;
        };
        let Ok(saved) = serde_json::from_str::<Saved>(&text) else {
            continue;
        };
        entries.push(Entry {
            path,
            title: saved.title,
            updated: saved.updated,
        });
    }
    Ok(entries)
}

async fn save(path: &str, conversation: &Saved) -> Result<(), String> {
    let text = serde_json::to_string_pretty(conversation)
        .map_err(|err| format!("could not encode the conversation: {err}"))?;
    fs::write_text(path, &text).await.map(|_| ())
}

/// Set aside whatever is in flight, without touching the messages.
///
/// Bumping the attempt is the load-bearing part: an answer already on the wire comes back
/// to a `send` that is watching a number nobody is waiting for any more, and discards
/// itself instead of being written into the conversation that replaced it.
fn abandon_in_flight(state: State) {
    stop(state);
    state.streaming.set(String::new());
    state
        .attempt
        .update(|attempt| *attempt = attempt.wrapping_add(1));
}

/// Clear the window for a new conversation.
fn start_new(state: State, composer: NodeRef<Textarea>) {
    abandon_in_flight(state);
    state.messages.set(Vec::new());
    state.phase.set(Phase::Idle);
    state.usage.set(None);
    state.saved_as.set(None);
    state.started.set(js_sys::Date::now());
    state.notice.set(None);
    state.has_text.set(false);
    if let Some(area) = composer.get() {
        area.set_value("");
        let _ = area.focus();
    }
}

/// Ask the answer in flight to stop.
///
/// Aborting is the point: a read that is still waiting for the model's first word never
/// returns, so it would never notice a flag being set, and Stop would look broken for
/// exactly as long as the model was slowest.
fn stop(state: State) {
    if let Some(controller) = state.abort.try_get_value().flatten() {
        controller.abort();
    }
}

/// Put an answer on screen, then save the conversation.
///
/// The save is best-effort: the answer is already the user's, and a root that is
/// read-only must show up in the footer rather than swallow it.
async fn commit(
    state: State,
    ctx: Context,
    text: String,
    model: Option<String>,
    usage: Option<Usage>,
    note: Option<String>,
) {
    state.streaming.set(String::new());

    let mut history = state.messages.get_untracked();
    history.push(ChatMessage::assistant(text));
    state.messages.set(history.clone());
    state.usage.set(usage);
    state.phase.set(Phase::Idle);

    let conversation = Saved {
        title: title_of(&history),
        updated: js_sys::Date::now(),
        model: model.unwrap_or_else(|| state.model.get_untracked()),
        messages: history,
    };
    let target = match state.saved_as.get_untracked() {
        Some(path) => path,
        None => {
            let path = file_name(&conversation.title, state.started.get_untracked());
            state.saved_as.set(Some(path.clone()));
            path
        }
    };

    let mut problem = note;
    match save(&target, &conversation).await {
        Ok(()) => {
            ctx.log(&format!("saved {target}"));
            if let Ok(found) = load_index().await {
                state.entries.set(found);
            }
        }
        Err(err) => {
            problem = Some(match problem {
                Some(note) => format!("{note}; could not save this chat: {err}"),
                None => format!("could not save this chat: {err}"),
            });
        }
    }
    state.notice.set(problem);
}

/// Send what is in the composer, and show the answer as it arrives.
///
/// The user's message is on screen before the request goes out, so a slow or refused
/// answer never looks like a message that was lost.
fn send(state: State, ctx: Context, composer: NodeRef<Textarea>) {
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

    let mut history = state.messages.get_untracked();
    history.push(ChatMessage::user(asked));
    state.messages.set(history.clone());
    state.phase.set(Phase::Waiting);
    state.usage.set(None);
    state.notice.set(None);
    state.streaming.set(String::new());

    let mut request = ChatRequest::new(history);
    let chosen = state.model.get_untracked();
    if !chosen.is_empty() {
        request.model = Some(chosen);
    }

    // One controller per request, kept where Stop can reach it. A browser that declines
    // to give us one is not a reason to refuse to ask; the answer just cannot be cut
    // short, and the flag-free path below still ends when the stream does.
    let controller = web_sys::AbortController::new().ok();
    let signal = controller.as_ref().map(web_sys::AbortController::signal);
    state.abort.set_value(controller);

    let attempt = state.attempt.get_untracked().wrapping_add(1);
    state.attempt.set(attempt);

    spawn_local(async move {
        let outcome = app_sdk::ai::chat_stream(&request, signal.as_ref(), |delta| {
            // A late delta from an abandoned request has nowhere to go.
            if state.attempt.get_untracked() == attempt {
                state.streaming.update(|answer| answer.push_str(delta));
            }
        })
        .await;
        state.abort.set_value(None);

        // The window moved on — another conversation, a new chat, or a second attempt.
        // Whatever came back belongs to a question nobody is looking at.
        if state.attempt.get_untracked() != attempt {
            return;
        }

        match outcome {
            // Nothing at all: there is no turn worth keeping, so the outcome is the whole
            // report. The user's own message stays where it is, ready to try again.
            Ok(streamed) if streamed.text.is_empty() => {
                state.streaming.set(String::new());
                if streamed.stopped {
                    // Stopping before the model started is not a failure — it is what the
                    // button was for — so it is a note, not a red box.
                    state.phase.set(Phase::Idle);
                    state
                        .notice
                        .set(Some("stopped before the model said anything".to_owned()));
                } else {
                    state
                        .phase
                        .set(Phase::Failed("the model sent an empty answer".to_owned()));
                }
            }
            Ok(streamed) => {
                let note = streamed
                    .stopped
                    .then(|| "you stopped it, so that answer is partial".to_owned());
                commit(
                    state,
                    ctx,
                    streamed.text,
                    streamed.model,
                    streamed.usage,
                    note,
                )
                .await;
            }
            Err(err) => {
                // Whatever arrived before the connection broke is still the model's answer,
                // and it was still paid for. Keep it, and report the failure beside it.
                let partial = state.streaming.get_untracked();
                if partial.is_empty() {
                    state.streaming.set(String::new());
                    state.phase.set(Phase::Failed(err));
                } else {
                    commit(state, ctx, partial, None, None, Some(err)).await;
                }
            }
        }
    });
}

/// Load a saved conversation into the window.
///
/// Takes no `Context`: this is called from a sidebar row, and a view's child closure
/// has to be `Send + Sync` while a `Context` holds a JS value. Nothing here needs the
/// shell, so it does not ask for it.
fn open_saved(state: State, path: String) {
    // Opening a conversation is also a decision to stop listening to the last one.
    abandon_in_flight(state);
    state.phase.set(Phase::Idle);
    spawn_local(async move {
        let text = match fs::read_text(&path).await {
            Ok(text) => text,
            Err(err) => {
                state
                    .notice
                    .set(Some(format!("could not open {path}: {err}")));
                return;
            }
        };
        match serde_json::from_str::<Saved>(&text) {
            Ok(saved) => {
                if !saved.model.is_empty() {
                    state.model.set(saved.model.clone());
                }
                state.started.set(saved.updated);
                state.messages.set(saved.messages);
                state.usage.set(None);
                state.phase.set(Phase::Idle);
                state.saved_as.set(Some(path.clone()));
                state.notice.set(None);
            }
            Err(err) => state
                .notice
                .set(Some(format!("{path} is not a conversation: {err}"))),
        }
    });
}

/// Move a saved conversation to the trash.
fn remove_saved(state: State, path: String) {
    spawn_local(async move {
        match fs::trash(&path).await {
            Ok(_) => {
                // Deleting what is open leaves it on screen, but the next answer starts
                // a new file rather than writing into the trash.
                if state.saved_as.get_untracked().as_deref() == Some(path.as_str()) {
                    state.saved_as.set(None);
                    state.started.set(js_sys::Date::now());
                }
                if let Ok(found) = load_index().await {
                    state.entries.set(found);
                }
            }
            Err(err) => state
                .notice
                .set(Some(format!("could not remove it: {err}"))),
        }
    });
}

const STYLES: &str = "
.chat { display: flex; flex-direction: column; height: 100%; font-family: inherit;
        font-size: 13px; box-sizing: border-box; }
.chat-head { display: flex; gap: 8px; align-items: center; padding: 9px 12px;
             border-bottom: 1px solid; flex: none; }
.chat-model { flex: 1; min-width: 0; padding: 6px 8px; border-radius: 8px; font-family: inherit;
              font-size: 12px; background: rgba(255,255,255,.05); color: inherit;
              border: 1px solid rgba(255,255,255,.12); }
.chat-btn { padding: 6px 11px; border-radius: 8px; font-family: inherit; font-size: 12px;
            font-weight: 600; cursor: pointer; border: 1px solid rgba(255,255,255,.14);
            background: rgba(255,255,255,.05); color: inherit; }
.chat-btn:disabled { opacity: .45; cursor: default; }
.chat-body { display: flex; flex: 1; min-height: 0; }
/* The sidebar scrolls on its own, so a long list never pushes the composer off screen. */
.chat-side { width: 168px; flex: none; overflow-y: auto; padding: 8px;
             border-right: 1px solid; display: flex; flex-direction: column; gap: 3px; }
.chat-empty { font-size: 11.5px; opacity: .55; margin: 4px; line-height: 1.5; }
.chat-side-item { display: flex; align-items: stretch; border-radius: 8px; }
.chat-side-item:hover { background: rgba(255,255,255,.05); }
.chat-side-item.active { background: rgba(255,255,255,.09); }
.chat-side-open { flex: 1; min-width: 0; text-align: left; background: none; border: 0;
                  color: inherit; font: inherit; cursor: pointer; padding: 6px 7px;
                  display: flex; flex-direction: column; gap: 2px; }
.chat-side-title { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; }
.chat-side-when { font-size: 10.5px; opacity: .5; font-family: ui-monospace, monospace; }
.chat-side-x { background: none; border: 0; color: inherit; cursor: pointer; opacity: .35;
               font-size: 14px; line-height: 1; padding: 0 7px; }
.chat-side-x:hover { opacity: .9; }
.chat-main { flex: 1; min-width: 0; display: flex; flex-direction: column; }
.chat-log { flex: 1; overflow-y: auto; padding: 14px 14px 4px; display: flex;
            flex-direction: column; gap: 10px; }
.turn { max-width: 88%; border-radius: 12px; padding: 8px 11px; }
/* An answer keeps its newlines and wraps long words, so a code block or a long url
   cannot push the bubble wide. */
.turn-body { white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.5; }
.turn-user { align-self: flex-end; background: rgba(255,255,255,.09); }
.turn-assistant { align-self: flex-start; background: rgba(255,255,255,.04); }
.turn-waiting { letter-spacing: .35em; opacity: .5; }
.chat-error { align-self: stretch; color: #ff9ea1; font-size: 12px; line-height: 1.5;
              padding: 8px 10px; border-radius: 10px; background: rgba(255,80,90,.08); }
.chat-welcome { opacity: .6; font-size: 12.5px; line-height: 1.6; max-width: 46ch; }
.chat-welcome code { font-family: ui-monospace, monospace; font-size: 11.5px; }
.chat-composer { display: flex; gap: 8px; align-items: flex-end; padding: 10px 12px;
                 border-top: 1px solid; flex: none; }
.chat-input { flex: 1; resize: none; min-height: 40px; max-height: 140px; padding: 9px 10px;
              border-radius: 10px; border: 1px solid rgba(255,255,255,.12);
              background: rgba(255,255,255,.05); color: inherit; font-family: inherit;
              font-size: 12.5px; line-height: 1.45; }
.chat-input:focus { outline: 1px solid rgba(255,255,255,.22); }
.chat-foot { display: flex; gap: 10px; align-items: baseline; padding: 0 12px 9px; flex: none; }
.chat-note { font-size: 11px; line-height: 1.45; color: #ffc9a0; }
.chat-tokens { font-size: 10.5px; opacity: .5; font-family: ui-monospace, monospace; margin-left: auto; }
";

app_sdk::app! {
    fn chat(ctx: Context) -> impl IntoView {
        let theme = ctx.theme();

        let state = State {
            info: RwSignal::new(AiInfo::default()),
            model: RwSignal::new(String::new()),
            messages: RwSignal::new(Vec::new()),
            phase: RwSignal::new(Phase::Idle),
            usage: RwSignal::new(None),
            entries: RwSignal::new(Vec::new()),
            notice: RwSignal::new(None),
            saved_as: RwSignal::new(None),
            started: RwSignal::new(js_sys::Date::now()),
            has_text: RwSignal::new(false),
            streaming: RwSignal::new(String::new()),
            attempt: RwSignal::new(0),
            abort: StoredValue::new_local(None),
        };

        let composer = NodeRef::<Textarea>::new();
        let log = NodeRef::<Div>::new();

        // Bootstrap: what this server offers, and what is already saved. The folder is
        // created when it is missing, so a first run with a writable root just works —
        // and a read-only one is reported rather than pretended about.
        let boot_ctx = ctx.clone();
        Effect::new(move |_| {
            let boot_ctx = boot_ctx.clone();
            spawn_local(async move {
                if fs::list(DIR).await.is_err() {
                    let _ = fs::create_dir("", DIR).await;
                }
                match app_sdk::ai::info().await {
                    Ok(found) => {
                        if !found.default_model.is_empty() {
                            state.model.set(found.default_model.clone());
                        }
                        state.info.set(found);
                    }
                    Err(err) => state.notice.set(Some(err)),
                }
                match load_index().await {
                    Ok(found) => state.entries.set(found),
                    Err(err) => state
                        .notice
                        .set(Some(format!("cannot read {DIR}/: {err}"))),
                }
                boot_ctx.log("ready");
            });
        });

        // Keep the newest thing said in view — including the answer as it grows.
        Effect::new(move |_| {
            state.messages.track();
            state.phase.track();
            state.streaming.track();
            if let Some(element) = log.get() {
                element.set_scroll_top(element.scroll_height());
            }
        });

        let ready = move || state.info.get().enabled();
        let text_colour = theme.text.clone();
        let muted = theme.muted.clone();
        let stroke = theme.stroke.clone();
        let accent = theme.accent.clone();

        // One `Context` per handler: each is moved into its closure, and a closure that
        // moved the only one out would leave nothing for the next.
        let click_ctx = ctx.clone();
        let key_ctx = ctx.clone();

        // The two faces of the one button. Built here so the closure below captures
        // these rather than the palette, which is still needed further down the view.
        let send_style = format!("border-color:{stroke};background:{accent};color:#0b0d12;");
        let stop_style = "border-color:#ef4444;background:#ef4444;color:#0b0d12;".to_owned();

        view! {
            <style>{STYLES}</style>
            <div class="chat" style=format!("color:{text_colour};")>
                <div class="chat-head" style=format!("border-color:{stroke};")>
                    <select
                        class="chat-model"
                        prop:value=move || state.model.get()
                        on:change=move |ev| state.model.set(event_target_value(&ev))
                    >
                        {move || {
                            let info = state.info.get();
                            if info.models.is_empty() {
                                return view! {
                                    <option value="">"no model configured on the server"</option>
                                }.into_any();
                            }
                            info.models.into_iter().map(|name| {
                                let value = name.clone();
                                let chosen = name.clone();
                                view! {
                                    // The property, not the attribute: an `option` with
                                    // `selected="false"` is still selected, since it is
                                    // the attribute's presence that counts. Driving each
                                    // option from `model` also means the box cannot show
                                    // one model while the app would send another — which
                                    // it would if the selection relied on the select's
                                    // `value` being set before its options existed.
                                    <option
                                        value=value
                                        prop:selected=move || state.model.get() == chosen
                                    >
                                        {name}
                                    </option>
                                }
                            }).collect_view().into_any()
                        }}
                    </select>
                    <button
                        class="chat-btn"
                        style=format!("border-color:{stroke};")
                        on:click=move |_| start_new(state, composer)
                    >
                        "New"
                    </button>
                </div>

                <div class="chat-body">
                    <div class="chat-side" style=format!("border-color:{stroke};")>
                        {move || {
                            let list = state.entries.get();
                            if list.is_empty() {
                                return view! {
                                    <p class="chat-empty">"Saved conversations appear here."</p>
                                }.into_any();
                            }
                            list.into_iter().map(|entry| {
                                let is_open = entry.path.clone();
                                let to_open = entry.path.clone();
                                let to_remove = entry.path;
                                let label = entry.title.clone();
                                let updated = entry.updated;
                                view! {
                                    <div
                                        class="chat-side-item"
                                        class:active=move || state.saved_as.get().as_deref() == Some(is_open.as_str())
                                    >
                                        <button
                                            class="chat-side-open"
                                            title=label
                                            on:click=move |_| open_saved(state, to_open.clone())
                                        >
                                            <span class="chat-side-title">{entry.title}</span>
                                            <span class="chat-side-when">{when(updated)}</span>
                                        </button>
                                        <button
                                            class="chat-side-x"
                                            title="Move this chat to the trash"
                                            on:click=move |_| remove_saved(state, to_remove.clone())
                                        >
                                            "×"
                                        </button>
                                    </div>
                                }
                            }).collect_view().into_any()
                        }}
                    </div>

                    <div class="chat-main">
                        <div class="chat-log" node_ref=log>
                            {move || state.messages.get().into_iter().map(|message| {
                                let class = match message.role {
                                    ChatRole::User => "turn turn-user",
                                    ChatRole::Assistant | ChatRole::System => "turn turn-assistant",
                                };
                                view! { <div class=class><div class="turn-body">{message.content}</div></div> }
                            }).collect_view()}

                            {move || match state.phase.get() {
                                Phase::Waiting => {
                                    // The words as they arrive. Empty means the model has
                                    // not started, which is a different thing to show
                                    // than an answer that is under way.
                                    let answer = state.streaming.get();
                                    if answer.is_empty() {
                                        view! {
                                            <div class="turn turn-assistant turn-waiting">"•••"</div>
                                        }.into_any()
                                    } else {
                                        view! {
                                            <div class="turn turn-assistant">
                                                <div class="turn-body">{answer}</div>
                                            </div>
                                        }.into_any()
                                    }
                                }
                                Phase::Idle => {
                                    if state.messages.get().is_empty() {
                                        view! {
                                            <p class="chat-welcome">
                                                "Ask the model something. The server holds the provider's key, so nothing here can spend it on a host it was not pointed at."
                                                <br/>
                                                <br/>
                                                "Each conversation is one JSON file in "
                                                <code>"chats/"</code>
                                                ", so Files can read it and the trash can bring it back."
                                            </p>
                                        }.into_any()
                                    } else {
                                        ().into_any()
                                    }
                                }
                                Phase::Failed(err) => view! {
                                    <div class="chat-error">{err}</div>
                                }.into_any(),
                            }}
                        </div>

                        <div class="chat-composer" style=format!("border-color:{stroke};")>
                            <textarea
                                class="chat-input"
                                node_ref=composer
                                rows="2"
                                placeholder=move || if ready() {
                                    "Ask something — Enter to send, Shift+Enter for a new line"
                                } else {
                                    "the server has no AI provider configured"
                                }
                                on:input=move |ev| state.has_text.set(!event_target_value(&ev).trim().is_empty())
                                on:keydown=move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
                                    "Enter" if !ev.shift_key() => {
                                        ev.prevent_default();
                                        send(state, key_ctx.clone(), composer);
                                    }
                                    "Escape" => stop(state),
                                    _ => {}
                                }
                            ></textarea>
                            <button
                                class="chat-btn"
                                style=move || {
                                    if state.phase.get() == Phase::Waiting {
                                        // Stop is the same button, and it should look like the
                                        // thing it now does rather than like Send.
                                        stop_style.clone()
                                    } else {
                                        send_style.clone()
                                    }
                                }
                                disabled=move || {
                                    state.phase.get() != Phase::Waiting
                                        && (!ready() || !state.has_text.get())
                                }
                                on:click=move |_| {
                                    if state.phase.get_untracked() == Phase::Waiting {
                                        stop(state);
                                    } else {
                                        send(state, click_ctx.clone(), composer);
                                    }
                                }
                            >
                                {move || if state.phase.get() == Phase::Waiting { "Stop" } else { "Send" }}
                            </button>
                        </div>

                        <div class="chat-foot">
                            {move || state.notice.get().map(|text| view! {
                                <span class="chat-note">{text}</span>
                            })}
                            <Show when=move || !ready()>
                                <span class="chat-note">
                                    "Set WEB_DESKTOP_AI_URL (and WEB_DESKTOP_AI_KEY) on the server to enable this."
                                </span>
                            </Show>
                            {move || state.usage.get().map(|usage| view! {
                                <span class="chat-tokens" style=format!("color:{muted};")>
                                    {format!("{} tokens", usage.total_tokens)}
                                </span>
                            })}
                        </div>
                    </div>
                </div>
            </div>
        }
    }
}
