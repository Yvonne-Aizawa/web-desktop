//! An image gateway: the server talks to an OpenAI-compatible image provider so the
//! browser never holds the provider's key.
//!
//! # Why this is its own module
//!
//! It is [`crate::ai`]'s sibling, and deliberately a separate one. Chat and image
//! generation are different bills with different failure modes — a chat provider streams
//! for minutes, an image provider takes a while and answers with megabytes of base64 —
//! and an operator may well point them at two different services. So each has its own
//! configuration (`WEB_DESKTOP_IMAGE_*` beside `WEB_DESKTOP_AI_*`), its own allow-lists
//! and its own concurrency cap.
//!
//! What it shares with the chat gateway is the policy, and it is short because the
//! caller never names an address:
//!
//! * **The URL is configuration.** `WEB_DESKTOP_IMAGE_URL` is the only place a host
//!   comes from, so there is no SSRF surface to fence.
//! * **The models and the sizes are allow-lists.** They are the two things a caller can
//!   choose, and both are a bill — a larger image costs more — so both are bounded by
//!   configuration. The count is capped at what the provider itself accepts.
//! * **Reference images for img2img are bounded and checked.** This is the one place a
//!   caller supplies file content rather than a name, so the number, each size, the total,
//!   the base64 shape and the file signature are all checked before anything is forwarded.
//! * **The key is the server's.** Read from the environment, never logged, never sent to
//!   the browser and never in the catalog payload.
//! * **Off until asked for.** An unset `WEB_DESKTOP_IMAGE_URL` means the endpoint
//!   answers "off" and names the variable to set, rather than 404ing.
//! * Bounded: prompt length in, reference images checked, a body ceiling out, a
//!   concurrency cap, and one timeout for the whole generation.
//!
//! What it adds on the way through: the provider returns base64 and never says which
//! format it produced, so the server sniffs the first bytes and reports a MIME type. That
//! is what lets the app save `ship.png` rather than `ship.bin`, and lets the Files app and
//! the Gallery treat the result as the picture it is.

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use shared::{
    fs::ApiError,
    image::{
        API_IMAGE_GENERATE, API_IMAGE_INFO, DEFAULT_SIZES, GeneratedImage, ImageInfo, ImageRequest,
        ImageResponse, MAX_COUNT, MAX_INPUT_IMAGES, accepts_input_mime,
    },
};
use tokio::sync::Semaphore;

use crate::{AppState, fs::same_origin, provider};

/// Characters in a prompt. Generous — FLUX and SDXL reward detail — and still a bound.
pub const MAX_PROMPT_CHARS: usize = 4_000;
/// Characters in an `avoid` list, or in one of the short option strings.
const MAX_OPTION_CHARS: usize = 512;
/// Largest reference image, decoded. Well above a `1024x1024` PNG; it exists for the
/// photograph someone actually wants to restyle.
const MAX_INPUT_BYTES: usize = 6 * 1024 * 1024;
/// Largest total across all reference images, decoded.
const MAX_INPUT_TOTAL: usize = 12 * 1024 * 1024;
/// Largest request body this router accepts.
///
/// Axum's default is 2 MiB, which one reference image blows through, so the images router
/// raises it explicitly. Four images at [`MAX_INPUT_BYTES`] are about 16 MiB once base64
/// has added its third, which is what this leaves room for.
const MAX_REQUEST_BYTES: usize = 20 * 1024 * 1024;
/// Most diffusion steps a request may ask for. Past a point more steps is only slower.
const MAX_STEPS: u32 = 150;
/// Largest guidance scale a request may ask for.
const MAX_GUIDANCE: f32 = 30.0;
/// Largest provider body read. One `1024x1024` PNG is a few hundred KB of base64, and
/// the provider takes up to ten of them; this leaves room without being unbounded.
const MAX_PROVIDER_BYTES: usize = 64 * 1024 * 1024;
/// Budget for one generation. Diffusion is slower than a chat completion, and a batch
/// of ten is slower still.
pub const TIMEOUT_SECS: u64 = 300;
/// Budget for establishing the connection.
const CONNECT_TIMEOUT_SECS: u64 = 10;
/// Generations in flight at once. Lower than the chat gateway's: each one holds a large
/// response in memory and costs real money.
const MAX_IN_FLIGHT: usize = 2;

