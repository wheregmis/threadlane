mod gallery;
mod code_samples;
mod file_picker_samples;
mod session;
mod editor;
mod automation;
mod automation_editor;
mod settings;
mod github;
mod terminal;
mod agents;
mod trajectory;
mod browser;
mod files;
mod review;
mod draft_pr;

use gpui::{prelude::*, *};
use gpui_component::Root;

#[cfg(not(target_family = "wasm"))]
actions!(ui_kit_preview, [QuitPreview]);

fn open_gallery(cx: &mut App) {
    gpui_component::init(cx);
    threadlane_ui_kit::init_editor(cx);
    #[cfg(target_family = "wasm")]
    threadlane_ui_kit::init_web_input_shortcuts(cx);
    threadlane_ui_kit::init_terminal_find(cx);
    threadlane_ui_kit::init_conversation_find(cx);
    threadlane_ui_kit::init_prompt_recall(cx);
    threadlane_ui_kit::file_completion::init_file_completion(cx);
    github::init(cx);
    session::init_palette(cx);
    threadlane_ui_kit::automation_form::init(cx);
    threadlane_ui_theme::init_bundled(cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1440.0), px(900.0)),
                cx,
            ))),
            window_min_size: Some(size(px(480.0), px(500.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("Threadlane UI Kit".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.0), px(12.0))),
            }),
            ..Default::default()
        },
        |window, cx| {
            let workspace = cx.new(|cx| session::SessionPreview::new(window, cx));
            cx.new(|cx| Root::new(workspace, window, cx))
        },
    )
    .expect("open UI kit gallery");
    cx.activate(true);
}

#[cfg(not(target_family = "wasm"))]
fn main() {
    if session::import_requested() {
        return;
    }
    gpui_platform::application()
        .with_assets(threadlane_ui_theme::Assets)
        .run(|cx| {
            cx.on_action(|_: &QuitPreview, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", QuitPreview, None)]);
            #[cfg(target_os = "macos")]
            cx.set_menus([Menu::new("Threadlane UI Kit").items([
                MenuItem::action("Quit Threadlane UI Kit", QuitPreview),
            ])]);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            open_gallery(cx);
        });
}

#[cfg(target_family = "wasm")]
fn main() {
    use std::cell::RefCell;
    thread_local! {
        static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
    }
    gpui_platform::web_init();
    // Markdown channel notifications can contend with worker threads and try
    // Atomics.wait on the browser's main thread. Use GPUI's gallery-safe host.
    let handle = gpui_platform::single_threaded_web()
        .with_assets(threadlane_ui_theme::Assets)
        .run_embedded(|cx| {
            open_gallery(cx);
        });
    APPLICATION.with(|cell| *cell.borrow_mut() = Some(handle));
}
