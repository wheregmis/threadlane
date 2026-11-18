//! Attachment preparation and inline thumbnails for chat images.

use std::{
    fs::File,
    io::{Cursor, Read as _},
    path::PathBuf,
    sync::Arc,
};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::attachment::{
    ImageAttachment, MAX_IMAGE_BYTES, check_image_budget, validate_images,
};

use super::{button, meta};
use crate::tokens::{READING_WIDTH, SM, XS};

// Cap the clipboard bytes we copy before handing them to the background worker.
const MAX_CLIPBOARD_BYTES: usize = 128 << 20;

pub(super) fn copy_clipboard_images(images: &[&Image]) -> Result<Vec<Image>, String> {
    let mut remaining_clipboard = MAX_CLIPBOARD_BYTES;
    let mut remaining_message = MAX_IMAGE_BYTES;
    for image in images {
        let len = image.bytes().len();
        remaining_clipboard = remaining_clipboard
            .checked_sub(len)
            .ok_or_else(|| "Clipboard images can total up to 128 MiB.".to_owned())?;
        if !matches!(image.format(), ImageFormat::Tiff | ImageFormat::Bmp) {
            remaining_message = remaining_message
                .checked_sub(len)
                .ok_or_else(|| "Images can total up to 2 MiB per message.".to_owned())?;
        }
    }
    Ok(images.iter().map(|&image| image.clone()).collect())
}

pub(super) fn prepare_clipboard_images(
    clipboard_images: Vec<Image>,
) -> Result<Vec<ImageAttachment>, String> {
    let mut images = Vec::new();
    for (index, image) in clipboard_images.into_iter().enumerate() {
        let name = format!("Pasted image {}", index + 1);
        let format = image.format();
        let extension = format.extension();
        let mut attachment = ImageAttachment::new(format!("{name}.{extension}"), image.bytes);
        if matches!(format, ImageFormat::Tiff | ImageFormat::Bmp) {
            let decoded = attachment.decode().map_err(|_| {
                format!(
                    "Could not decode {:?}. Check the file and its dimensions.",
                    attachment.name()
                )
            })?;
            let mut png = Cursor::new(Vec::new());
            decoded
                .write_to(&mut png, image::ImageFormat::Png)
                .map_err(|_| format!("Could not convert {name} to PNG."))?;
            attachment = ImageAttachment::new(format!("{name}.png"), png.into_inner());
        }
        images.push(attachment);
        check_image_budget(&images)?;
    }
    validate_images(&images)?;
    Ok(images)
}

pub(super) fn read_images(paths: Vec<PathBuf>) -> Result<Vec<ImageAttachment>, String> {
    let mut images = Vec::new();
    let mut remaining = MAX_IMAGE_BYTES;
    for path in paths {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut bytes = Vec::new();
        File::open(&path)
            .and_then(|file| file.take((remaining + 1) as u64).read_to_end(&mut bytes))
            .map_err(|error| format!("Could not read {name:?}. {error}"))?;
        remaining = remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| "Images can total up to 2 MiB per message.".to_owned())?;
        images.push(ImageAttachment::new(name, bytes));
    }
    validate_images(&images)?;
    Ok(images)
}

pub(crate) fn previews(images: &[ImageAttachment], cx: &App) -> Option<Div> {
    (!images.is_empty()).then(|| {
        h_flex()
            .gap(XS)
            .flex_wrap()
            .children(images.iter().map(|image| preview(image, cx)))
    })
}

fn preview(image: &ImageAttachment, cx: &App) -> Div {
    v_flex()
        .gap(XS)
        .w_32()
        .child(thumbnail(image))
        .child(meta(image.name().to_owned(), cx).truncate())
}

pub(super) fn preview_button(id: ElementId, image: &ImageAttachment, cx: &App) -> Button {
    let selected = image.clone();
    button(id)
        .ghost()
        .h_auto()
        .tab_stop(true)
        .accessibility_label(format!("Preview {}", image.name()))
        .tooltip(image.name().to_owned())
        .child(preview(image, cx))
        .on_click(move |_, window, cx| open_preview(selected.clone(), window, cx))
}

fn thumbnail(image: &ImageAttachment) -> Img {
    let attachment = image.clone();
    img(ImageSource::Custom(Arc::new(move |window, cx| {
        window
            .use_asset::<Thumbnail>(&attachment, cx)
            .flatten()
            .map(Ok)
    })))
    .w_full()
    .h_20()
    .object_fit(ObjectFit::Contain)
}