/// The gateway's configuration, derived from the environment once at startup.
#[derive(Debug)]
pub struct Images {
    client: reqwest::Client,
    /// Provider base URL with no trailing slash, e.g. `https://api.melious.ai/v1`.
    base_url: String,
    /// The provider key. Empty is allowed — a local provider may not want one — but the
    /// operator is warned at startup, because hosted providers will 401.
    key: String,
    default_model: String,
    /// Every model a request may name. Empty means the gateway is off.
    models: Vec<String>,
    /// Every `WxH` a request may name, in the order the app should offer them.
    sizes: Vec<String>,
    in_flight: Arc<Semaphore>,
}

impl Images {
    /// `WEB_DESKTOP_IMAGE_URL` plus, optionally, `WEB_DESKTOP_IMAGE_KEY`,
    /// `WEB_DESKTOP_IMAGE_MODEL`, `WEB_DESKTOP_IMAGE_MODELS` and
    /// `WEB_DESKTOP_IMAGE_SIZES`.
    ///
    /// `None` when no URL is set, which is what "off" means here.
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("WEB_DESKTOP_IMAGE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty())?;

        let key = std::env::var("WEB_DESKTOP_IMAGE_KEY")
            .unwrap_or_default()
            .trim()
            .to_owned();

        let mut default_model = std::env::var("WEB_DESKTOP_IMAGE_MODEL")
            .unwrap_or_default()
            .trim()
            .to_owned();

        let mut models = comma_list("WEB_DESKTOP_IMAGE_MODELS");
        let sizes = match comma_list("WEB_DESKTOP_IMAGE_SIZES") {
            list if list.is_empty() => default_sizes(),
            list => list,
        };

        // Same rule as the chat gateway: a default the allow-list does not contain would
        // make every default request fail, so the allow-list wins and the default is
        // moved into it.
        if models.is_empty() && !default_model.is_empty() {
            models.push(default_model.clone());
        } else if !models.is_empty() && !models.iter().any(|model| model == &default_model) {
            if !default_model.is_empty() {
                tracing::warn!(
                    "images: WEB_DESKTOP_IMAGE_MODEL={default_model} is not in \
                     WEB_DESKTOP_IMAGE_MODELS; using {} instead",
                    models[0]
                );
            }
            default_model = models[0].clone();
        }

        Some(Self::with_config(
            base_url,
            key,
            default_model,
            models,
            sizes,
        ))
    }

    fn with_config(
        base_url: String,
        key: String,
        default_model: String,
        models: Vec<String>,
        sizes: Vec<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
            // Also bounds the wait for the provider's first byte, which for a diffusion
            // model is most of the request.
            .read_timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            base_url,
            key,
            default_model,
            models,
            sizes,
            in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        }
    }

    /// What the catalog and Settings get to see. No key, and no base URL either.
    pub fn info(&self) -> ImageInfo {
        ImageInfo {
            default_model: self.default_model.clone(),
            models: self.models.clone(),
            sizes: self.sizes.clone(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn has_key(&self) -> bool {
        !self.key.is_empty()
    }

    /// The model a request will actually use, or why it cannot.
    fn resolve_model(&self, requested: Option<&str>) -> Result<String, ImageError> {
        let model = match requested.map(str::trim) {
            Some(model) if !model.is_empty() => model.to_owned(),
            _ => self.default_model.clone(),
        };

        if model.is_empty() {
            return Err(ImageError::Denied(
                "no image model is configured on this server; set WEB_DESKTOP_IMAGE_MODEL"
                    .to_owned(),
            ));
        }
        if !self.models.iter().any(|allowed| allowed == &model) {
            return Err(ImageError::BadRequest(format!(
                "`{model}` is not one of the image models this server offers ({}); widen \
                 WEB_DESKTOP_IMAGE_MODELS to allow it",
                self.models.join(", ")
            )));
        }
        Ok(model)
    }

    /// The size a request will actually use, or why it cannot.
    fn resolve_size(&self, requested: Option<&str>) -> Result<String, ImageError> {
        let size = match requested.map(str::trim) {
            Some(size) if !size.is_empty() => size.to_owned(),
            _ => self.sizes.first().cloned().unwrap_or_default(),
        };

        if size.is_empty() {
            return Err(ImageError::Denied(
                "no image size is configured on this server; set WEB_DESKTOP_IMAGE_SIZES"
                    .to_owned(),
            ));
        }
        if !self.sizes.iter().any(|allowed| allowed == &size) {
            return Err(ImageError::BadRequest(format!(
                "`{size}` is not one of the sizes this server offers ({}); widen \
                 WEB_DESKTOP_IMAGE_SIZES to allow it",
                self.sizes.join(", ")
            )));
        }
        Ok(size)
    }
}

/// The default size allow-list, exactly what the provider documents.
fn default_sizes() -> Vec<String> {
    DEFAULT_SIZES
        .iter()
        .map(|size| (*size).to_owned())
        .collect()
}

/// A comma-separated environment variable, trimmed, dropped when empty, deduplicated.
fn comma_list(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in std::env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !out.iter().any(|seen| seen == value) {
            out.push(value.to_owned());
        }
    }
    out
}

/// `GET` for [`ImageInfo`], `POST` to generate.
///
/// Answers even when the gateway is off, so an app can explain itself instead of
/// treating a 404 as a bug.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(API_IMAGE_INFO, get(info))
        .route(API_IMAGE_GENERATE, post(generate))
        // A body of reference images is megabytes of base64, so the images router raises
        // axum's 2 MiB default explicitly rather than failing on the first photograph.
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        // The same guard the file API, the proxy and the chat gateway carry: this
        // endpoint spends the operator's key and money, so a random page must not be
        // able to cause a request.
        .route_layer(middleware::from_fn(same_origin))
}

