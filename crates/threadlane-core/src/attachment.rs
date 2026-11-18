//! Image data shared by the chat, hub, and agent backends.

use std::{fmt, io::Cursor, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize, de::Error as _};

/// The combined raw image bytes allowed in one message. Base64 fits within
/// the remote transport's 4 MiB request limit with room for the caption.
pub const MAX_IMAGE_BYTES: usize = 2 << 20;

const MAX_IMAGES: usize = 8;
const MAX_DIMENSION: u32 = 8192;

/// An inline chat image. JSON contains base64 data and a display name.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageAttachment {
    name: String,
    #[serde(with = "base64_bytes")]
    data: Arc<[u8]>,
}

impl fmt::Debug for ImageAttachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageAttachment")
            .field("name", &self.name)
            .field("bytes", &self.data.len())
            .finish()
    }
}

impl ImageAttachment {
    /// Constructs an attachment. Call [`validate_images`] before sending.
    #[must_use]
    pub fn new(name: String, data: Vec<u8>) -> Self {
        Self {
            name,
            data: data.into(),
        }
    }

    /// The display name, never used as a filesystem path.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The original encoded image.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// The image bytes as standard base64.
    #[must_use]
    pub fn base64(&self) -> String {
        STANDARD.encode(&self.data)
    }

    /// The canonical MIME type and file extension, determined from the bytes.
    #[must_use]
    pub fn format(&self) -> Option<(&'static str, &'static str)> {
        let format = match image::guess_format(&self.data).ok()? {
            ImageFormat::Png => ("image/png", "png"),
            ImageFormat::Jpeg => ("image/jpeg", "jpg"),
            ImageFormat::WebP => ("image/webp", "webp"),
            ImageFormat::Gif => ("image/gif", "gif"),
            _ => return None,
        };
        Some(format)
    }

    /// Decodes the first frame with bounded dimensions and allocation.
    ///
    /// # Errors
    /// The image is corrupt, unsupported, or exceeds the decode limits.
    pub fn decode(&self) -> image::ImageResult<DynamicImage> {
        let mut reader = ImageReader::new(Cursor::new(self.bytes())).with_guessed_format()?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_DIMENSION);
        limits.max_image_height = Some(MAX_DIMENSION);
        limits.max_alloc = Some(128 << 20);
        reader.limits(limits);
        reader.decode()
    }
}

/// Checks the count, combined size, metadata, and decodability.
///
/// # Errors
/// Returns a message suitable for displaying beside the composer.
pub fn validate_images(images: &[ImageAttachment]) -> Result<(), String> {
    check_image_budget(images)?;
    for image in images {
        if image.name().trim().is_empty()
            || image.name().len() > 1024
            || image.name().chars().any(char::is_control)
        {
            return Err(
                "Use an image name between 1 and 1024 bytes without control characters.".into(),
            );
        }
        if image.format().is_none() {
            return Err(format!(
                "Use PNG, JPEG, WebP, or GIF for {:?}.",
                image.name()
            ));
        }
        if image.decode().is_err() {
            return Err(format!(
                "Could not decode {:?}. Check the file and its dimensions.",
                image.name()
            ));
        }
    }
    Ok(())
}

/// Checks the per-message budget without decoding images.
///
/// # Errors
/// Returns an error if the images exceed the combined count or size limit.
pub fn check_image_budget(images: &[ImageAttachment]) -> Result<(), String> {
    if images.len() > MAX_IMAGES {
        return Err(format!("Attach up to {MAX_IMAGES} images per message."));
    }
    if images
        .iter()
        .map(|image| image.bytes().len())
        .sum::<usize>()
        > MAX_IMAGE_BYTES
    {
        return Err("Images can total up to 2 MiB per message.".into());
    }
    Ok(())
}

/// Reads the optional image field of a stored user event. Older events have
/// only text.
#[must_use]
pub fn images_in(payload: &serde_json::Value) -> Vec<ImageAttachment> {
    payload
        .get("images")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

mod base64_bytes {
    use super::*;

    pub(super) fn serialize<S: serde::Serializer>(
        bytes: &[u8],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<[u8]>, D::Error> {
        let data = String::deserialize(deserializer)?;
        if data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err(D::Error::custom("image data exceeds 2 MiB"));
        }
        STANDARD
            .decode(data)
            .map(Arc::from)
            .map_err(D::Error::custom)
    }
}
