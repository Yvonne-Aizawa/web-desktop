//! Generating images with a model the server talks to.
//!
//! The provider's key is the *server's*, exactly as it is for [`crate::ai`]: an app
//! posts a prompt and gets image bytes back, and never holds a credential or names a
//! host. The two gateways are configured separately (`WEB_DESKTOP_IMAGE_*` beside
//! `WEB_DESKTOP_AI_*`), so a chat model and an image model can live at different
//! providers, on different keys, with different allow-lists:
//!
//! ```text
//! app ──POST /api/images/generate──▶ server ──POST {WEB_DESKTOP_IMAGE_URL}/images/generations──▶ provider
//! ```
//!
//! The provider answers with base64 and no URL (it does not host the result), so the
//! response carries [`GeneratedImage::b64_json`] and the app decodes it. The one thing
//! the server adds is [`GeneratedImage::mime`], sniffed from the bytes: the provider
//! does not say what format it returned, and a file needs an extension it can be
//! browsed under.

use serde::{Deserialize, Serialize};

/// `POST` an [`ImageRequest`]: an [`ImageResponse`] with one or more images.
pub const API_IMAGE_GENERATE: &str = "/api/images/generate";
/// `GET` -> [`ImageInfo`]. Answers even when the gateway is off, so an app can say so.
pub const API_IMAGE_INFO: &str = "/api/images/info";

/// The sizes the provider documents, and the server's default allow-list. The order is
/// also the order the app offers them in, and the first is the one a request that names
/// no size gets — so it starts with the provider's own default rather than its smallest.
pub const DEFAULT_SIZES: [&str; 5] = ["1024x1024", "1792x1024", "1024x1792", "512x512", "256x256"];

/// Most images one request may ask for. The provider's own ceiling is `10`; the server
/// caps at the same number so an app cannot ask for more than the provider will take.
pub const MAX_COUNT: u32 = 10;

/// What the server's image gateway is configured to do.
///
/// Carried in [`crate::Catalog`] as well, so Settings can report it the way it reports
/// the chat gateway. Empty `models` means the gateway is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImageInfo {
    /// Used when a request does not name a model.
    pub default_model: String,
    /// Every model a request may ask for.
    pub models: Vec<String>,
    /// Every `WxH` a request may ask for. The server keeps this to what the operator
    /// allows, because a bigger image is a bigger bill.
    #[serde(default)]
    pub sizes: Vec<String>,
}

impl ImageInfo {
    pub fn enabled(&self) -> bool {
        !self.models.is_empty()
    }
}

/// Request body for [`API_IMAGE_GENERATE`].
///
/// Only the fields this desktop means to send are modelled: the server builds the
/// provider's body field by field rather than forwarding a caller's JSON, so a field
/// added here is a deliberate decision. The model-specific controls the provider
/// documents (`steps`, `guidance`, `loras`, and `input_images` for the img2img
/// variants) are the natural extension — each is one more explicit option, not a
/// passthrough map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRequest {
    /// What to draw. Prompt conventions vary by model; the server only bounds the length.
    pub prompt: String,
    /// A model from [`ImageInfo::models`]. `None` means the server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How many images, `1..=MAX_COUNT`.
    #[serde(default = "one")]
    pub n: u32,
    /// A `WxH` from [`ImageInfo::sizes`]. `None` means the server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// `standard` or `hd` for the models that offer both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// `vivid` or `natural` for the models that offer both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// Deterministic generation when the model supports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// Concepts to avoid, for the FLUX and SDXL variants that accept it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
    /// `png`, `jpeg` or `webp`, for the models that can re-encode. Unset means the
    /// provider's own default, which is why the response reports what it actually got.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
}

fn one() -> u32 {
    1
}

impl ImageRequest {
    /// One image from the server's default model and size.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model: None,
            n: 1,
            size: None,
            quality: None,
            style: None,
            seed: None,
            negative_prompt: None,
            output_format: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_count(mut self, n: u32) -> Self {
        self.n = n;
        self
    }

    pub fn with_size(mut self, size: impl Into<String>) -> Self {
        self.size = Some(size.into());
        self
    }
}

/// A finished batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageResponse {
    /// The model that actually answered, which is the provider's word, not the
    /// request's.
    pub model: String,
    pub images: Vec<GeneratedImage>,
    /// What the provider says the batch cost, in its own currency. Optional because
    /// not every provider reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits_cost: Option<f64>,
}

/// One generated image, still base64.
///
/// The bytes are not decoded for transport: a 1024×1024 PNG is a few hundred KB, and
/// the app has to hold them to write them anyway. [`Self::mime`] is the server's read
/// of the first bytes, so the app can name the file correctly and only preview what a
/// browser can actually decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedImage {
    /// The provider's base64, unchanged.
    pub b64_json: String,
    /// Sniffed from the decoded prefix: `image/png`, `image/jpeg`, `image/gif`,
    /// `image/webp`, or `application/octet-stream` when the magic is not recognised.
    pub mime: String,
    /// Some models rewrite the prompt before generating; the revised version is here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
}

impl GeneratedImage {
    /// The file extension this image should be saved under, from its sniffed type.
    pub fn extension(&self) -> &'static str {
        match self.mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/bmp" => "bmp",
            _ => "bin",
        }
    }

    /// Whether a browser can be pointed at this as an `<img>`.
    pub fn is_previewable(&self) -> bool {
        self.mime.starts_with("image/") && self.mime != "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::{GeneratedImage, ImageInfo, ImageRequest};

    #[test]
    fn a_gateway_with_no_model_is_off() {
        assert!(!ImageInfo::default().enabled());
        assert!(
            ImageInfo {
                default_model: "m".to_owned(),
                models: vec!["m".to_owned()],
                sizes: Vec::new(),
            }
            .enabled()
        );
    }

    #[test]
    fn an_unset_model_and_size_are_omitted_rather_than_sent_as_null() {
        let request = ImageRequest::new("a ship");
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"prompt":"a ship","n":1}"#
        );
    }

    #[test]
    fn a_request_without_a_count_still_means_one() {
        let request: ImageRequest = serde_json::from_str(r#"{"prompt":"a ship"}"#).unwrap();
        assert_eq!(request.n, 1);
    }

    #[test]
    fn the_extension_follows_the_sniffed_type() {
        let image = |mime: &str| GeneratedImage {
            b64_json: String::new(),
            mime: mime.to_owned(),
            revised_prompt: None,
        };
        assert_eq!(image("image/png").extension(), "png");
        assert_eq!(image("image/jpeg").extension(), "jpg");
        assert_eq!(image("image/webp").extension(), "webp");
        assert_eq!(image("application/octet-stream").extension(), "bin");
        assert!(image("image/png").is_previewable());
        // An unknown type is saved, but not pointed at an `<img>`.
        assert!(!image("application/octet-stream").is_previewable());
    }
}
