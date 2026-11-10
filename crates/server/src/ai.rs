//! A chat gateway: the server talks to an OpenAI-compatible provider so the browser
//! never holds the provider's key.
//!
//! # Why this is not the outbound proxy
//!
//! `crate::proxy` exists so an app can *read* a third-party URL that sends no CORS
//! headers, and it is fenced accordingly: `GET` only, an allow-list of hosts, a
//! private-address check, a request body nobody controls. Chat needs the opposite
//! shape — a `POST` with a JSON body that the caller composes, answered over minutes
//! rather than a 10-second budget, and streamed a token at a time. Widening the proxy
//! into that would blunt the checks that make it safe, so this is a second module with
//! its own policy.
//!
//! The policy here is simpler, because the caller never names an address:
//!
//! * **The URL is configuration.** `WEB_DESKTOP_AI_URL` is the only place a host comes
//!   from, so there is no SSRF surface to fence. An app cannot ask this gateway to
//!   talk to anything the operator did not point it at.
//! * **The models are an allow-list** (`WEB_DESKTOP_AI_MODELS`). The one thing a caller
//!   *can* choose is which model, and a model is a bill, so the choice is bounded.
//! * **The key is the server's.** It is read from the environment, never logged, never
//!   sent to the browser and never in the catalog payload.
//! * **Off until asked for**, like the proxy. An unset `WEB_DESKTOP_AI_URL` means the
//!   endpoint answers "off" and names the variable to set, rather than 404ing.
//! * Bounded: message count and total size in, concurrency cap out, and an idle timeout
//!   between stream frames — but **no whole-request timeout on a stream**, because that
//!   would cut a long answer off mid-sentence.
//!
//! What this does not do: cap what a single answer costs (no `max_tokens` is sent, since
//! the field name differs between providers and set it wrong and every request fails),
//! and it does not rate-limit per caller beyond [`MAX_IN_FLIGHT`].

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderName, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use shared::{
    ai::{
        API_AI_CHAT, API_AI_INFO, AiInfo, ChatMessage, ChatRequest, ChatResponse, ChatRole, Usage,
    },
    fs::ApiError,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{AppState, fs::same_origin, provider};

/// Messages in one request. Well above a long conversation, low enough that a runaway
/// client cannot make the server build an unbounded body.
pub const MAX_MESSAGES: usize = 200;
/// Characters across all messages, which is the real cost bound.
pub const MAX_CHARS: usize = 400_000;
/// Largest answer body read from the provider when it is not streaming.
const MAX_ANSWER_BYTES: usize = 4 * 1024 * 1024;
/// Whole-request budget when the answer is not streamed.
pub const TIMEOUT_SECS: u64 = 180;
/// Budget for establishing the connection.
const CONNECT_TIMEOUT_SECS: u64 = 10;
/// Longest gap between two stream frames. This is what stops a stalled stream from
/// holding a slot forever, without capping how long a long answer may take.
const IDLE_TIMEOUT_SECS: u64 = 60;
/// Requests to the provider in flight at once. A local model has no queue worth
/// speaking of, and a rate-limited key does not want to be hit by everything at once.
const MAX_IN_FLIGHT: usize = 4;

/// The gateway's configuration, derived from the environment once at startup.
#[derive(Debug)]
pub struct Ai {
    client: reqwest::Client,
    /// Provider base URL with no trailing slash, e.g. `https://api.openai.com/v1`.
    base_url: String,
    /// The provider key. Empty is allowed — a local provider may not want one — but
    /// then the operator is warned at startup, because hosted providers will 401.
    key: String,
    default_model: String,
    /// Every model a request may name. Empty means the gateway is off.
    models: Vec<String>,
    in_flight: Arc<Semaphore>,
}

impl Ai {
    /// `WEB_DESKTOP_AI_URL` plus, optionally, `WEB_DESKTOP_AI_KEY`,
    /// `WEB_DESKTOP_AI_MODEL` and `WEB_DESKTOP_AI_MODELS`.
    ///
    /// `None` when no URL is set, which is what "off" means here.
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("WEB_DESKTOP_AI_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty())?;

        let key = std::env::var("WEB_DESKTOP_AI_KEY")
            .unwrap_or_default()
            .trim()
            .to_owned();

        let mut default_model = std::env::var("WEB_DESKTOP_AI_MODEL")
            .unwrap_or_default()
            .trim()
            .to_owned();

        let mut models: Vec<String> = Vec::new();
        for model in std::env::var("WEB_DESKTOP_AI_MODELS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|model| !model.is_empty())
        {
            if !models.iter().any(|seen| seen == model) {
                models.push(model.to_owned());
            }
        }

        // Three ways this can be configured, and one of them is a mistake worth naming:
        // a default the allow-list does not contain would make every default request
        // fail, so the allow-list wins and the default is moved into it.
        if models.is_empty() && !default_model.is_empty() {
            models.push(default_model.clone());
        } else if !models.is_empty() && !models.iter().any(|model| model == &default_model) {
            if !default_model.is_empty() {
                tracing::warn!(
                    "ai: WEB_DESKTOP_AI_MODEL={default_model} is not in WEB_DESKTOP_AI_MODELS; \
                     using {} instead",
                    models[0]
                );
            }
            default_model = models[0].clone();
        }

        Some(Self::with_config(base_url, key, default_model, models))
    }

    fn with_config(
        base_url: String,
        key: String,
        default_model: String,
        models: Vec<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
            // Bounds the gap between frames, so it applies to a stream too. There is
            // deliberately no whole-request timeout: see `chat`.
            .read_timeout(Duration::from_secs(IDLE_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            base_url,
            key,
            default_model,
            models,
            in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        }
    }

    /// What the catalog and Settings get to see. No key, and no base URL either: an
    /// app has no use for the provider's address, and not sending it keeps the address
    /// out of the browser entirely.
    pub fn info(&self) -> AiInfo {
        AiInfo {
            default_model: self.default_model.clone(),
            models: self.models.clone(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn has_key(&self) -> bool {
        !self.key.is_empty()
    }

    /// The model a request will actually use, or why it cannot.
    fn resolve_model(&self, requested: Option<&str>) -> Result<String, AiError> {
        let model = match requested.map(str::trim) {
            Some(model) if !model.is_empty() => model.to_owned(),
            _ => self.default_model.clone(),
        };

        if model.is_empty() {
            return Err(AiError::Denied(
                "no model is configured on this server; set WEB_DESKTOP_AI_MODEL".to_owned(),
            ));
        }
        if !self.models.iter().any(|allowed| allowed == &model) {
            return Err(AiError::BadRequest(format!(
                "`{model}` is not one of the models this server offers ({}); widen \
                 WEB_DESKTOP_AI_MODELS to allow it",
                self.models.join(", ")
            )));
        }
        Ok(model)
    }
}

/// `GET` for [`AiInfo`], `POST` for a conversation.
///
/// Answers even when the gateway is off, so an app can explain itself instead of
/// treating a 404 as a bug.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(API_AI_INFO, get(info))
        .route(API_AI_CHAT, post(chat))
        // The same guard the file API and the proxy have. This endpoint spends the
        // operator's key, so a random page must not be able to cause a request — and
        // unlike a read, that is true even though the request is not "a write".
        .route_layer(middleware::from_fn(same_origin))
}

async fn info(State(state): State<Arc<AppState>>) -> Json<AiInfo> {
    Json(state.ai.as_ref().map(Ai::info).unwrap_or_default())
}

async fn chat(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ChatRequest>,
) -> Result<Response, AiError> {
    let ai = state.ai.as_ref().ok_or_else(|| {
        AiError::Denied(
            "the AI gateway is off; start the server with WEB_DESKTOP_AI_URL set to an \
             OpenAI-compatible endpoint"
                .to_owned(),
        )
    })?;

    validate(&request)?;
    let model = ai.resolve_model(request.model.as_deref())?;

    // Built field by field rather than by forwarding the caller's JSON: what is sent is
    // exactly what this version means to send, so a field added to the request type
    // cannot reach the provider by accident.
    let body = serde_json::json!({
        "model": model,
        "messages": request
            .messages
            .iter()
            .map(|message| serde_json::json!({
                "role": message.role,
                "content": message.content,
            }))
            .collect::<Vec<_>>(),
        "stream": request.stream,
    });

    // A slot is taken before the request goes out and held for as long as the answer
    // lasts — for a stream, that is the whole stream, so a client that stops reading
    // releases it by dropping the response.
    let permit = Arc::clone(&ai.in_flight)
        .acquire_owned()
        .await
        .map_err(|_| AiError::Internal("the gateway is shutting down".to_owned()))?;

    let mut outgoing = ai
        .client
        .post(format!("{}/chat/completions", ai.base_url))
        .json(&body);

    if ai.has_key() {
        outgoing = outgoing.bearer_auth(&ai.key);
    }
    if !request.stream {
        // Only the unstreamed path gets a deadline. Applied to a stream this would cut
        // a long answer off partway through, which is the one thing a chat client
        // cannot recover from — `read_timeout` already covers a stalled connection.
        outgoing = outgoing.timeout(Duration::from_secs(TIMEOUT_SECS));
    }

    tracing::debug!("ai: {} -> {}/chat/completions", model, ai.base_url);

    let response = outgoing.send().await.map_err(|err| {
        if err.is_timeout() {
            AiError::Timeout(format!(
                "the provider did not answer within {TIMEOUT_SECS}s"
            ))
        } else {
            AiError::Upstream(format!("could not reach the provider: {err}"))
        }
    })?;

    if !response.status().is_success() {
        return Err(provider_error(response).await);
    }

    if request.stream {
        Ok(stream_response(response, permit))
    } else {
        drop(permit);
        complete(response, model).await
    }
}

/// One answer, in one piece.
async fn complete(response: reqwest::Response, model: String) -> Result<Response, AiError> {
    // Read with a ceiling rather than trusting `content-length`, which a provider may
    // omit or lie about — the same reason the proxy reads its body by hand.
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|err| AiError::Upstream(format!("reading the answer: {err}")))?;
        if body.len() + chunk.len() > MAX_ANSWER_BYTES {
            return Err(AiError::Upstream(format!(
                "the provider's answer is larger than {MAX_ANSWER_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }

    let answer: ProviderCompletion = serde_json::from_slice(&body).map_err(|err| {
        AiError::Upstream(format!(
            "the provider sent something that is not an OpenAI completion: {err}"
        ))
    })?;

    let content = answer
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message)
        // `content` is null for a provider that answered with a tool call, which this
        // desktop does not send. An empty answer is the honest report of that.
        .and_then(|message| message.content)
        .unwrap_or_default();

    Ok(Json(ChatResponse {
        // The provider's own word for which model answered, falling back to what was
        // asked for when it does not say.
        model: answer.model.unwrap_or(model),
        message: ChatMessage {
            role: ChatRole::Assistant,
            content,
        },
        usage: answer.usage,
    })
    .into_response())
}

/// The provider's bytes, relayed as they arrive.
///
/// Deliberately a relay rather than a re-encoding: the frames are the OpenAI ones the
/// caller asked for, so any OpenAI client can be pointed at this endpoint, and a field
/// the provider adds is not silently dropped on the way through. Errors *before* the
/// first frame are caught above and returned as JSON, which is why nothing is parsed
/// here — a caller can never be halfway through a stream when the bad news arrives.
///
/// Dropping the stream is how a caller stops paying: axum drops the body when the
/// client goes away, which drops the upstream response, which closes the provider
/// connection. The permit rides along with the stream for the same reason.
fn stream_response(response: reqwest::Response, permit: OwnedSemaphorePermit) -> Response {
    let frames = stream::unfold(
        (permit, response.bytes_stream()),
        |(permit, mut chunks)| async move { chunks.next().await.map(|frame| (frame, (permit, chunks))) },
    );

    let mut out = Response::new(Body::from_stream(frames));
    let headers = out.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    // A conversation is not the shell's to cache, and the handler's policy has to win
    // over the middleware's default for anything under `/api`.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // So a reverse proxy in front of this does not sit on the frames until the answer
    // is finished, which is exactly the behaviour that makes streaming pointless.
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    out
}

/// Turn a provider failure into something the app can show a person.
///
/// The provider's own words are kept, because "insufficient_quota" is worth more to
/// whoever has to fix it than "bad gateway" is, and the status is remapped: a 401 from
/// the provider is *this server's* configuration problem, not the caller's, so it must
/// not arrive looking like the caller's own credentials were rejected.
async fn provider_error(response: reqwest::Response) -> AiError {
    let status = response.status();
    let detail = provider::error_body(response).await;
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };

    match status.as_u16() {
        429 => AiError::RateLimited(format!("the provider is rate limiting this server{detail}")),
        400 | 422 => AiError::BadRequest(format!("the provider rejected the request{detail}")),
        401 | 403 => AiError::Upstream(format!(
            "the provider rejected this server's API key; check WEB_DESKTOP_AI_KEY{detail}"
        )),
        404 => AiError::Upstream(format!(
            "the provider knows no such endpoint or model{detail}"
        )),
        _ => AiError::Upstream(format!("the provider answered {status}{detail}")),
    }
}

/// What a request has to look like before anything is sent.
fn validate(request: &ChatRequest) -> Result<(), AiError> {
    if request.messages.is_empty() {
        return Err(AiError::BadRequest(
            "the request has no messages".to_owned(),
        ));
    }
    if request.messages.len() > MAX_MESSAGES {
        return Err(AiError::BadRequest(format!(
            "{} messages; the limit is {MAX_MESSAGES}",
            request.messages.len()
        )));
    }

    let characters: usize = request
        .messages
        .iter()
        .map(|message| message.content.chars().count())
        .sum();
    if characters > MAX_CHARS {
        return Err(AiError::BadRequest(format!(
            "{characters} characters of conversation; the limit is {MAX_CHARS}"
        )));
    }
    Ok(())
}

/// The parts of an OpenAI completion this server reads. Everything else the provider
/// sends is ignored rather than modelled.
#[derive(Debug, Deserialize)]
struct ProviderCompletion {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ProviderChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct ProviderChoice {
    #[serde(default)]
    message: Option<ProviderMessage>,
}

#[derive(Debug, Deserialize)]
struct ProviderMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug)]
pub enum AiError {
    BadRequest(String),
    /// Refused before anything went out.
    Denied(String),
    RateLimited(String),
    Timeout(String),
    Upstream(String),
    Internal(String),
}

impl AiError {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Denied(_) => StatusCode::FORBIDDEN,
            Self::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::BadRequest(message)
            | Self::Denied(message)
            | Self::RateLimited(message)
            | Self::Timeout(message)
            | Self::Upstream(message)
            | Self::Internal(message) => message,
        }
    }
}

impl IntoResponse for AiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = self.message().to_owned();
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            tracing::warn!("ai: {status}: {message}");
        } else {
            tracing::debug!("ai: {status}: {message}");
        }
        (status, Json(ApiError { error: message })).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post as axum_post;

    fn gateway(base_url: &str, default_model: &str, models: &[&str]) -> Ai {
        Ai::with_config(
            base_url.trim_end_matches('/').to_owned(),
            "test-key".to_owned(),
            default_model.to_owned(),
            models.iter().map(|model| (*model).to_owned()).collect(),
        )
    }

    fn request(model: Option<&str>, stream: bool) -> ChatRequest {
        ChatRequest {
            messages: vec![ChatMessage::user("hello")],
            model: model.map(str::to_owned),
            stream,
        }
    }

    #[test]
    fn the_default_model_is_omitted_and_the_default_is_used() {
        let ai = gateway("http://example.test/v1", "small", &["small", "big"]);
        assert_eq!(ai.resolve_model(None).unwrap(), "small");
        assert_eq!(ai.resolve_model(Some("  ")).unwrap(), "small");
        assert_eq!(ai.resolve_model(Some("big")).unwrap(), "big");
    }

    #[test]
    fn a_model_outside_the_allow_list_is_refused_by_name() {
        let ai = gateway("http://example.test/v1", "small", &["small"]);
        let err = ai.resolve_model(Some("expensive")).unwrap_err();
        assert!(matches!(err, AiError::BadRequest(_)), "{err:?}");
        assert!(err.message().contains("expensive"), "{}", err.message());
        assert!(err.message().contains("small"), "{}", err.message());
    }

    #[test]
    fn a_gateway_with_no_model_says_which_variable_to_set() {
        let ai = gateway("http://example.test/v1", "", &[]);
        let err = ai.resolve_model(None).unwrap_err();
        assert!(matches!(err, AiError::Denied(_)), "{err:?}");
        assert!(
            err.message().contains("WEB_DESKTOP_AI_MODEL"),
            "{}",
            err.message()
        );
        assert!(!ai.info().enabled());
    }

    #[test]
    fn an_empty_conversation_is_refused() {
        let empty = ChatRequest {
            messages: Vec::new(),
            model: None,
            stream: false,
        };
        assert!(matches!(validate(&empty), Err(AiError::BadRequest(_))));
    }

    #[test]
    fn too_many_messages_or_too_many_characters_are_both_refused() {
        let many = ChatRequest {
            messages: vec![ChatMessage::user("hi"); MAX_MESSAGES + 1],
            model: None,
            stream: false,
        };
        assert!(matches!(validate(&many), Err(AiError::BadRequest(_))));

        let long = ChatRequest {
            messages: vec![ChatMessage::user("x".repeat(MAX_CHARS + 1))],
            model: None,
            stream: false,
        };
        let err = validate(&long).unwrap_err();
        assert!(err.message().contains("characters"), "{}", err.message());
    }

    #[test]
    fn a_conversation_at_the_limit_is_allowed() {
        let at_limit = ChatRequest {
            messages: vec![ChatMessage::user("x".repeat(MAX_CHARS))],
            model: None,
            stream: false,
        };
        assert!(validate(&at_limit).is_ok());
    }

    /// A stand-in for a provider: it answers the two shapes a real one does, and one
    /// failure, so the gateway can be exercised without a key or a network.
    async fn fake_provider(mode: &'static str) -> String {
        let app = Router::new().route(
            "/v1/chat/completions",
            axum_post(move |Json(body): Json<serde_json::Value>| async move {
                match mode {
                    "complete" => (
                        [(header::CONTENT_TYPE, "application/json")],
                        serde_json::json!({
                            "model": "fake-1",
                            "choices": [{"message": {"role": "assistant", "content": "hi there"}}],
                            "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5},
                        })
                        .to_string(),
                    )
                        .into_response(),
                    "stream" => {
                        assert_eq!(body["stream"], true);
                        let frames = "data: {\"model\":\"fake-1\",\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
                                      data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                                      data: {\"choices\":[{\"delta\":{\"content\":\" there\"}}]}\n\n\
                                      data: [DONE]\n\n";
                        (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            frames,
                        )
                            .into_response()
                    }
                    _ => (
                        StatusCode::UNAUTHORIZED,
                        [(header::CONTENT_TYPE, "application/json")],
                        r#"{"error":{"message":"Incorrect API key provided"}}"#,
                    )
                        .into_response(),
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/v1")
    }

    fn state_with(ai: Ai) -> Arc<AppState> {
        Arc::new(AppState {
            started: std::time::Instant::now(),
            catalog: shared::default_catalog(),
            apps_dir: None,
            proxy: None,
            fs: None,
            ai: Some(ai),
            images: None,
        })
    }

    async fn read_body(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn a_finished_answer_becomes_a_chat_response() {
        let base = fake_provider("complete").await;
        let state = state_with(gateway(&base, "fake-1", &["fake-1"]));

        let response = chat(State(state), Json(request(None, false)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body: serde_json::Value = serde_json::from_str(&read_body(response).await).unwrap();
        assert_eq!(body["message"]["role"], "assistant");
        assert_eq!(body["message"]["content"], "hi there");
        assert_eq!(body["model"], "fake-1");
        assert_eq!(body["usage"]["total_tokens"], 5);
    }

    #[tokio::test]
    async fn a_streamed_answer_arrives_frame_by_frame() {
        let base = fake_provider("stream").await;
        let state = state_with(gateway(&base, "fake-1", &["fake-1"]));

        let response = chat(State(state), Json(request(None, true))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );
        // Without these two, something in front of the server may hold the frames back
        // until the answer is over, which is the whole point of streaming undone.
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        assert!(response.headers().contains_key("x-accel-buffering"));

        let frames = read_body(response).await;
        // Relayed, not re-encoded: the provider's own bytes, including `[DONE]`.
        assert!(frames.contains(r#""content":"hi""#), "{frames}");
        assert!(frames.contains(r#""content":" there""#), "{frames}");
        assert!(frames.trim_end().ends_with("data: [DONE]"), "{frames}");
    }

    #[tokio::test]
    async fn a_provider_auth_failure_reads_as_the_servers_problem_not_the_callers() {
        let base = fake_provider("unauthorized").await;
        let state = state_with(gateway(&base, "fake-1", &["fake-1"]));

        let err = chat(State(state), Json(request(None, false)))
            .await
            .unwrap_err();
        // 502, not the provider's 401: the caller's own request was fine, and telling
        // them 401 would send them looking for a credential they do not have.
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
        assert!(
            err.message().contains("WEB_DESKTOP_AI_KEY"),
            "{}",
            err.message()
        );
        // The provider's own words are kept, because they are what names the fix.
        assert!(
            err.message().contains("Incorrect API key"),
            "{}",
            err.message()
        );
    }

    #[tokio::test]
    async fn a_bad_model_is_refused_before_anything_is_sent() {
        // A provider that would panic if it were reached: the assertion inside the fake
        // is what proves no request went out.
        let state = state_with(gateway("http://127.0.0.1:1/v1", "small", &["small"]));
        let request = request(Some("expensive"), false);

        let err = chat(State(state), Json(request)).await.unwrap_err();
        assert!(matches!(err, AiError::BadRequest(_)), "{err:?}");
    }

    #[tokio::test]
    async fn the_gateway_being_off_is_explained_rather_than_404() {
        let state = Arc::new(AppState {
            started: std::time::Instant::now(),
            catalog: shared::default_catalog(),
            apps_dir: None,
            proxy: None,
            fs: None,
            ai: None,
            images: None,
        });

        let err = chat(State(state.clone()), Json(request(None, false)))
            .await
            .unwrap_err();
        assert!(matches!(err, AiError::Denied(_)), "{err:?}");
        assert!(
            err.message().contains("WEB_DESKTOP_AI_URL"),
            "{}",
            err.message()
        );

        // And asking what is on offer is still a 200, with an empty list.
        let Json(info) = info(State(state)).await;
        assert!(!info.enabled());
    }

    #[test]
    fn an_unset_url_means_the_gateway_is_off() {
        // Only the URL is required; without one there is nothing to be off *from*.
        let saved = std::env::var_os("WEB_DESKTOP_AI_URL");
        unsafe { std::env::remove_var("WEB_DESKTOP_AI_URL") };
        assert!(Ai::from_env().is_none());
        if let Some(value) = saved {
            unsafe { std::env::set_var("WEB_DESKTOP_AI_URL", value) };
        }
    }
}
