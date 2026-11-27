//! Attachment preparation and inline thumbnails for chat images.

use std::{
    cell::Cell,
    fs::File,
    io::{Cursor, Read as _},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, TextareaState, Undo};
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

const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// The image file a paste names, when the whole pasted text is one absolute
/// path (optionally quoted or a `file://` URL) with an image extension. This
/// only parses; [`read_pasted_image`] checks the file off the UI thread.
pub(super) fn pasted_image_path(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    if text.contains(['\n', '\r']) {
        return None;
    }
    let text = ['"', '\'']
        .into_iter()
        .find_map(|quote| text.strip_prefix(quote)?.strip_suffix(quote))
        .unwrap_or(text);
    let path = match text.strip_prefix("file://") {
        Some(url) => native_path(percent_decode(
            url.strip_prefix("localhost").unwrap_or(url),
        )?)?,
        None => PathBuf::from(text),
    };
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    (path.is_absolute() && IMAGE_EXTENSIONS.contains(&extension.as_str())).then_some(path)
}

/// Reads the image a pasted path names, refusing anything but a regular file
/// before opening it so a FIFO or device cannot block the read.
pub(super) fn read_pasted_image(path: PathBuf) -> Result<Vec<ImageAttachment>, String> {
    let meta = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file.", path.display()));
    }
    read_images(vec![path])
}

/// Finishes a paste of an image path that `input` has already pasted as
/// ordinary text. When `read` yields the images, `attach` takes them and
/// returns whether to take the paste back; that uses the input's own Undo, so
/// every cursor, the replaced selection and earlier history come back as they
/// were. A failed read, or any edit made meanwhile, leaves the text pasted and
/// hands `attach` an error.
pub(super) fn finish_path_paste(
    input: Entity<TextareaState>,
    read: Task<Result<Vec<ImageAttachment>, String>>,
    attach: impl FnOnce(Result<Vec<ImageAttachment>, String>, &mut App) -> bool + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    // Deferred so watching for edits starts after the paste's own change.
    window.defer(cx, move |window, cx| {
        let pasted = input.read(cx).value();
        let edited = Rc::new(Cell::new(false));
        let watch = cx.subscribe(&input, {
            let edited = edited.clone();
            move |_, event: &InputEvent, _| {
                if matches!(event, InputEvent::Change) {
                    edited.set(true);
                }
            }
        });
        window
            .spawn(cx, async move |cx| {
                let images = read.await;
                drop(watch);
                let _ = cx.update(|window, cx| {
                    let images = images.and_then(|images| {
                        // `set_value` changes the text without a Change event.
                        (!edited.get() && input.read(cx).value() == pasted)
                            .then_some(images)
                            .ok_or_else(|| "The draft changed while the image was read.".to_owned())
                    });
                    if attach(images, cx) {
                        input
                            .read(cx)
                            .focus_handle(cx)
                            .dispatch_action(&Undo, window, cx);
                    }
                });
            })
            .detach();
    });
}

#[cfg(unix)]
fn native_path(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;
    Some(std::ffi::OsString::from_vec(bytes).into())
}

