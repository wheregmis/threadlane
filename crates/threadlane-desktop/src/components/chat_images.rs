//! File selection and inline thumbnails for chat images.

use std::{fs::File, io::Read as _, path::PathBuf, sync::Arc};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::attachment::{ImageAttachment, MAX_IMAGE_BYTES, validate_images};

use super::{button, meta};
use crate::tokens::{READING_WIDTH, SM, XS};

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
