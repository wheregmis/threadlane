//! The macOS menu bar: the app menu (About, Hide, Quit), Edit wired to GPUI
//! Kit's text field actions, View (full screen, reload) and Window
//! (Minimize, Zoom), with their usual shortcuts.

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::*;

actions!(
    threadlane,
    [About, Hide, Quit, ToggleFullScreen, Reload, Minimize, Zoom]
);

/// Binds the shortcuts and the app-wide actions; `reload` refreshes now.
pub(crate) fn install(cx: &mut App, reload: impl Fn(&mut App) + 'static) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("cmd-r", Reload, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(move |_: &Reload, cx| reload(cx));
    cx.on_action(|_: &Minimize, cx| on_window(cx, |window, _| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| on_window(cx, |window, _| window.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| {
        on_window(cx, |window, _| window.toggle_fullscreen());
    });
    cx.on_action(|_: &About, cx| {
        on_window(cx, |window, cx| {
            let detail = format!("Version {}", env!("CARGO_PKG_VERSION"));
            let _answer =
                window.prompt(PromptLevel::Info, "Threadlane", Some(&detail), &["OK"], cx);
        });
    });
    set(false, cx);
}

/// Runs `act` on the active window once the current update is done.
fn on_window(cx: &mut App, act: impl FnOnce(&mut Window, &mut App) + 'static) {
    if let Some(window) = cx.active_window() {
        cx.defer(move |cx| {
            let _gone = window.update(cx, |_, window, cx| act(window, cx));
        });
    }
}

/// Sets the menu bar; View says "Exit Full Screen" while `full_screen`.
pub(crate) fn set(full_screen: bool, cx: &App) {
    let full_screen = if full_screen {
        "Exit Full Screen"
    } else {
        "Enter Full Screen"
    };
    cx.set_menus([
        Menu::new("Threadlane").items([
            MenuItem::action("About Threadlane", About),
            MenuItem::separator(),
            MenuItem::action("Hide Threadlane", Hide),
            MenuItem::separator(),
            MenuItem::action("Quit Threadlane", Quit),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action(full_screen, ToggleFullScreen),
            MenuItem::separator(),
            MenuItem::action("Reload", Reload),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
    ]);
}