async fn info(State(state): State<Arc<AppState>>) -> Json<ImageInfo> {
    Json(state.images.as_ref().map(Images::info).unwrap_or_default())
}

async fn generate(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ImageRequest>,
) -> Result<Json<ImageResponse>, ImageError> {
    let images = state.images.as_ref().ok_or_else(|| {
        ImageError::Denied(
            "the image gateway is off; start the server with WEB_DESKTOP_IMAGE_URL set to an \
             OpenAI-compatible endpoint"
                .to_owned(),
        )
    })?;

    let prompt = validate(&request)?;
    let model = images.resolve_model(request.model.as_deref())?;
    let size = images.resolve_size(request.size.as_deref())?;

    // Built field by field rather than by forwarding the caller's JSON: what is sent is
    // exactly what this version means to send, so a field added to the request type
    // cannot reach the provider by accident. Model-specific extras go at the top level,
    // which is where the provider routes them from.
    let mut body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "n": request.n,
        "size": size,
        // The provider hosts no result URLs, for privacy; this is the only format it
        // accepts, so sending it is documentation rather than a choice.
        "response_format": "b64_json",
    });
    let object = body.as_object_mut().expect("a JSON object");
    for (key, value) in [
        ("quality", request.quality.as_deref()),
        ("style", request.style.as_deref()),
        ("negative_prompt", request.negative_prompt.as_deref()),
        ("output_format", request.output_format.as_deref()),
    ] {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            object.insert(key.to_owned(), serde_json::Value::from(value));
        }
    }
    if let Some(seed) = request.seed {
        object.insert("seed".to_owned(), serde_json::Value::from(seed));
    }
    // The reference images that make this img2img rather than text-to-image. Sent as the
    // bare base64 the provider documents, not as `data:` URLs.
    if !request.input_images.is_empty() {
        object.insert(
            "input_images".to_owned(),
            serde_json::Value::from(request.input_images.clone()),
        );
    }
    if let Some(steps) = request.steps {
        object.insert("steps".to_owned(), serde_json::Value::from(steps));
    }
    if let Some(guidance) = request.guidance {
        object.insert("guidance".to_owned(), serde_json::Value::from(guidance));
    }

    // A slot is taken before the request goes out and held until the answer is read, so
    // a burst of Generate clicks cannot put more than a couple of generations on the
    // operator's bill at once.
    let _permit = Arc::clone(&images.in_flight)
        .acquire_owned()
        .await
        .map_err(|_| ImageError::Internal("the gateway is shutting down".to_owned()))?;

    let mut outgoing = images
        .client
        .post(format!("{}/images/generations", images.base_url))
        .json(&body)
        .timeout(Duration::from_secs(TIMEOUT_SECS));
    if images.has_key() {
        outgoing = outgoing.bearer_auth(&images.key);
    }

    tracing::debug!(
        "images: {} {} -> {}/images/generations",
        model,
        size,
        images.base_url
    );

    let response = outgoing.send().await.map_err(|err| {
        if err.is_timeout() {
            ImageError::Timeout(format!(
                "the provider did not answer within {TIMEOUT_SECS}s"
            ))
        } else {
            ImageError::Upstream(format!("could not reach the provider: {err}"))
        }
    })?;

    if !response.status().is_success() {
        return Err(provider_error(response).await);
    }

    // Read with a ceiling rather than trusting `content-length`, which a provider may
    // omit or lie about — the same reason the chat gateway and the proxy read by hand.
    let mut raw = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk =
            chunk.map_err(|err| ImageError::Upstream(format!("reading the answer: {err}")))?;
        if raw.len() + chunk.len() > MAX_PROVIDER_BYTES {
            return Err(ImageError::Upstream(format!(
                "the provider's answer is larger than {} MiB",
                MAX_PROVIDER_BYTES / (1024 * 1024)
            )));
        }
        raw.extend_from_slice(&chunk);
    }

    let answer: ProviderImages = serde_json::from_slice(&raw).map_err(|err| {
        ImageError::Upstream(format!(
            "the provider sent something that is not an OpenAI image response: {err}"
        ))
    })?;

    let mut out = Vec::new();
    for image in answer.data {
        let Some(b64_json) = image.b64_json.filter(|data| !data.is_empty()) else {
            continue;
        };
        out.push(GeneratedImage {
            mime: sniff_mime(&b64_json).to_owned(),
            b64_json,
            revised_prompt: image.revised_prompt,
        });
    }
    if out.is_empty() {
        return Err(ImageError::Upstream(
            "the provider answered with no images".to_owned(),
        ));
    }

    Ok(Json(ImageResponse {
        // The provider's own word when it echoes one, otherwise what was asked for. The
        // documented response carries no model field at all.
        model: answer.model.unwrap_or(model),
        images: out,
        credits_cost: answer.credits_cost,
    }))
}

