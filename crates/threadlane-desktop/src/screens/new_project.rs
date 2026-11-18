//! A new project in one space (wireframe 1, sidebar "+ New project"): a
//! name and an optional description, created through the hub.

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use crate::components::{button, field, heading, meta};
use crate::icons::AppIcon;
use crate::tokens::{COMPACT_FORM_WIDTH, MD, SM};
use crate::{Desktop, Screen};

impl Desktop {
    pub(crate) fn new_project(&self, space: &str, cx: &mut Context<Self>) -> AnyElement {
        let in_space = space.to_owned();
        v_flex()
            .w_full()
            .max_w(COMPACT_FORM_WIDTH)
            .mx_auto()
            .gap(MD)
            .child(heading(format!("New project in {space}")))
            .child(meta(
                "It gets its own lead; tell the lead what you need once it exists.",
                cx,
            ))
            .child(field(&self.project_name))
            .child(field(&self.project_description))
            .child(
                h_flex()
                    .gap(SM)
                    .child(
                        button("create-project")
                            .primary()
                            .icon(AppIcon::FolderPlus)
                            .label("Create project")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.create_project(in_space.clone(), cx);
                            })),
                    )
                    .child(
                        button("cancel-project")
                            .icon(AppIcon::X)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Home, cx))),
                    ),
            )
            .into_any_element()
    }
}