fn open_preview(image: ImageAttachment, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let name = image.name().to_owned();
    let actual_size = cx.new(|_| false);
    let result = cx.new(|_| None);
    window
        .observe(&result, cx, |_, window, _| window.refresh())
        .detach();
    let decoded = cx.background_spawn(async move { image.decode().map(render_image) });
    let output = result.downgrade();
    cx.spawn(async move |cx| {
        let decoded = decoded.await;
        let _ = output.update(cx, |output, cx| {
            *output = Some(decoded);
            cx.notify();
        });
    })
    .detach();

    window.open_dialog(cx, move |dialog, window, cx| {
        let dialog = dialog.title(name.clone()).width(READING_WIDTH);
        let image = match result.read(cx) {
            None => return dialog.child(meta("Loading image…", cx)),
            Some(Err(_)) => return dialog.child(meta("Could not preview this image.", cx)),
            Some(Ok(image)) => image,
        };
        let actual = *actual_size.read(cx);
        let height = window.viewport_size().height * 0.65;
        let mut preview = img(Arc::clone(image));
        if actual {
            let size = image.size(0);
            let scale = window.scale_factor();
            preview = preview
                .w(px(size.width.0 as f32 / scale))
                .h(px(size.height.0 as f32 / scale))
                .object_fit(ObjectFit::Fill)
                .flex_none();
        } else {
            preview = preview.w_full().h_full().object_fit(ObjectFit::Contain);
        }
        dialog.child(
            v_flex()
                .gap(SM)
                .child(
                    h_flex()
                        .gap(XS)
                        .children(
                            [("Fit", false), ("Actual size", true)].map(|(label, value)| {
                                let actual_size = actual_size.clone();
                                button(label)
                                    .small()
                                    .ghost()
                                    .tab_stop(true)
                                    .label(label)
                                    .selected(actual == value)
                                    .on_click(move |_, window, cx| {
                                        actual_size.update(cx, |mode, _| *mode = value);
                                        window.refresh();
                                    })
                            }),
                        ),
                )
                .child(
                    div()
                        .id("image-preview-scroll")
                        .w_full()
                        .h(height)
                        .overflow_scroll()
                        .child(preview),
                ),
        )
    });
}

fn render_image(image: image::DynamicImage) -> Arc<RenderImage> {
    let mut frame = image::Frame::new(image.into_rgba8());
    for pixel in frame.buffer_mut().as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new([frame]))
}

enum Thumbnail {}

impl Asset for Thumbnail {
    type Source = ImageAttachment;
    type Output = Option<Arc<RenderImage>>;

    #[expect(
        clippy::manual_async_fn,
        reason = "the returned future must not capture the non-Send App"
    )]
    fn load(
        source: Self::Source,
        _: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move { Some(render_image(source.decode().ok()?.thumbnail(320, 240))) }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use gpui_kit::{Image, ImageFormat};
    use threadlane_core::attachment::MAX_IMAGE_BYTES;

    use super::{copy_clipboard_images, prepare_clipboard_images};

    fn clipboard_image(
        pixels: &image::DynamicImage,
        clipboard_format: ImageFormat,
        encoded_format: image::ImageFormat,
    ) -> Image {
        let mut bytes = Cursor::new(Vec::new());
        pixels.write_to(&mut bytes, encoded_format).unwrap();
        Image::from_bytes(clipboard_format, bytes.into_inner())
    }

    #[test]
    fn clipboard_normalizes_native_formats_and_preserves_png() {
        let pixels = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            1024,
            768,
            image::Rgb([20, 40, 60]),
        ));
        let tiff = clipboard_image(&pixels, ImageFormat::Tiff, image::ImageFormat::Tiff);
        let bmp = clipboard_image(&pixels, ImageFormat::Bmp, image::ImageFormat::Bmp);
        let png = clipboard_image(&pixels, ImageFormat::Png, image::ImageFormat::Png);
        assert!(tiff.bytes().len() > MAX_IMAGE_BYTES);
        assert!(bmp.bytes().len() > MAX_IMAGE_BYTES);

        let images =
            prepare_clipboard_images(copy_clipboard_images(&[&tiff, &bmp, &png]).unwrap()).unwrap();
        for (index, image) in images.iter().enumerate() {
            assert_eq!(image.name(), format!("Pasted image {}.png", index + 1));
            assert_eq!(image.format(), Some(("image/png", "png")));
            assert_eq!(image.decode().unwrap(), pixels);
        }
        assert_eq!(images[2].bytes(), png.bytes());
    }

    #[test]
    fn clipboard_checks_combined_normalized_size() {
        let mut state = 1u32;
        let pixels = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(768, 768, |_, _| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let [r, g, b, _] = state.to_le_bytes();
            image::Rgb([r, g, b])
        }));
        let tiff = clipboard_image(&pixels, ImageFormat::Tiff, image::ImageFormat::Tiff);
        let bmp = clipboard_image(&pixels, ImageFormat::Bmp, image::ImageFormat::Bmp);
        for input in [&tiff, &bmp] {
            let image = prepare_clipboard_images(copy_clipboard_images(&[input]).unwrap()).unwrap();
            assert!(image[0].bytes().len() > MAX_IMAGE_BYTES / 2);
        }
        let error =
            prepare_clipboard_images(copy_clipboard_images(&[&tiff, &bmp]).unwrap()).unwrap_err();
        assert_eq!(error, "Images can total up to 2 MiB per message.");
    }

    #[test]
    fn clipboard_rejects_corrupt_and_oversized_native_images() {
        let corrupt = Image::from_bytes(ImageFormat::Tiff, b"II*\0".to_vec());
        let pixels = image::DynamicImage::new_rgb8(8193, 1);
        let too_wide = clipboard_image(&pixels, ImageFormat::Bmp, image::ImageFormat::Bmp);
        for input in [corrupt, too_wide] {
            let error = prepare_clipboard_images(vec![input]).unwrap_err();
            assert!(error.starts_with("Could not decode"));
        }
    }
}
