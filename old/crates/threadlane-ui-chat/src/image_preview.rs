use std::io::Cursor;
use std::sync::Arc;

use base64::Engine as _;
use gpui::RenderImage;
use image::{Frame, ImageFormat, ImageReader, Limits};
use threadlane_protocol::ImageAttachment;

const MAX_DATA_URL_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 8192;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_DECODE_ALLOCATION: u64 = 64 * 1024 * 1024;

/// Decode only the inline PNG/JPEG formats accepted by the staged-image preview.
/// The upload attachment itself is never modified when this local preview fails.
pub(crate) fn decode_staged_image(
    attachment: &ImageAttachment,
) -> Result<Arc<RenderImage>, String> {
    let (metadata, encoded) = attachment
        .data_url
        .split_once(',')
        .ok_or_else(|| "This image data is invalid and cannot be previewed.".to_string())?;
    let format = match metadata {
        "data:image/png;base64" => ImageFormat::Png,
        "data:image/jpeg;base64" => ImageFormat::Jpeg,
        _ => return Err("Preview supports PNG and JPEG images only.".into()),
    };

    if encoded.len() > MAX_DATA_URL_BYTES {
        return Err("This image exceeds the 32 MB preview limit.".into());
    }
    if encoded.is_empty() {
        return Err("This image is empty and cannot be previewed.".into());
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "This image data is invalid and cannot be previewed.".to_string())?;
    if bytes.is_empty() {
        return Err("This image is empty and cannot be previewed.".into());
    }

    let mut dimensions_reader = ImageReader::with_format(Cursor::new(bytes.as_slice()), format);
    dimensions_reader.limits(image_limits());
    let dimensions = dimensions_reader
        .into_dimensions()
        .map_err(preview_decode_error)?;
    let (width, height) = dimensions;
    if width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        return Err("This image is too large to preview (maximum 16 megapixels).".into());
    }

    let mut decode_reader = ImageReader::with_format(Cursor::new(bytes), format);
    decode_reader.limits(image_limits());
    let decoded = decode_reader.decode().map_err(preview_decode_error)?;
    let mut pixels = decoded.into_rgba8();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }

    Ok(Arc::new(RenderImage::new(vec![Frame::new(pixels)])))
}

fn image_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOCATION);
    limits
}

fn preview_decode_error(error: image::ImageError) -> String {
    match error {
        image::ImageError::Limits(_) => {
            "This image is too large to preview (maximum 16 megapixels).".into()
        }
        _ => "This image could not be decoded. The attachment is unchanged.".into(),
    }
}
