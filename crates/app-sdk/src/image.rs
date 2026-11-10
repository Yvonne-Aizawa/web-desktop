//! Generating an image with a model the shell's server talks to.
//!
//! Same shape and same reason as [`crate::ai`]: the provider's key belongs to the
//! *server*, so the app never holds one and never names a host. It sends a prompt and
//! gets bytes back:
//!
//! ```no_run
//! # async fn example() {
//! use app_sdk::image::ImageRequest;
//!
//! // What is on offer here, so the app can gray out or explain itself.
//! let info = app_sdk::image::info().await.unwrap();
//! if !info.enabled() {
//!     // the gateway is off; `WEB_DESKTOP_IMAGE_URL` is what turns it on
//!     return;
//! }
//!
//! let request = ImageRequest::new("a Hanseatic trading ship entering Lübeck harbor at dawn")
//!     .with_model(info.default_model.clone())
//!     .with_size("1024x1024");
//! let answer = app_sdk::image::generate(&request).await.unwrap();
//!
//! // The provider answers with base64 and no URL, so the app decodes it — and then
//! // usually writes it to the sandbox through [`crate::fs`].
//! let bytes = app_sdk::image::decode_base64(&answer.images[0].b64_json).unwrap();
//! # }
//! ```

use gloo_net::http::Request;
use shared::image::{API_IMAGE_GENERATE, API_IMAGE_INFO};

// The wire types live in `shared` so both ends agree by construction; re-exported here
// so an app can name a request and its result without depending on `shared` itself.
pub use shared::image::{
    DEFAULT_SIZES, GeneratedImage, ImageInfo, ImageRequest, ImageResponse, MAX_COUNT,
};

/// What this server's gateway offers. `info().enabled()` is false when it is off.
pub async fn info() -> Result<ImageInfo, String> {
    let response = Request::get(API_IMAGE_INFO)
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;

    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(error_message(response, status).await);
    }
    response
        .json::<ImageInfo>()
        .await
        .map_err(|err| format!("unexpected response: {err}"))
}

/// Ask for a batch of images and wait for all of them.
///
/// There is no streaming half to this: an image arrives whole or not at all.
pub async fn generate(request: &ImageRequest) -> Result<ImageResponse, String> {
    let response = Request::post(API_IMAGE_GENERATE)
        .json(request)
        .map_err(|err| format!("could not build the request: {err}"))?
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;

    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(error_message(response, status).await);
    }
    response
        .json::<ImageResponse>()
        .await
        .map_err(|err| format!("unexpected response: {err}"))
}

/// The bytes behind a [`GeneratedImage::b64_json`].
///
/// `atob` is the browser's own decoder, so this does not carry a base64 table of its
/// own. Its result is a string whose code points are the bytes, which is why each one is
/// taken as a `u8` rather than re-encoded.
pub fn decode_base64(data: &str) -> Result<Vec<u8>, String> {
    let window = web_sys::window().ok_or_else(|| "no browser window".to_owned())?;
    let binary = window
        .atob(data)
        .map_err(|_| "the provider's image data is not valid base64".to_owned())?;
    Ok(bytes_of(&binary))
}

/// `atob`'s result, whose code points are the bytes. Split out so the mapping can be
/// tested on the host, where there is no `window` to call `atob` on.
fn bytes_of(binary: &str) -> Vec<u8> {
    binary.chars().map(|byte| byte as u8).collect()
}

/// The server's own words when it sends an `ApiError`, a status code otherwise.
///
/// Kept the same shape as [`crate::fetch`] and [`crate::ai`]: what an app shows on
/// failure is a sentence the server wrote, not a number.
async fn error_message(response: gloo_net::http::Response, status: u16) -> String {
    response
        .json::<shared::fs::ApiError>()
        .await
        .map(|body| body.error)
        .unwrap_or_else(|_| format!("HTTP {status}"))
}

#[cfg(test)]
mod tests {
    use super::bytes_of;

    #[test]
    fn a_binary_string_becomes_one_byte_per_code_point() {
        // `atob` hands back a string whose code points are the bytes, high ones
        // included: 0x89 is the first byte of a PNG signature.
        assert_eq!(bytes_of("P\u{89}NG"), vec![b'P', 0x89, b'N', b'G']);
        // NUL is a byte like any other, and a code point above 0xFF cannot occur.
        assert_eq!(bytes_of("a\u{0}b"), vec![b'a', 0x00, b'b']);
        assert_eq!(bytes_of(""), Vec::<u8>::new());
    }
}
