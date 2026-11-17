//! A new project in one space (wireframe 1, sidebar "+ New project"): a
//! name and an optional description, created through the hub.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

use super::heading;
use crate::{Desktop, Screen};

impl Desktop {
    pub(crate) fn new_project(&self, space: &str, cx: &mut Context<Self>) -> AnyElement {
        let in_space = space.to_owned();
        v_flex()
            .w(px(420.))
            .gap_3()
            .child(heading(format!("New project in {space}")))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("It gets its own lead; tell the lead what you need once it exists."),
            )
            .child(Input::new(&self.project_name))
            .child(Input::new(&self.project_description))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("create-project")
                            .primary()
                            .label("Create project")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.create_project(in_space.clone(), cx);
                            })),
                    )
                    .child(
                        Button::new("cancel-project")
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Home, cx))),
                    ),
            )
            .into_any_element()
    }
}