/// What a request has to look like before anything is sent.
///
/// Returns the trimmed prompt, which is what goes out.
fn validate(request: &ImageRequest) -> Result<&str, ImageError> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err(ImageError::BadRequest(
            "the request has no prompt".to_owned(),
        ));
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(ImageError::BadRequest(format!(
            "the prompt is {} characters; the limit is {MAX_PROMPT_CHARS}",
            prompt.chars().count()
        )));
    }
    if request.n == 0 || request.n > MAX_COUNT {
        return Err(ImageError::BadRequest(format!(
            "asked for {} images; the limit is {MAX_COUNT}",
            request.n
        )));
    }
    for (name, value) in [
        ("quality", request.quality.as_deref()),
        ("style", request.style.as_deref()),
        ("negative_prompt", request.negative_prompt.as_deref()),
        ("output_format", request.output_format.as_deref()),
    ] {
        if value.is_some_and(|value| value.chars().count() > MAX_OPTION_CHARS) {
            return Err(ImageError::BadRequest(format!(
                "`{name}` is longer than {MAX_OPTION_CHARS} characters"
            )));
        }
    }
    if request
        .steps
        .is_some_and(|steps| steps == 0 || steps > MAX_STEPS)
    {
        return Err(ImageError::BadRequest(format!(
            "`steps` must be between 1 and {MAX_STEPS}"
        )));
    }
    if request
        .guidance
        .is_some_and(|guidance| !guidance.is_finite() || !(0.0..=MAX_GUIDANCE).contains(&guidance))
    {
        return Err(ImageError::BadRequest(format!(
            "`guidance` must be between 0 and {MAX_GUIDANCE}"
        )));
    }
    validate_references(&request.input_images)?;
    Ok(prompt)
}

/// Check the img2img reference images before any of them is forwarded.
///
/// The one thing a caller controls here is file *content* — megabytes of base64 that the
/// server hands to the provider — so this is the boundary that bounds it: how many, how
/// big each, how big in total, and that each one really is a raster image rather than
/// arbitrary bytes wearing a reference's clothes.
fn validate_references(images: &[String]) -> Result<(), ImageError> {
    if images.len() > MAX_INPUT_IMAGES {
        return Err(ImageError::BadRequest(format!(
            "{} reference images; the limit is {MAX_INPUT_IMAGES}",
            images.len()
        )));
    }

    let mut total = 0usize;
    for (index, data) in images.iter().enumerate() {
        let number = index + 1;
        let Some(decoded) = base64_len(data) else {
            return Err(ImageError::BadRequest(format!(
                "reference image {number} is not base64"
            )));
        };
        if decoded > MAX_INPUT_BYTES {
            return Err(ImageError::BadRequest(format!(
                "reference image {number} is {} MiB decoded; the limit is {} MiB",
                decoded / (1024 * 1024),
                MAX_INPUT_BYTES / (1024 * 1024)
            )));
        }
        total += decoded;
        if total > MAX_INPUT_TOTAL {
            return Err(ImageError::BadRequest(format!(
                "the reference images total more than {} MiB decoded",
                MAX_INPUT_TOTAL / (1024 * 1024)
            )));
        }
        let mime = sniff_mime(data);
        if !accepts_input_mime(mime) {
            return Err(ImageError::BadRequest(format!(
                "reference image {number} is not a PNG, JPEG or WebP"
            )));
        }
    }
    Ok(())
}

