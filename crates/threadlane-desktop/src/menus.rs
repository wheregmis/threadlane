//! The macOS menu bar: the app menu (About, Hide, Quit), Edit wired to GPUI
//! Kit's text field actions, View (full screen, reload) and Window
//! (Minimize, Zoom), with their usual shortcuts.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{Copy, Cut, Enter, Escape, Paste, Redo, SelectAll, Undo};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::*;
use threadlane_desktop::{Appearance, DecisionCommand};

use crate::{Desktop, Screen};

actions!(
    threadlane,
    [
        About,
        Hide,
        Quit,
        ToggleFullScreen,
        Reload,
        Minimize,
        Zoom,
        SystemAppearance,
        LightAppearance,
        DarkAppearance,
        FocusComposer,
        GoHome,
        ToggleSidebar,
        OpenSettings,
        ShowShortcuts,
        OpenShortcutsMenu,
        Dismiss,
    ]
);

#[derive(Action, Clone, PartialEq)]
#[action(namespace = threadlane, no_json)]
pub(crate) struct SidebarShortcut(pub(crate) usize);

#[derive(Action, Clone, PartialEq)]
#[action(namespace = threadlane, no_json)]
pub(crate) struct DecisionShortcut(pub(crate) DecisionCommand);

pub(crate) const KEY_CONTEXT: &str = "Threadlane";
const OUTSIDE_INPUT: &str = "Threadlane && !Input && !Dialog";

struct Shortcut {
    group: &'static str,
    label: &'static str,
    bindings: Vec<KeyBinding>,
}

fn shortcut(
    group: &'static str,
    label: &'static str,
    bindings: impl IntoIterator<Item = KeyBinding>,
) -> Shortcut {
    Shortcut {
        group,
        label,
        bindings: bindings.into_iter().collect(),
    }
}

fn shortcuts() -> Vec<Shortcut> {
    let single =
        |key: &str, command| KeyBinding::new(key, DecisionShortcut(command), Some(OUTSIDE_INPUT));
    vec![
        shortcut(
            "Navigate",
            "Focus composer",
            [
                KeyBinding::new("cmd-k", FocusComposer, None),
                KeyBinding::new("/", FocusComposer, Some(OUTSIDE_INPUT)),
            ],
        ),
        shortcut(
            "Navigate",
            "Go Home",
            [KeyBinding::new("g h", GoHome, Some(OUTSIDE_INPUT))],
        ),
        shortcut(
            "Navigate",
            "Go to sidebar row",
            (1..=9).map(|n| KeyBinding::new(&format!("cmd-{n}"), SidebarShortcut(n - 1), None)),
        ),
        shortcut(
            "Navigate",
            "Toggle sidebar",
            [KeyBinding::new("cmd-b", ToggleSidebar, None)],
        ),
        shortcut(
            "Navigate",
            "Open Settings",
            [KeyBinding::new("cmd-,", OpenSettings, None)],
        ),
        shortcut(
            "Decisions",
            "Next pending decision",
            [single("j", DecisionCommand::Next)],
        ),
        shortcut(
            "Decisions",
            "Previous pending decision",
            [single("k", DecisionCommand::Previous)],
        ),
        shortcut(
            "Decisions",
            "Write other instructions",
            [single("r", DecisionCommand::Reply)],
        ),
        shortcut(
            "Decisions",
            "Choose an option",
            (1..=9).map(|n| single(&n.to_string(), DecisionCommand::Pick(n - 1))),
        ),
        shortcut(
            "Window",
            "Keyboard shortcuts",
            [KeyBinding::new("?", ShowShortcuts, Some(OUTSIDE_INPUT))],
        ),
        shortcut(
            "Window",
            "Dismiss dialog, leave field, or clear decision focus",
            [KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT))],
        ),
        shortcut("Window", "Reload", [KeyBinding::new("cmd-r", Reload, None)]),
        shortcut(
            "Window",
            "Toggle full screen",
            [KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None)],
        ),
        shortcut(
            "Window",
            "Minimize",
            [KeyBinding::new("cmd-m", Minimize, None)],
        ),
        shortcut(
            "Window",
            "Hide Threadlane",
            [KeyBinding::new("cmd-h", Hide, None)],
        ),
        shortcut(
            "Window",
            "Quit Threadlane",
            [KeyBinding::new("cmd-q", Quit, None)],
        ),
    ]
}

fn app_bindings() -> impl Iterator<Item = KeyBinding> {
    shortcuts()
        .into_iter()
        .flat_map(|shortcut| shortcut.bindings)
}

