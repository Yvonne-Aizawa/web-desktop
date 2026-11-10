//! Holding a conversation with a chat model, through the shell's server.
//!
//! The provider's key belongs to the *server*, so an app cannot call a provider
//! directly and does not need to know which one it is. Two calls cover it:
//!
//! ```no_run
//! # async fn example() {
//! use app_sdk::ai::{ChatMessage, ChatRequest};
//!
//! // What is on offer here, so the app can gray out or explain itself.
//! let info = app_sdk::ai::info().await.unwrap();
//! if !info.enabled() {
//!     // the gateway is off; `WEB_DESKTOP_AI_URL` is what turns it on
//!     return;
//! }
//!
//! let request = ChatRequest::new(vec![
//!     ChatMessage::system("Answer in one short sentence."),
//!     ChatMessage::user("Why is the sky blue?"),
//! ])
//! .with_model(info.default_model.clone());
//!
//! let answer = app_sdk::ai::chat(&request).await.unwrap();
//! println!("{}", answer.message.content);
//! # }
//! ```
//!
//! The server also relays a token stream from the same endpoint. [`chat`] waits for the
//! finished answer; with the extra `ai-stream` feature, [`chat_stream`] hands each piece
//! to a callback as it arrives instead:
//!
//! ```ignore
//! # async fn example(request: ChatRequest) {
//! let mut answer = String::new();
//! let streamed = app_sdk::ai::chat_stream(&request, None, |delta| answer.push_str(delta))
//!     .await
//!     .unwrap();
//! // `streamed.stopped` says whether the caller cut it short; `streamed.text` is what
//! // arrived either way, so a partial answer is not thrown away.
//! # }
//! ```
//!
//! Cancellation is the caller's: pass a `web_sys::AbortSignal` and abort it. That is
//! what makes Stop immediate even when the model has not produced its first token yet —
//! a read that has not resolved cannot notice a flag being set.

use gloo_net::http::Request;
use shared::ai::{API_AI_CHAT, API_AI_INFO};

// The wire types live in `shared` so both ends agree by construction; re-exported here
// so an app can name a conversation without depending on `shared` itself.
pub use shared::ai::{
    AiInfo, ChatChunk, ChatMessage, ChatRequest, ChatResponse, ChatRole, ChunkChoice, Delta, Usage,
};

/// What this server's gateway offers. `info().enabled()` is false when it is off.
pub async fn info() -> Result<AiInfo, String> {
    let response = Request::get(API_AI_INFO)
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;

    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(error_message(response, status).await);
    }
    response
        .json::<AiInfo>()
        .await
        .map_err(|err| format!("unexpected response: {err}"))
}

/// Send a conversation and wait for the whole answer.
///
/// Forces `stream: false`: this is the one-piece call, and the server decides what it
/// sends from that flag rather than from what the caller hoped.
pub async fn chat(request: &ChatRequest) -> Result<ChatResponse, String> {
    let mut request = request.clone();
    request.stream = false;

    let response = Request::post(API_AI_CHAT)
        .json(&request)
        .map_err(|err| format!("could not build the request: {err}"))?
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;

    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(error_message(response, status).await);
    }
    response
        .json::<ChatResponse>()
        .await
        .map_err(|err| format!("unexpected response: {err}"))
}

/// The server's own words when it sends an `ApiError`, a status code otherwise.
///
/// Kept the same shape as [`crate::fetch`]: what an app shows on failure is a sentence
/// the server wrote, not a number.
async fn error_message(response: gloo_net::http::Response, status: u16) -> String {
    response
        .json::<shared::fs::ApiError>()
        .await
        .map(|body| body.error)
        .unwrap_or_else(|_| format!("HTTP {status}"))
}

/// What a streamed answer produced.
///
/// `text` is what arrived even when the answer was cut short, because a partial answer is
/// still an answer: throwing it away would leave the app with nothing to show for a
/// request that was already paid for.
#[cfg(feature = "ai-stream")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Streamed {
    pub text: String,
    /// The model's own name for itself, from the first frame that carried one.
    pub model: Option<String>,
    /// Token counts, if the provider volunteered them on a frame.
    pub usage: Option<Usage>,
    /// True when `signal` was aborted, rather than the answer finishing on its own.
    pub stopped: bool,
}