/// The decoded length of a base64 body, or `None` when it is not base64 at all.
///
/// A validator rather than a decoder: the server never needs the reference image's bytes,
/// only its size and its first few, and the first few are readable straight off the base64
/// prefix (see [`sniff_mime`]). What it has to be sure of is that the string is something
/// the provider will accept, so it checks the alphabet and the length and stops there.
fn base64_len(data: &str) -> Option<usize> {
    // The smallest base64 group is four characters, and every group is four.
    if data.len() < 4 || !data.len().is_multiple_of(4) {
        return None;
    }

    let bytes = data.as_bytes();
    let mut padding = 0usize;
    for byte in bytes {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => {
                // Anything after the padding means the `=` was not at the end.
                if padding > 0 {
                    return None;
                }
            }
            b'=' => padding += 1,
            _ => return None,
        }
        if padding > 2 {
            return None;
        }
    }

    Some(data.len() / 4 * 3 - padding)
}

/// Which image format a base64 body holds, from its leading bytes.
///
/// The provider decodes base64 and looks for the file signatures (PNG, JPEG, GIF, WebP).
/// Base64 encodes three bytes at a time and independently, so the encoding of the first
/// bytes is a prefix of the whole encoding — which means the signature can be read from
/// the string without decoding a megabyte to look at eight bytes.
///
/// Anything unrecognised is `application/octet-stream`: the app then saves it rather
/// than pointing an `<img>` at it, which is the honest thing to do with bytes whose type
/// nobody knows.
fn sniff_mime(b64: &str) -> &'static str {
    // PNG \x89PNG\r\n\x1a\n, JPEG \xff\xd8\xff, GIF87a/GIF89a, RIFF….WEBP
    const SIGNATURES: [(&str, &str); 4] = [
        ("iVBORw0KGgo", "image/png"),
        ("/9j/", "image/jpeg"),
        ("R0lGOD", "image/gif"),
        ("UklGR", "image/webp"),
    ];
    SIGNATURES
        .iter()
        .find(|(signature, _)| b64.starts_with(signature))
        .map(|(_, mime)| *mime)
        .unwrap_or("application/octet-stream")
}

/// The parts of an OpenAI image response this server reads. Everything else the
/// provider sends is ignored rather than modelled.
#[derive(Debug, serde::Deserialize)]
struct ProviderImages {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    data: Vec<ProviderImage>,
    #[serde(default)]
    credits_cost: Option<f64>,
}

#[derive(Debug, serde::Deserialize)]
struct ProviderImage {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    revised_prompt: Option<String>,
}

/// Turn a provider failure into something the app can show a person.
///
/// The provider's own words are kept — a `content_policy_violation` is worth more to
/// whoever wrote the prompt than "bad request" is — and the status is remapped: a 401 or
/// a 403 from the provider is *this server's* configuration problem, not the caller's.
async fn provider_error(response: reqwest::Response) -> ImageError {
    let status = response.status();
    let detail = provider::error_body(response).await;
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };

    match status.as_u16() {
        429 => {
            ImageError::RateLimited(format!("the provider is rate limiting this server{detail}"))
        }
        400 | 422 => ImageError::BadRequest(format!("the provider rejected the request{detail}")),
        401 | 403 => ImageError::Upstream(format!(
            "the provider rejected this server's API key or the key lacks the image scope; \
             check WEB_DESKTOP_IMAGE_KEY{detail}"
        )),
        404 => ImageError::Upstream(format!(
            "the provider knows no such endpoint or image model{detail}"
        )),
        _ => ImageError::Upstream(format!("the provider answered {status}{detail}")),
    }
}