fn binding_label(binding: &KeyBinding) -> String {
    binding
        .keystrokes()
        .iter()
        .map(|stroke| Kbd::format(stroke.inner()))
        .collect::<Vec<_>>()
        .join(" then ")
}

fn kit_shortcuts() -> [(&'static str, Box<dyn Action>); 9] {
    [
        (
            "Send, submit question note, or confirm",
            Box::new(Enter {
                secondary: false,
                shift: false,
            }),
        ),
        (
            "Send from the composer",
            Box::new(Enter {
                secondary: false,
                shift: true,
            }),
        ),
        (
            "Send from the composer",
            Box::new(Enter {
                secondary: true,
                shift: false,
            }),
        ),
        ("Undo", Box::new(Undo)),
        ("Redo", Box::new(Redo)),
        ("Cut", Box::new(Cut)),
        ("Copy", Box::new(Copy)),
        ("Paste", Box::new(Paste)),
        ("Select All", Box::new(SelectAll)),
    ]
}

pub(crate) fn shortcut_rows(cx: &App) -> Vec<(&'static str, &'static str, Vec<String>)> {
    let mut rows: Vec<_> = shortcuts()
        .into_iter()
        .map(|shortcut| {
            (
                shortcut.group,
                shortcut.label,
                shortcut.bindings.iter().map(binding_label).collect(),
            )
        })
        .collect();
    let bindings = cx.key_bindings();
    let bindings = bindings.borrow();
    rows.extend(kit_shortcuts().into_iter().filter_map(|(label, action)| {
        let mut keys: Vec<_> = bindings
            .bindings_for_action(action.as_ref())
            .map(binding_label)
            .collect();
        keys.dedup();
        (!keys.is_empty()).then_some(("Editing", label, keys))
    }));
    rows
}

pub(crate) fn shortcut_hint(cx: &App, action: &dyn Action) -> Option<String> {
    cx.key_bindings()
        .borrow()
        .bindings_for_action(action)
        .next()
        .map(binding_label)
}

/// Binds the shortcuts and the app-wide actions; `reload` refreshes now.
pub(crate) fn install(
    appearance: Appearance,
    desktop: WeakEntity<Desktop>,
    cx: &mut App,
    reload: impl Fn(&mut App) + 'static,
    change_appearance: impl Fn(Appearance, &mut Window, &mut App) + Clone + 'static,
) {
    cx.bind_keys(app_bindings());
    desktop_action(cx, &desktop, |this, _: &FocusComposer, window, cx| {
        this.focus_composer(window, cx);
    });
    desktop_action(cx, &desktop, |this, _: &GoHome, window, cx| {
        if !window.has_focused_input(cx) && !window.has_active_dialog(cx) {
            this.go(Screen::Home, cx);
        }
    });
    desktop_action(
        cx,
        &desktop,
        |this, action: &SidebarShortcut, window, cx| {
            if !window.has_active_dialog(cx)
                && let Some(screen) = this.sidebar_screen(action.0)
            {
                this.keyboard_focus.focus(window, cx);
                this.go(screen, cx);
            }
        },
    );
    desktop_action(cx, &desktop, |this, _: &ToggleSidebar, window, cx| {
        if !window.has_active_dialog(cx) {
            this.sidebar_collapsed = !this.sidebar_collapsed;
            cx.notify();
        }
    });
    desktop_action(cx, &desktop, |this, _: &OpenSettings, window, cx| {
        if !window.has_active_dialog(cx) {
            this.keyboard_focus.focus(window, cx);
            this.go(Screen::Settings, cx);
        }
    });
    desktop_action(cx, &desktop, |this, _: &ShowShortcuts, window, cx| {
        if !window.has_focused_input(cx) {
            this.show_shortcuts(window, cx);
        }
    });
    desktop_action(cx, &desktop, |this, _: &OpenShortcutsMenu, window, cx| {
        this.show_shortcuts(window, cx);
    });
    desktop_action(
        cx,
        &desktop,
        |this, action: &DecisionShortcut, window, cx| {
            this.decision_shortcut(action.0, window, cx);
        },
    );
    desktop_action(cx, &desktop, |this, _: &Dismiss, window, cx| {
        this.dismiss_keyboard_focus(window, cx);
    });
    desktop_action(cx, &desktop, |this, _: &Escape, window, cx| {
        this.dismiss_keyboard_focus(window, cx);
    });
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
    let system = change_appearance.clone();
    let light = change_appearance.clone();
    cx.on_action(move |_: &SystemAppearance, cx| {
        let change = system.clone();
        on_window(cx, move |window, cx| change(Appearance::System, window, cx));
    });
    cx.on_action(move |_: &LightAppearance, cx| {
        let change = light.clone();
        on_window(cx, move |window, cx| change(Appearance::Light, window, cx));
    });
    cx.on_action(move |_: &DarkAppearance, cx| {
        let change = change_appearance.clone();
        on_window(cx, move |window, cx| change(Appearance::Dark, window, cx));
    });
    set(false, appearance, cx);
}

fn desktop_action<A: Action + Clone>(
    cx: &mut App,
    view: &WeakEntity<Desktop>,
    handle: fn(&mut Desktop, &A, &mut Window, &mut Context<Desktop>),
) {
    let view = view.clone();
    cx.on_action(move |action: &A, cx| {
        let action = action.clone();
        let view = view.clone();
        on_window(cx, move |window, cx| {
            let _gone = view.update(cx, |this, cx| handle(this, &action, window, cx));
        });
    });
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
pub(crate) fn set(full_screen: bool, appearance: Appearance, cx: &App) {
    let full_screen = if full_screen {
        "Exit Full Screen"
    } else {
        "Enter Full Screen"
    };
    cx.set_menus([
        Menu::new("Threadlane").items([
            MenuItem::action("About Threadlane", About),
            MenuItem::action("Settings…", OpenSettings),
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
            MenuItem::action("Go Home", GoHome),
            MenuItem::action("Focus Composer", FocusComposer),
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::submenu(
                Menu::new("Appearance").items([
                    MenuItem::action("System", SystemAppearance)
                        .checked(appearance == Appearance::System),
                    MenuItem::action("Light", LightAppearance)
                        .checked(appearance == Appearance::Light),
                    MenuItem::action("Dark", DarkAppearance)
                        .checked(appearance == Appearance::Dark),
                ]),
            ),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
        Menu::new("Help").items([MenuItem::action("Keyboard Shortcuts", OpenShortcutsMenu)]),
    ]);
}

#[cfg(test)]
mod tests {
    use gpui_kit::{KeyBinding, KeyContext, Keymap, Keystroke};
    use threadlane_desktop::DecisionCommand;

    use super::{
        DecisionShortcut, KEY_CONTEXT, OUTSIDE_INPUT, ShowShortcuts, app_bindings, binding_label,
        shortcuts,
    };

    #[test]
    fn shortcut_catalog_matches_registered_bindings_and_excludes_text_input() {
        let keymap = Keymap::new(app_bindings().collect());
        let desktop = KeyContext::parse(KEY_CONTEXT).unwrap();
        let input = KeyContext::parse("Input").unwrap();
        let dialog = KeyContext::parse("Dialog").unwrap();
        let groups = shortcuts();
        let count = groups
            .iter()
            .map(|shortcut| shortcut.bindings.len())
            .sum::<usize>();
        assert_eq!(count, keymap.bindings().len());
        for shortcut in groups {
            for listed in shortcut.bindings {
                let keys: Vec<_> = listed
                    .keystrokes()
                    .iter()
                    .map(|key| key.inner().clone())
                    .collect();
                assert!(!binding_label(&listed).is_empty(), "{}", shortcut.label);
                let (matching, _) =
                    keymap.bindings_for_input(&keys, std::slice::from_ref(&desktop));
                assert!(
                    matching
                        .iter()
                        .any(|bound| listed.action().partial_eq(bound.action())),
                    "{}",
                    shortcut.label
                );
                if listed.predicate()
                    == KeyBinding::new(
                        "j",
                        DecisionShortcut(DecisionCommand::Next),
                        Some(OUTSIDE_INPUT),
                    )
                    .predicate()
                {
                    assert!(
                        keymap
                            .bindings_for_input(&keys, &[desktop.clone(), input.clone()])
                            .0
                            .is_empty()
                    );
                    assert!(
                        keymap
                            .bindings_for_input(&keys, &[desktop.clone(), dialog.clone()])
                            .0
                            .is_empty()
                    );
                }
            }
        }
        let (matching, _) =
            keymap.bindings_for_input(&[Keystroke::parse("shift-/->?").unwrap()], &[desktop]);
        assert!(
            matching
                .iter()
                .any(|binding| binding.action().partial_eq(&ShowShortcuts))
        );
    }
}
