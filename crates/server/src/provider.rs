//! Reading a provider's error body, for the two gateways that talk to one.
//!
//! [`crate::ai`] and [`crate::images`] keep separate policies on purpose — the bodies
//! they send, the limits they enforce and the variables they name are different — but a
//! provider's *error* is the same problem in both: it may be JSON in one of several
//! shapes, or just prose, and only so much of it is worth quoting back to whoever has
//! to fix it.

use futures_util::StreamExt;

/// How much of a provider error body is read before it is quoted.
pub const MAX_ERROR_BYTES: usize = 8 * 1024;

/// The provider's message, in its own words, capped.
///
/// OpenAI puts it at `error.message`; other compatible servers put it at `message` or
/// just send prose, so all three are tried before giving up on the body. Best-effort by
/// design: an error that cannot be read still becomes an error, with whatever was
/// readable quoted into it.
pub async fn error_body(response: reqwest::Response) -> String {
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while body.len() < MAX_ERROR_BYTES {
        match chunks.next().await {
            Some(Ok(chunk)) => {
                let room = MAX_ERROR_BYTES - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(room)]);
            }
            Some(Err(err)) => return format!("(the error body could not be read: {err})"),
            None => break,
        }
    }
    let bytes = &body[..];

    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        for path in [["error", "message"], ["message", "message"]] {
            if let Some(message) = value
                .get(path[0])
                .and_then(|error| error.get(path[1]))
                .and_then(serde_json::Value::as_str)
            {
                return message.to_owned();
            }
        }
        if let Some(message) = value.get("error").and_then(serde_json::Value::as_str) {
            return message.to_owned();
        }
    }

    String::from_utf8_lossy(bytes).trim().to_owned()
}