/// Ask for a token stream and hand each piece to `on_delta` as it arrives.
///
/// Set `signal` to cancel: the request is aborted, the connection is closed, and the
/// partial text comes back with [`Streamed::stopped`] set. This is a chat's Stop button —
/// and aborting is what makes it immediate, because a read that is still waiting for the
/// first token would never notice a flag being set.
///
/// The frames are the provider's, relayed unchanged by the server, so this reads the
/// OpenAI shape described by [`ChatChunk`] and ends at the literal `data: [DONE]`. A frame
/// it cannot parse is skipped rather than treated as the end — a field this version does
/// not know about must not be able to silence an answer.
#[cfg(feature = "ai-stream")]
pub async fn chat_stream<F>(
    request: &ChatRequest,
    signal: Option<&web_sys::AbortSignal>,
    mut on_delta: F,
) -> Result<Streamed, String>
where
    F: FnMut(&str),
{
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;

    let aborted = || signal.map(web_sys::AbortSignal::aborted).unwrap_or(false);
    let mut answer = Streamed::default();

    let mut request = request.clone();
    request.stream = true;

    let outgoing = Request::post(API_AI_CHAT)
        .abort_signal(signal)
        .json(&request)
        .map_err(|err| format!("could not build the request: {err}"))?;

    let response = match outgoing.send().await {
        Ok(response) => response,
        // An abort shows up here as a failed request, and it is not a failure.
        Err(_) if aborted() => {
            answer.stopped = true;
            return Ok(answer);
        }
        Err(err) => return Err(format!("request failed: {err}")),
    };

    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(error_message(response, status).await);
    }

    let body = response
        .body()
        .ok_or_else(|| "the server sent no answer body".to_owned())?;
    let reader = body
        .get_reader()
        .dyn_into::<web_sys::ReadableStreamDefaultReader>()
        .map_err(|_| "the answer body is not a byte stream".to_owned())?;

    let mut events = Events::default();
    loop {
        // Also checked between frames: an abort that lands while a chunk is being parsed
        // should not wait for the next one.
        if aborted() {
            answer.stopped = true;
            break;
        }

        let paid = match JsFuture::from(reader.read()).await {
            Ok(paid) => paid,
            Err(_) if aborted() => {
                answer.stopped = true;
                break;
            }
            Err(err) => return Err(format!("reading the answer: {}", describe(&err))),
        };

        if field(&paid, "done").and_then(|done| done.as_bool()) == Some(true) {
            break;
        }
        let value = field(&paid, "value").unwrap_or(JsValue::NULL);
        let chunk = value
            .dyn_into::<js_sys::Uint8Array>()
            .map_err(|_| "the server sent a frame that is not bytes".to_owned())?;

        let mut finished = false;
        for payload in events.push(&chunk.to_vec()) {
            if payload.trim() == "[DONE]" {
                finished = true;
                break;
            }
            let Ok(frame) = serde_json::from_str::<ChatChunk>(&payload) else {
                continue;
            };
            if answer.model.is_none() {
                answer.model = frame.model.clone();
            }
            if frame.usage.is_some() {
                answer.usage = frame.usage;
            }
            for choice in frame.choices {
                if let Some(delta) = choice.delta.content {
                    answer.text.push_str(&delta);
                    on_delta(&delta);
                }
            }
        }
        if finished {
            break;
        }
    }

    // Close the connection rather than leaving it to be dropped. On a stop this is what
    // the server notices: its own upstream stream is dropped with the client's, and the
    // provider stops generating tokens nobody will read.
    if answer.stopped {
        let _ = JsFuture::from(reader.cancel()).await;
    }
    Ok(answer)
}

/// `Reflect::get`, for reading one field out of a JS object.
#[cfg(feature = "ai-stream")]
fn field(object: &wasm_bindgen::JsValue, name: &str) -> Option<wasm_bindgen::JsValue> {
    js_sys::Reflect::get(object, &wasm_bindgen::JsValue::from_str(name)).ok()
}

/// A JS value as something worth putting in an error message.
#[cfg(feature = "ai-stream")]
fn describe(value: &wasm_bindgen::JsValue) -> String {
    if let Some(text) = value.as_string() {
        return text;
    }
    if let Some(message) = field(value, "message").and_then(|it| it.as_string()) {
        return message;
    }
    format!("{value:?}")
}

/// Server-sent events, reassembled from byte chunks that arrive wherever the network
/// felt like splitting them: an event can span chunks, and a chunk can hold several.
///
/// Splitting on `\n` at the **byte** level is safe for UTF-8 — no continuation byte is
/// `\n` — so there is no `TextDecoder` here, and no way for a multi-byte character to be
/// mangled because its bytes landed on either side of a chunk boundary. That is the whole
/// reason this is a struct with a buffer instead of a `String::from_utf8` per chunk.
#[cfg(feature = "ai-stream")]
#[derive(Default)]
struct Events {
    pending: Vec<u8>,
}

#[cfg(feature = "ai-stream")]
impl Events {
    /// Add a chunk, and take every event it completed, in order.
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);

        let mut events = Vec::new();
        while let Some(end) = blank_line(&self.pending) {
            let block = self.pending[..end.0].to_vec();
            self.pending.drain(..end.1);
            if let Some(payload) = data_lines(&block) {
                events.push(payload);
            }
        }
        events
    }
}