#[cfg(not(unix))]
fn native_path(bytes: Vec<u8>) -> Option<PathBuf> {
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    Some(bytes)
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
        let dialog = dialog
            .title(name.clone())
            .width(READING_WIDTH.to_pixels(window.rem_size()));
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

    use super::{
        copy_clipboard_images, pasted_image_path, prepare_clipboard_images, read_pasted_image,
    };

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

    pub(super) fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("threadlane-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(super) fn png() -> Vec<u8> {
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::new(2, 2))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        png.into_inner()
    }

    #[test]
    fn pasted_text_naming_an_image_file_resolves_to_its_path() {
        let dir = temp_dir("paste");
        let path = dir.join("Screenshot 2026-11-26 at 09.17.27.PNG");
        std::fs::write(&path, png()).unwrap();
        let text = path.to_str().unwrap();
        let url = format!("file://{}", text.replace(' ', "%20"));
        for pasted in [
            text.to_owned(),
            format!("  {text}\n"),
            format!("\"{text}\""),
            format!("'{text}'"),
            url.clone(),
            format!("file://localhost{}", text.replace(' ', "%20")),
        ] {
            assert_eq!(pasted_image_path(&pasted), Some(path.clone()), "{pasted:?}");
        }
        let images = read_pasted_image(pasted_image_path(&url).unwrap()).unwrap();
        assert_eq!(images[0].name(), "Screenshot 2026-11-26 at 09.17.27.PNG");

        for pasted in [
            dir.join("notes.txt").to_str().unwrap().to_owned(),
            format!("look at {text}"),
            format!("{text}\n{text}"),
            "Screenshot.png".to_owned(),
            "file://%ZZ.png".to_owned(),
        ] {
            assert_eq!(pasted_image_path(&pasted), None, "{pasted:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pasted_image_reads_refuse_missing_special_and_non_image_files() {
        let dir = temp_dir("paste-read");
        let text = dir.join("notes.png");
        std::fs::write(&text, "not an image").unwrap();
        std::fs::create_dir_all(dir.join("folder.png")).unwrap();
        let fifo = dir.join("pipe.png");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        // A FIFO without a writer would block an open; this must return.
        for path in [dir.join("missing.png"), text, dir.join("folder.png"), fifo] {
            assert!(read_pasted_image(path.clone()).is_err(), "{path:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn file_urls_decode_to_native_filename_bytes() {
        use std::os::unix::ffi::OsStrExt as _;
        let path = pasted_image_path("file:///tmp/shot%FF%20a.png").unwrap();
        assert_eq!(path.as_os_str().as_bytes(), b"/tmp/shot\xFF a.png");
    }

    // macOS filesystems refuse non-UTF-8 names, so only Linux can create one.
    #[cfg(target_os = "linux")]
    #[test]
    fn file_urls_resolve_existing_non_utf8_filenames() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = temp_dir("paste-bytes");
        let path = dir.join(std::ffi::OsStr::from_bytes(b"shot\xFF.png"));
        std::fs::write(&path, png()).unwrap();
        let url = format!("file://{}/shot%FF.png", dir.to_str().unwrap());
        let resolved = pasted_image_path(&url).unwrap();
        assert_eq!(resolved, path);
        assert_eq!(read_pasted_image(resolved).unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod paste_tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui_kit::component::input::{Textarea, TextareaState};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        App, AppContext as _, Bounds, ClipboardItem, Context, Entity, IntoElement,
        ParentElement as _, Point, Render, Styled as _, TestAppContext, Window, WindowBounds,
        WindowHandle, WindowOptions, div, gpui, px, size,
    };

    use super::{finish_path_paste, pasted_image_path, read_pasted_image};

    /// A textarea wired to pasted image paths the way the composer is.
    struct Composer {
        text: Entity<TextareaState>,
        attached: Rc<RefCell<Vec<Result<usize, String>>>>,
    }

    impl Render for Composer {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let (input, attached) = (self.text.clone(), self.attached.clone());
            div()
                .size_full()
                .child(Textarea::new(&self.text).on_paste(move |item, window, cx| {
                    if let Some(path) = item.text().and_then(|text| pasted_image_path(&text)) {
                        let read = cx.background_spawn(async move { read_pasted_image(path) });
                        let attached = attached.clone();
                        let attach = move |images: Result<Vec<_>, String>, _: &mut App| {
                            let ok = images.is_ok();
                            attached
                                .borrow_mut()
                                .push(images.map(|images| images.len()));
                            ok
                        };
                        finish_path_paste(input.clone(), read, attach, window, cx);
                    }
                    false
                }))
        }
    }

    struct Fixture {
        window: WindowHandle<gpui_kit::base::Root>,
        text: Entity<TextareaState>,
        attached: Rc<RefCell<Vec<Result<usize, String>>>>,
        dir: std::path::PathBuf,
    }

    impl Fixture {
        fn new(cx: &mut TestAppContext, name: &str, draft: &str) -> Self {
            cx.update(gpui_kit::init);
            let attached = Rc::<RefCell<Vec<_>>>::default();
            let (window, view) = cx.update(|cx| {
                let (window, view) = gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds {
                            origin: Point::default(),
                            size: size(px(480.), px(480.)),
                        })),
                        ..Default::default()
                    },
                    cx,
                    |window, cx| {
                        let text = cx.new(|cx| {
                            TextareaState::new(window, cx)
                                .rows(4)
                                .default_value(draft.to_owned())
                        });
                        let attached = attached.clone();
                        cx.new(|_| Composer { text, attached })
                    },
                )
                .unwrap();
                (window.downcast::<gpui_kit::base::Root>().unwrap(), view)
            });
            let text = cx.update(|cx| view.read(cx).text.clone());
            let dir = super::tests::temp_dir(name);
            let fixture = Self {
                window,
                text,
                attached,
                dir,
            };
            fixture.ui(cx, |window, text, cx| {
                window.click(("input", text.entity_id()), cx)
            });
            fixture
        }

        fn ui(
            &self,
            cx: &mut TestAppContext,
            act: impl FnOnce(&mut Window, &Entity<TextareaState>, &mut App),
        ) {
            cx.update_window(self.window.into(), |_, window, cx| {
                window.render_frame(cx);
                act(window, &self.text, cx);
            })
            .unwrap();
        }

        fn paste(&self, cx: &mut TestAppContext, text: &str) {
            cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned())));
            self.ui(cx, |window, _, cx| window.press("secondary-v", cx));
        }

        fn press(&self, cx: &mut TestAppContext, key: &str) {
            self.ui(cx, |window, _, cx| window.press(key, cx));
        }

        fn value(&self, cx: &mut TestAppContext) -> String {
            cx.update(|cx| self.text.read(cx).value().to_string())
        }

        fn image(&self) -> String {
            let path = self.dir.join("Screenshot 1.png");
            std::fs::write(&path, super::tests::png()).unwrap();
            format!("'{}'", path.display())
        }

        fn missing(&self) -> String {
            self.dir.join("gone.png").display().to_string()
        }

        /// Lets the read finish and the paste settle.
        fn settle(&self, cx: &mut TestAppContext) {
            cx.run_until_parked();
            self.ui(cx, |_, _, _| {});
            cx.run_until_parked();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[gpui_kit::test]
    fn attached_path_paste_is_undone_back_over_the_selection(cx: &mut TestAppContext) {
        let composer = Fixture::new(cx, "paste-undo", "draft");
        composer.ui(cx, |window, _, cx| window.input(" kept", cx));
        composer.press(cx, "secondary-a");
        composer.paste(cx, &composer.image());
        composer.settle(cx);
        assert_eq!(composer.value(cx), "draft kept");
        assert_eq!(*composer.attached.borrow(), [Ok(1)]);
        composer.press(cx, "secondary-z");
        assert_eq!(composer.value(cx), "draft");
    }

    #[gpui_kit::test]
    fn failed_path_paste_is_an_ordinary_undoable_paste(cx: &mut TestAppContext) {
        let composer = Fixture::new(cx, "paste-fail", "draft");
        composer.ui(cx, |window, _, cx| window.input(" kept", cx));
        composer.press(cx, "secondary-a");
        let missing = composer.missing();
        composer.paste(cx, &missing);
        composer.settle(cx);
        assert_eq!(composer.value(cx), missing);
        assert!(composer.attached.borrow()[0].is_err());
        composer.press(cx, "secondary-z");
        assert_eq!(composer.value(cx), "draft kept");
        composer.press(cx, "secondary-z");
        assert_eq!(composer.value(cx), "draft");
    }

    #[gpui_kit::test]
    fn path_paste_at_two_cursors_pastes_and_undoes_at_both(cx: &mut TestAppContext) {
        let composer = Fixture::new(cx, "paste-cursors", "a\nb");
        let add_cursor = if cfg!(target_os = "macos") {
            "cmd-alt-up"
        } else if cfg!(target_os = "windows") {
            "ctrl-alt-up"
        } else {
            // Linux avoids Ctrl+Alt+arrows, which desktops may reserve.
            "shift-alt-up"
        };
        composer.press(cx, add_cursor);
        let missing = composer.missing();
        composer.paste(cx, &missing);
        composer.settle(cx);
        assert_eq!(composer.value(cx), format!("a{missing}\nb{missing}"));

        composer.press(cx, "secondary-z");
        composer.paste(cx, &composer.image());
        composer.settle(cx);
        assert_eq!(composer.value(cx), "a\nb");
        assert_eq!(composer.attached.borrow()[1], Ok(1));
        // Both cursors survive the take-back.
        composer.ui(cx, |window, _, cx| window.input("x", cx));
        assert_eq!(composer.value(cx), "ax\nbx");
    }

    #[gpui_kit::test]
    fn edits_during_a_slow_read_keep_the_paste_where_it_was(cx: &mut TestAppContext) {
        let composer = Fixture::new(cx, "paste-slow", "see please");
        composer.press(
            cx,
            if cfg!(target_os = "macos") {
                "cmd-up"
            } else {
                "ctrl-home"
            },
        );
        for _ in 0..4 {
            composer.press(cx, "right");
        }
        for paste in [composer.missing(), composer.image()] {
            composer.attached.borrow_mut().clear();
            let before = composer.value(cx);
            composer.paste(cx, &paste);
            // Typed before the worker's read comes back.
            composer.ui(cx, |window, _, cx| window.input(" ", cx));
            composer.settle(cx);
            let (head, tail) = before.split_at(4);
            assert_eq!(composer.value(cx), format!("{head}{paste} {tail}"));
            assert!(composer.attached.borrow()[0].is_err());
            composer.press(cx, "secondary-z");
            composer.press(cx, "secondary-z");
            assert_eq!(composer.value(cx), before);
        }
    }
}
