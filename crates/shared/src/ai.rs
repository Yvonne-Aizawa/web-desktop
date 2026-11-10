//! Talking to a chat model, through the server.
//!
//! The provider's key is the *server's*, so it never reaches the browser and an app
//! never holds one. An app names messages and a model from the list the server
//! advertises — never a host and never a URL:
//!
//! ```text
//! app ──POST /api/ai/chat──▶ server ──POST {WEB_DESKTOP_AI_URL}/chat/completions──▶ provider
//! ```
//!
//! That is the whole security story here, and it is deliberately a different shape from
//! [`crate::proxy`]: there is no caller-supplied address, so there is nothing to fence.
//! The model list is the one thing a caller can choose, which is why the server keeps
//! it to what `WEB_DESKTOP_AI_MODELS` allows.

use serde::{Deserialize, Serialize};

/// `POST` a [`ChatRequest`]: a [`ChatResponse`], or — with `stream` — an SSE stream of
/// [`ChatChunk`] frames ending in `data: [DONE]`.
pub const API_AI_CHAT: &str = "/api/ai/chat";
/// `GET` -> [`AiInfo`]. Answers even when the gateway is off, so an app can say so.
pub const API_AI_INFO: &str = "/api/ai/info";

/// What the server's AI gateway is configured to do.
///
/// Carried in [`crate::Catalog`] as well, so Settings can report it the way it reports
/// the outbound proxy — an app should not have to ask before it can explain itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AiInfo {
    /// Used when a request does not name a model.
    pub default_model: String,
    /// Every model a request may ask for. Empty means the gateway is off or has no
    /// model configured, and [`AiInfo::enabled`] is false.
    pub models: Vec<String>,
}

impl AiInfo {
    pub fn enabled(&self) -> bool {
        !self.models.is_empty()
    }
}

/// Who said it. The subset of the OpenAI roles this desktop uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }
}

/// Request body for [`API_AI_CHAT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    /// A model from [`AiInfo::models`]. `None` means the server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Ask for one frame per token instead of one finished answer.
    #[serde(default)]
    pub stream: bool,
}

impl ChatRequest {
    /// A request for the server's default model, answered in one piece.
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            model: None,
            stream: false,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

/// A finished answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatResponse {
    /// The model that actually answered, which is the provider's word, not the
    /// request's.
    pub model: String,
    pub message: ChatMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// Token counts, when the provider reports them. Every field is optional on the wire:
/// the shape is the provider's, and not every provider fills all of it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

/// One frame of a streamed answer.
///
/// The server relays the provider's own SSE bytes rather than re-encoding them, so
/// this type **describes** the OpenAI-compatible shape the frames arrive in; it does not
/// impose one. A reply is the concatenation of `choices[0].delta.content` across frames,
/// and the stream ends with a literal `data: [DONE]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChatChunk {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<ChunkChoice>,
    /// Token counts, on the last frame that carries them. OpenAI sends this only when
    /// the request asks with `stream_options`, which this server does not set — so it is
    /// usually absent, and a provider that volunteers it anyway is worth having.
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChunkChoice {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub delta: Delta,
    /// `Some("stop")` and friends on the last frame that carries content.
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Delta {
    /// Present on the first frame of most providers' streams.
    #[serde(default)]
    pub role: Option<ChatRole>,
    #[serde(default)]
    pub content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{AiInfo, ChatChunk, ChatMessage, ChatRequest, ChatRole};

    #[test]
    fn a_gateway_with_no_model_is_off() {
        assert!(!AiInfo::default().enabled());
        assert!(
            AiInfo {
                default_model: "m".to_owned(),
                models: vec!["m".to_owned()],
            }
            .enabled()
        );
    }

    #[test]
    fn roles_are_the_lowercase_words_the_provider_uses() {
        for (role, word) in [
            (ChatRole::System, "\"system\""),
            (ChatRole::User, "\"user\""),
            (ChatRole::Assistant, "\"assistant\""),
        ] {
            assert_eq!(serde_json::to_string(&role).unwrap(), word);
        }
        assert_eq!(
            serde_json::from_str::<ChatRole>("\"assistant\"").unwrap(),
            ChatRole::Assistant
        );
    }

    #[test]
    fn an_unset_model_is_omitted_rather_than_sent_as_null() {
        let request = ChatRequest::new(vec![ChatMessage::user("hi")]);
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"messages":[{"role":"user","content":"hi"}],"stream":false}"#
        );
    }

    #[test]
    fn a_delta_frame_survives_being_partly_filled_in() {
        // The first frame carries only a role, the last only a finish reason, and some
        // providers send `content: null` on both.
        let first: ChatChunk =
            serde_json::from_str(r#"{"model":"m","choices":[{"delta":{"role":"assistant"}}]}"#)
                .unwrap();
        assert_eq!(first.choices[0].delta.role, Some(ChatRole::Assistant));
        assert_eq!(first.choices[0].delta.content, None);

        let last: ChatChunk = serde_json::from_str(
            r#"{"choices":[{"index":0,"delta":{"content":null},"finish_reason":"stop"}]}"#,
        )
        .unwrap();
        assert_eq!(last.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(last.choices[0].delta.content, None);

        let empty: ChatChunk = serde_json::from_str("{}").unwrap();
        assert!(empty.choices.is_empty());
        assert!(empty.usage.is_none());
    }

    #[test]
    fn a_chunk_may_carry_usage_and_no_choices() {
        // The shape a provider uses for a final accounting frame.
        let frame: ChatChunk = serde_json::from_str(
            r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}}"#,
        )
        .unwrap();
        assert!(frame.choices.is_empty());
        assert_eq!(frame.usage.unwrap().total_tokens, 7);
    }
}
