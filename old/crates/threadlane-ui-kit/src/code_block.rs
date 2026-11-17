//! Controlled conversation code chrome. Hosts retain file and terminal guards.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Icon, IconName, Selectable, Sizable};

pub fn code_block_surface(key: &str, cx: &App) -> Stateful<Div> {
    let selector = format!("code-block-{key}");
    super::result_surface(&cx.theme().colors)
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .my_2p5()
        .rounded_xl()
        .border_color(cx.theme().border.opacity(0.35))
        .bg(cx.theme().muted.opacity(0.18))
}

pub fn code_block_header(
    key: &str,
    language: &str,
    path: Option<&str>,
    actions: impl IntoElement,
    cx: &App,
) -> Div {
    let language = if language.trim().is_empty() {
        "code"
    } else {
        language.trim()
    };
    super::result_header(&cx.theme().colors)
        .flex_wrap()
        .justify_between()
        .px_3p5()
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(cx.theme().muted_foreground)
                        .child(language.to_owned()),
                )
                .children(path.map(|path| {
                    let path: SharedString = path.to_owned().into();
                    let tip = path.clone();
                    div()
                        .id(SharedString::from(format!("code-path-{key}")))
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .aria_label(path.clone())
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                        })
                        .child(Icon::new(IconName::File).xsmall().flex_shrink_0())
                        .child(div().min_w_0().truncate().child(path))
                })),
        )
        .child(actions)
}

pub fn code_block_actions() -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_end()
        .gap_1()
        .min_w_0()
        .max_w_full()
}

pub fn code_block_run_button(key: &str) -> Button {
    let selector = format!("run-term-{key}");
    Button::new(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .icon(IconName::SquareTerminal)
        .label("Run in terminal")
        .accessibility_label("Run in active project terminal")
        .tooltip("Run in active project terminal")
        .xsmall()
        .secondary()
}

pub fn code_block_open_button(key: &str) -> Button {
    let selector = format!("open-edit-{key}");
    Button::new(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .icon(IconName::File)
        .label("Open in editor")
        .accessibility_label("Open file in central editor")
        .tooltip("Open file in central editor")
        .xsmall()
        .ghost()
}

pub fn code_block_copy_button(key: &str, copied: bool, cx: &App) -> Button {
    super::conversation::copy_feedback_button(format!("copy-code-{key}"), copied, cx)
        .accessibility_label(if copied {
            "Code copied"
        } else {
            "Copy code to clipboard"
        })
        .tooltip(if copied {
            "Code copied"
        } else {
            "Copy code to clipboard"
        })
}

/// Per-block soft-wrap toggle. `selected`/`toggled` expose the on state to
/// eyes and assistive technology; the button stays quiet when off.
pub fn code_block_wrap_button(key: &str, wrapped: bool) -> Button {
    let selector = format!("wrap-lines-{key}");
    Button::new(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .label("Wrap lines")
        .accessibility_label(if wrapped {
            "Stop wrapping code lines"
        } else {
            "Wrap code lines"
        })
        .tooltip(if wrapped {
            "Stop wrapping code lines"
        } else {
            "Wrap code lines"
        })
        .xsmall()
        .ghost()
        .selected(wrapped)
        .toggled(wrapped)
}

/// Code body layout. Unwrapped keeps source line boundaries inside a
/// block-local horizontal scroller (keyed per block so positions don't
/// leak between blocks); wrapped constrains the text to the pane width and
/// soft-wraps long lines and tokens without touching the source that
/// Copy/Run consume.
pub fn code_block_body(
    key: &str,
    cx: &App,
    wrapped: bool,
    content: impl IntoElement,
) -> AnyElement {
    let base = |mode: &'static str| {
        let selector = format!("code-body-{mode}-{key}");
        div()
            .min_w_0()
            .p_3()
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .debug_selector(move || selector.clone())
    };
    if wrapped {
        base("wrap")
            .w_full()
            .overflow_x_hidden()
            .child(content)
            .into_any_element()
    } else {
        base("scroll")
            .overflow_x_scrollbar()
            .id(SharedString::from(format!("code-body-scroll-{key}")))
            .child(content)
            .into_any_element()
    }
}
