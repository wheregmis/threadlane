//! Controlled browser presentation. Hosts own navigation, tab lifetime and page content.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Icon, IconName, Selectable, Sizable};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserAction {
    Back,
    Forward,
    Reload,
    ToggleAnnotate,
    SelectTab(usize),
    CloseTab(usize),
    NewTab,
}

/// The shared toolbar, tab strip and annotation/empty states. Append page content
/// with `browser_viewport`; the host retains input subscriptions and tab scrolling.
pub fn browser_chrome(
    address: &Entity<InputState>,
    tabs: &[(usize, String)],
    active_id: Option<usize>,
    tab_scroll: &ScrollHandle,
    annotating: bool,
    secure: bool,
    on_action: impl Fn(&BrowserAction, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let callback = std::rc::Rc::new(on_action);
    let request = move |action: BrowserAction| {
        let callback = callback.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| callback(&action, window, cx)
    };
    div()
        .id("browser-panel")
        .role(Role::Application)
        .tab_group()
        .size_full()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .py_0p5()
                .rounded_md()
                .when(annotating, |bar| {
                    bar.bg(cx.theme().warning.opacity(0.08))
                        .border_1()
                        .border_color(cx.theme().warning.opacity(0.25))
                })
                .child(
                    Button::new("browser-back")
                        .debug_selector(|| "browser-back".into())
                        .icon(IconName::ArrowLeft)
                        .accessibility_label("Go back")
                        .tooltip("Go back")
                        .ghost()
                        .xsmall()
                        .on_click(request(BrowserAction::Back)),
                )
                .child(
                    Button::new("browser-forward")
                        .debug_selector(|| "browser-forward".into())
                        .icon(IconName::ArrowRight)
                        .accessibility_label("Go forward")
                        .tooltip("Go forward")
                        .ghost()
                        .xsmall()
                        .on_click(request(BrowserAction::Forward)),
                )
                .child(
                    Button::new("browser-reload")
                        .debug_selector(|| "browser-reload".into())
                        .icon(Icon::default().path("icons/refresh-cw.svg"))
                        .accessibility_label("Reload page")
                        .tooltip("Reload page")
                        .ghost()
                        .xsmall()
                        .on_click(request(BrowserAction::Reload)),
                )
                .child(
                    Button::new("browser-annotate")
                        .debug_selector(|| "browser-annotate".into())
                        .icon(Icon::default().path("icons/crosshair.svg"))
                        .accessibility_label(if annotating {
                            "Stop annotating. Click elements, then attach. Escape cancels."
                        } else {
                            "Annotate page elements into the composer"
                        })
                        .tooltip(if annotating {
                            "Annotating — click elements, then attach (Esc cancels)"
                        } else {
                            "Annotate: pick page elements into the composer"
                        })
                        .ghost()
                        .xsmall()
                        .selected(annotating)
                        .on_click(request(BrowserAction::ToggleAnnotate)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .debug_selector(|| "browser-address-field".into())
                        .when(secure, |row| {
                            row.child(
                                div()
                                    .flex_none()
                                    .text_color(cx.theme().success)
                                    .child(Icon::default().path("icons/lock.svg").xsmall()),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(address).aria_label("Browser address")),
                        ),
                ),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .min_w_0()
                .child(
                    div()
                        .id("browser-tab-strip")
                        .debug_selector(|| "browser-tab-strip".into())
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .overflow_x_scroll()
                        .track_scroll(tab_scroll)
                        .children(tabs.iter().cloned().map(|(id, url)| {
                            let selected = Some(id) == active_id;
                            let title = browser_tab_title(&url);
                            div()
                                .id(SharedString::from(format!("browser-tab-{id}")))
                                .group(SharedString::from(format!("browser-tab-group-{id}")))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .rounded_md()
                                .bg(if selected {
                                    cx.theme().list_active
                                } else {
                                    gpui::transparent_black()
                                })
                                .child(
                                    Button::new(SharedString::from(format!("browser-tab-{id}")))
                                        .debug_selector(move || format!("browser-tab-{id}"))
                                        .label(title.clone())
                                        .accessibility_label(format!(
                                            "Show browser tab {title}, {url}{}",
                                            if selected { ", selected" } else { "" }
                                        ))
                                        .tooltip(url.clone())
                                        .ghost()
                                        .xsmall()
                                        .selected(selected)
                                        .on_click(request(BrowserAction::SelectTab(id))),
                                )
                                .child({
                                    let is_active_tab = selected;
                                    let group_name =
                                        SharedString::from(format!("browser-tab-group-{id}"));
                                    div()
                                        .when(!is_active_tab, |el| {
                                            el.invisible()
                                                .group_hover(group_name, |el| el.visible())
                                        })
                                        .child(
                                            Button::new(SharedString::from(format!(
                                                "browser-tab-close-{id}"
                                            )))
                                            .debug_selector(move || {
                                                format!("browser-tab-close-{id}")
                                            })
                                            .icon(IconName::Close)
                                            .accessibility_label(format!("Close tab {title}"))
                                            .ghost()
                                            .xsmall()
                                            .on_click(request(BrowserAction::CloseTab(id))),
                                        )
                                })
                        })),
                )
                .child(
                    div()
                        .debug_selector(|| "browser-new-tab-control".into())
                        .flex_shrink_0()
                        .child(
                            Button::new("browser-new-tab")
                                .debug_selector(|| "browser-new-tab".into())
                                .icon(IconName::Plus)
                                .accessibility_label("New browser tab")
                                .tooltip("New tab")
                                .flex_shrink_0()
                                .ghost()
                                .xsmall()
                                .on_click(request(BrowserAction::NewTab)),
                        ),
                ),
        )
        .when(annotating, |panel| {
            panel.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_1()
                    .py_0p5()
                    .rounded_md()
                    .bg(cx.theme().warning.opacity(0.08))
                    .child(
                        div()
                            .size_4()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(cx.theme().warning)
                            .child(Icon::default().path("icons/crosshair.svg").xsmall()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(cx.theme().warning)
                            .child("Click elements, Enter attaches \u{2014} Esc cancels"),
                    ),
            )
        })
        .when(tabs.is_empty(), |panel| {
            panel.child(
                div().flex_1().flex().items_center().justify_center().child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(IconName::Globe),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No tabs open"),
                        )
                        .child(
                            Button::new("open-first-tab")
                                .debug_selector(|| "open-first-tab".into())
                                .label("New Tab")
                                .small()
                                .on_click(request(BrowserAction::NewTab)),
                        ),
                ),
            )
        })
}

pub fn browser_viewport(cx: &App) -> Div {
    div()
        .debug_selector(|| "browser-viewport".into())
        .flex_1()
        .min_w_0()
        .min_h_0()
        .border_1()
        .border_color(cx.theme().border)
        .rounded_md()
        .overflow_hidden()
}

pub fn browser_tab_title(url: &str) -> String {
    let bare = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = bare.split('/').next().unwrap_or(bare);
    let short: String = host.chars().take(24).collect();
    if short.len() < host.len() {
        format!("{short}…")
    } else {
        short
    }
}