#[derive(Debug)]
pub enum ImageError {
    BadRequest(String),
    /// Refused before anything went out.
    Denied(String),
    RateLimited(String),
    Timeout(String),
    Upstream(String),
    Internal(String),
}

impl ImageError {
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

impl IntoResponse for ImageError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = self.message().to_owned();
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            tracing::warn!("images: {status}: {message}");
        } else {
            tracing::debug!("images: {status}: {message}");
        }
        (status, Json(ApiError { error: message })).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header;
    use axum::routing::post as axum_post;

    /// A one-pixel PNG, base64, as a provider would return it.
    const TINY_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn gateway(base_url: &str, default_model: &str, models: &[&str]) -> Images {
        Images::with_config(
            base_url.trim_end_matches('/').to_owned(),
            "test-key".to_owned(),
            default_model.to_owned(),
            models.iter().map(|model| (*model).to_owned()).collect(),
            default_sizes(),
        )
    }

    fn request(model: Option<&str>) -> ImageRequest {
        let mut request = ImageRequest::new("a Hanseatic trading ship at dawn");
        request.model = model.map(str::to_owned);
        request
    }

    /// A stand-in for an image provider: it answers the success shape, a policy refusal,
    /// or an auth failure, so the gateway can be exercised without a key or a network.
    async fn fake_provider(mode: &'static str) -> String {
        let app = Router::new().route(
            "/v1/images/generations",
            axum_post(move |Json(body): Json<serde_json::Value>| async move {
                match mode {
                    "ok" => {
                        assert_eq!(body["response_format"], "b64_json");
                        (
                            [(header::CONTENT_TYPE, "application/json")],
                            serde_json::json!({
                                "created": 1_699_999_999,
                                "data": [{"b64_json": TINY_PNG, "revised_prompt": "a ship"}],
                                "credits_cost": 0.04,
                            })
                            .to_string(),
                        )
                            .into_response()
                    }
                    "no_images" => (
                        [(header::CONTENT_TYPE, "application/json")],
                        r#"{"created":1,"data":[]}"#,
                    )
                        .into_response(),
                    "img2img" => {
                        // The whole point of the mode: what the app sent as a reference
                        // and as model extras arrived at the provider unchanged.
                        assert_eq!(body["input_images"][0], TINY_PNG);
                        assert_eq!(body["steps"], 20);
                        assert_eq!(body["guidance"], 3.5);
                        (
                            [(header::CONTENT_TYPE, "application/json")],
                            serde_json::json!({
                                "created": 1,
                                "data": [{"b64_json": TINY_PNG}],
                            })
                            .to_string(),
                        )
                            .into_response()
                    }
                    "policy" => (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        r#"{"error":{"message":"content_policy_violation","type":"invalid_request_error"}}"#,
                    )
                        .into_response(),
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

    fn state_with(images: Images) -> Arc<AppState> {
        Arc::new(AppState {
            started: std::time::Instant::now(),
            catalog: shared::default_catalog(),
            apps_dir: None,
            proxy: None,
            ai: None,
            images: Some(images),
            fs: None,
        })
    }

    #[tokio::test]
    async fn a_generated_image_comes_back_with_a_sniffed_type() {
        let base = fake_provider("ok").await;
        let state = state_with(gateway(&base, "sd", &["sd"]));

        let Json(answer) = generate(State(state), Json(request(None))).await.unwrap();
        assert_eq!(answer.model, "sd");
        assert_eq!(answer.credits_cost, Some(0.04));
        assert_eq!(answer.images.len(), 1);
        assert_eq!(answer.images[0].mime, "image/png");
        assert_eq!(answer.images[0].extension(), "png");
        assert_eq!(answer.images[0].b64_json, TINY_PNG);
        assert_eq!(answer.images[0].revised_prompt.as_deref(), Some("a ship"));
    }

    #[tokio::test]
    async fn a_provider_that_sends_no_images_is_an_error_not_an_empty_batch() {
        let base = fake_provider("no_images").await;
        let state = state_with(gateway(&base, "sd", &["sd"]));

        let err = generate(State(state), Json(request(None)))
            .await
            .unwrap_err();
        assert!(matches!(err, ImageError::Upstream(_)), "{err:?}");
        assert!(err.message().contains("no images"), "{}", err.message());
    }

    #[tokio::test]
    async fn a_policy_refusal_keeps_the_providers_own_words() {
        let base = fake_provider("policy").await;
        let state = state_with(gateway(&base, "sd", &["sd"]));

        let err = generate(State(state), Json(request(None)))
            .await
            .unwrap_err();
        assert!(matches!(err, ImageError::BadRequest(_)), "{err:?}");
        assert!(
            err.message().contains("content_policy_violation"),
            "{}",
            err.message()
        );
    }

    #[tokio::test]
    async fn a_provider_auth_failure_reads_as_the_servers_problem_not_the_callers() {
        let base = fake_provider("unauthorized").await;
        let state = state_with(gateway(&base, "sd", &["sd"]));

        let err = generate(State(state), Json(request(None)))
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
        assert!(
            err.message().contains("WEB_DESKTOP_IMAGE_KEY"),
            "{}",
            err.message()
        );
        assert!(
            err.message().contains("Incorrect API key"),
            "{}",
            err.message()
        );
    }

    #[tokio::test]
    async fn a_bad_model_or_size_is_refused_before_anything_is_sent() {
        // Port 1 would refuse the connection, so reaching the provider at all fails the
        // test — which is what proves the refusal happened first.
        let state = state_with(gateway("http://127.0.0.1:1/v1", "sd", &["sd"]));

        let err = generate(State(state.clone()), Json(request(Some("expensive"))))
            .await
            .unwrap_err();
        assert!(matches!(err, ImageError::BadRequest(_)), "{err:?}");

        let mut sized = request(None);
        sized.size = Some("4096x4096".to_owned());
        let err = generate(State(state), Json(sized)).await.unwrap_err();
        assert!(matches!(err, ImageError::BadRequest(_)), "{err:?}");
        assert!(err.message().contains("WEB_DESKTOP_IMAGE_SIZES"));
    }

    #[tokio::test]
    async fn the_gateway_being_off_is_explained_rather_than_404() {
        let state = Arc::new(AppState {
            started: std::time::Instant::now(),
            catalog: shared::default_catalog(),
            apps_dir: None,
            proxy: None,
            ai: None,
            images: None,
            fs: None,
        });

        let err = generate(State(state.clone()), Json(request(None)))
            .await
            .unwrap_err();
        assert!(matches!(err, ImageError::Denied(_)), "{err:?}");
        assert!(
            err.message().contains("WEB_DESKTOP_IMAGE_URL"),
            "{}",
            err.message()
        );

        let Json(info) = info(State(state)).await;
        assert!(!info.enabled());
    }

    #[test]
    fn a_gateway_with_no_model_says_which_variable_to_set() {
        let images = gateway("http://example.test/v1", "", &[]);
        let err = images.resolve_model(None).unwrap_err();
        assert!(matches!(err, ImageError::Denied(_)), "{err:?}");
        assert!(
            err.message().contains("WEB_DESKTOP_IMAGE_MODEL"),
            "{}",
            err.message()
        );
        assert!(!images.info().enabled());
    }

    #[test]
    fn the_default_size_is_the_first_one_offered() {
        let images = gateway("http://example.test/v1", "sd", &["sd"]);
        assert_eq!(images.resolve_size(None).unwrap(), "1024x1024");
        assert_eq!(images.resolve_size(Some(" 256x256 ")).unwrap(), "256x256");
    }

    #[test]
    fn the_signature_is_read_from_the_base64_without_decoding_it() {
        assert_eq!(sniff_mime(TINY_PNG), "image/png");
        assert_eq!(sniff_mime("/9j/4AAQSkZJRg=="), "image/jpeg");
        assert_eq!(sniff_mime("R0lGODlhAQABAAAAACw="), "image/gif");
        assert_eq!(sniff_mime("UklGRiIAAABXRUJQVlA4IBYAAAAw"), "image/webp");
        assert_eq!(sniff_mime("bm90IGFuIGltYWdl"), "application/octet-stream");
        assert_eq!(sniff_mime(""), "application/octet-stream");
    }

    #[test]
    fn an_empty_prompt_and_a_silly_count_are_both_refused() {
        let mut empty = ImageRequest::new("   ");
        assert!(matches!(validate(&empty), Err(ImageError::BadRequest(_))));

        empty.prompt = "a ship".to_owned();
        empty.n = 0;
        assert!(matches!(validate(&empty), Err(ImageError::BadRequest(_))));

        empty.n = MAX_COUNT + 1;
        assert!(matches!(validate(&empty), Err(ImageError::BadRequest(_))));

        empty.n = MAX_COUNT;
        assert!(validate(&empty).is_ok());
    }

    #[test]
    fn the_prompt_comes_back_trimmed() {
        let request = ImageRequest::new("  a ship  ");
        assert_eq!(validate(&request).unwrap(), "a ship");
    }

    #[tokio::test]
    async fn reference_images_and_the_model_extras_reach_the_provider() {
        let base = fake_provider("img2img").await;
        let state = state_with(gateway(&base, "sd", &["sd"]));

        let mut request = ImageRequest::new("make it dusk");
        request.input_images = vec![TINY_PNG.to_owned()];
        request.steps = Some(20);
        request.guidance = Some(3.5);

        let Json(answer) = generate(State(state), Json(request)).await.unwrap();
        assert_eq!(answer.images.len(), 1);
    }

    #[test]
    fn a_reference_that_is_not_an_image_is_refused_before_anything_is_sent() {
        let mut request = ImageRequest::new("a ship");

        // Not base64 at all.
        request.input_images = vec!["not base64!".to_owned()];
        assert!(matches!(validate(&request), Err(ImageError::BadRequest(_))));

        // Valid base64, but the bytes are `hi` rather than a picture.
        request.input_images = vec!["aGk=".to_owned()];
        let err = validate(&request).unwrap_err();
        assert!(err.message().contains("not a PNG"), "{}", err.message());

        // A GIF is an image, and still not one of the formats the models take.
        request.input_images = vec!["R0lGODlhAQABAAAAACw=".to_owned()];
        assert!(matches!(validate(&request), Err(ImageError::BadRequest(_))));
    }

    #[test]
    fn too_many_reference_images_or_a_silly_extras_value_is_refused() {
        let mut request = ImageRequest::new("a ship");
        request.input_images = vec![TINY_PNG.to_owned(); MAX_INPUT_IMAGES + 1];
        let err = validate(&request).unwrap_err();
        assert!(
            err.message().contains("reference images"),
            "{}",
            err.message()
        );

        request.input_images = vec![TINY_PNG.to_owned()];
        request.steps = Some(0);
        assert!(matches!(validate(&request), Err(ImageError::BadRequest(_))));
        request.steps = Some(MAX_STEPS + 1);
        assert!(matches!(validate(&request), Err(ImageError::BadRequest(_))));
        request.steps = Some(20);

        request.guidance = Some(-1.0);
        assert!(matches!(validate(&request), Err(ImageError::BadRequest(_))));
        request.guidance = Some(f32::NAN);
        assert!(matches!(validate(&request), Err(ImageError::BadRequest(_))));
        request.guidance = Some(3.5);
        assert!(validate(&request).is_ok());
    }

    #[test]
    fn a_reference_larger_than_the_cap_is_refused() {
        // `iVBORw0KGgoA` is the PNG signature in base64 and is a whole number of groups,
        // so appending `A`s keeps it valid base64 and keeps the signature at the front.
        // Base64 is four characters per three bytes, so this is what it takes to decode
        // to more than the cap rather than exactly to it.
        let chars = MAX_INPUT_BYTES / 3 * 4 + 4;
        assert!(
            chars.is_multiple_of(4),
            "the constructed body has to be valid base64"
        );
        let oversize = format!("iVBORw0KGgoA{}", "A".repeat(chars));
        let request = ImageRequest::new("a ship").with_input_images(vec![oversize]);
        let err = validate(&request).unwrap_err();
        assert!(err.message().contains("MiB decoded"), "{}", err.message());
    }

    #[test]
    fn the_base64_shapes_that_are_not_base64_are_caught() {
        assert_eq!(base64_len("AAAA"), Some(3));
        // One byte of payload, two padding characters.
        assert_eq!(base64_len("aGk="), Some(2));
        assert_eq!(base64_len("aGk"), None, "length is not a multiple of four");
        assert_eq!(base64_len("AAA"), None);
        assert_eq!(base64_len(""), None);
        assert_eq!(base64_len("===="), None, "nothing but padding");
        assert_eq!(base64_len("AA=="), Some(1));
        assert_eq!(base64_len("A==="), None, "three padding characters");
        assert_eq!(base64_len("AA=A"), None, "data after the padding");
        assert_eq!(base64_len("AA-A"), None, "a character outside the alphabet");
    }
}
