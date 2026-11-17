//! Issue creation presentation. Hosts own the draft, request, and completion.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::dialog::Dialog;
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::{ActiveTheme, Disableable, WindowExt};
use std::rc::Rc;

pub fn validate_issue_title(title: &str) -> Result<(), &'static str> {
    if title.trim().is_empty() {
        Err("Enter a title for the issue.")
    } else {
        Ok(())
    }
}

pub fn github_issue_create_form(
    repository: impl Into<SharedString>,
    title: &Entity<InputState>,
    description: &Entity<TextareaState>,
    creating: bool,
    error: Option<&str>,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme().colors;
    div()
        .id("github-issue-create-form")
        .debug_selector(|| "github-issue-create-form".into())
        .role(Role::Group)
        .aria_label("New GitHub issue")
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_4()
        .text_sm()
        .child(
            div()
                .min_w_0()
                .text_color(theme.muted_foreground)
                .child(repository.into()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child("Title · required")
                .child(
                    Input::new(title)
                        .aria_label("Issue title, required")
                        .disabled(creating),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child("Description")
                .child(
                    Textarea::new(description)
                        .aria_label("Issue description, optional, Markdown supported")
                        .disabled(creating),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Optional · Markdown supported"),
                ),
        )
        .children(error.map(|error| {
            div()
                .id("github-issue-create-error")
                .debug_selector(|| "github-issue-create-error".into())
                .role(Role::Alert)
                .aria_label(error.to_owned())
                .text_color(theme.danger)
                .child(error.to_owned())
        }))
}

/// Keep the dialog mounted until the host acknowledges successful creation.
pub fn github_issue_create_dialog(
    dialog: Dialog,
    creating: bool,
    on_create: impl Fn(&mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&mut App) + 'static,
) -> Dialog {
    let on_create = Rc::new(on_create);
    let confirm = on_create.clone();
    let on_dismiss = Rc::new(on_dismiss);
    let cancel = on_dismiss.clone();
    dialog
        .title("New issue")
        .overlay_closable(false)
        .keyboard(!creating)
        .close_button(!creating)
        .footer(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("cancel-create-issue")
                        .debug_selector(|| "cancel-create-issue".into())
                        .label("Cancel")
                        .disabled(creating)
                        .on_click(move |_, window, cx| {
                            cancel(cx);
                            window.close_dialog(cx);
                        }),
                )
                .child(
                    Button::new("confirm-create-issue")
                        .debug_selector(|| "confirm-create-issue".into())
                        .primary()
                        .label(if creating {
                            "Creating issue…"
                        } else {
                            "Create issue"
                        })
                        .loading(creating)
                        .disabled(creating)
                        .on_click(move |_, window, cx| {
                            if !creating {
                                confirm(window, cx);
                            }
                        }),
                ),
        )
        .on_ok(move |_, window, cx| {
            if !creating {
                on_create(window, cx);
            }
            false
        })
        .on_close(move |_, _, cx| on_dismiss(cx))
}