/// Where the next event ends: `(where the blank line starts, where it ends)`.
///
/// `None` while the event is still incomplete, which is the normal state between chunks.
#[cfg(feature = "ai-stream")]
fn blank_line(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        // `\r\n` is a line ending too.
        let line = bytes[start..index]
            .strip_suffix(b"\r")
            .unwrap_or(&bytes[start..index]);
        if line.is_empty() {
            return Some((start, index + 1));
        }
        start = index + 1;
    }
    None
}

/// One event's payload: its `data:` lines, joined by newlines, or `None` when the event
/// carries no data at all — which is what a comment or a keep-alive looks like, and what
/// this should hand to a JSON parser never.
#[cfg(feature = "ai-stream")]
fn data_lines(block: &[u8]) -> Option<String> {
    let mut data: Vec<&[u8]> = Vec::new();
    for line in block.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(rest) = line.strip_prefix(b"data:") else {
            continue;
        };
        // The single space after the colon is part of the framing, not the payload.
        let rest = rest.strip_prefix(b" ").unwrap_or(rest);
        data.push(rest);
    }

    if data.is_empty() {
        return None;
    }
    let mut payload = Vec::new();
    for (index, line) in data.iter().enumerate() {
        if index > 0 {
            payload.push(b'\n');
        }
        payload.extend_from_slice(line);
    }
    // Lossy on purpose: a payload that is not valid UTF-8 cannot be the JSON we are here
    // for, and returning something lets the caller skip it like any other unparseable
    // frame instead of failing the whole answer.
    Some(String::from_utf8_lossy(&payload).into_owned())
}

#[cfg(all(test, feature = "ai-stream"))]
mod stream_tests {
    use super::{Events, Streamed};

    fn payloads(chunks: &[&[u8]]) -> Vec<String> {
        let mut events = Events::default();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(events.push(chunk));
        }
        out
    }

    #[test]
    fn one_event_per_chunk() {
        assert_eq!(
            payloads(&[b"data: {\"a\":1}\n\n", b"data: {\"b\":2}\n\n"]),
            vec!["{\"a\":1}", "{\"b\":2}"]
        );
    }

    #[test]
    fn an_event_split_across_chunks_waits_for_the_rest() {
        assert_eq!(payloads(&[b"data: {\"a\":"]), Vec::<String>::new());
        assert_eq!(payloads(&[b"data: {\"a\":", b"1}\n\n"]), vec!["{\"a\":1}"]);
    }

    #[test]
    fn several_events_in_one_chunk() {
        assert_eq!(
            payloads(&[b"data: 1\n\ndata: 2\n\ndata: 3\n\n"]),
            vec!["1", "2", "3"]
        );
    }

    #[test]
    fn carriage_returns_are_a_line_ending_too() {
        assert_eq!(
            payloads(&[b"data: 1\r\n\r\ndata: 2\r\n\r\n"]),
            vec!["1", "2"]
        );
    }

    #[test]
    fn a_character_split_across_chunks_is_reassembled() {
        // "data: é\n\n": six bytes of framing, then the two bytes of `é`, then the blank
        // line. Splitting between those two bytes is what would break a decoder that
        // worked a chunk at a time.
        let whole = "data: é\n\n".as_bytes();
        assert_eq!(whole.len(), 10);
        assert_eq!(payloads(&[&whole[..7], &whole[7..]]), vec!["é"]);
    }

    #[test]
    fn data_lines_are_joined_and_other_fields_are_ignored() {
        assert_eq!(
            payloads(&[b"event: message\nid: 7\ndata: one\ndata: two\n\n"]),
            vec!["one\ntwo"]
        );
        // A comment-only block is a keep-alive, and carries nothing to parse.
        assert_eq!(payloads(&[b": ping\n\n"]), Vec::<String>::new());
        assert_eq!(payloads(&[b"\n\n"]), Vec::<String>::new());
    }

    #[test]
    fn the_space_after_the_colon_is_optional() {
        assert_eq!(
            payloads(&[b"data:{\"a\":1}\n\ndata: 2\n\n"]),
            vec!["{\"a\":1}", "2"]
        );
    }

    #[test]
    fn an_unterminated_event_is_held_back() {
        // No blank line yet, so nothing is handed out — and then it is, once it ends.
        assert_eq!(payloads(&[b"data: 1\n"]), Vec::<String>::new());
        assert_eq!(payloads(&[b"data: 1\n", b"\n"]), vec!["1"]);
    }

    #[test]
    fn a_stream_that_was_cut_short_keeps_what_arrived() {
        let stopped = Streamed {
            text: "half an answ".to_owned(),
            stopped: true,
            ..Streamed::default()
        };
        assert_eq!(stopped.text, "half an answ");
        assert!(stopped.stopped);
        assert!(stopped.model.is_none());
        assert!(stopped.usage.is_none());
        assert_eq!(Streamed::default().text, "");
    }
}
