//! Typed confirmation for permanently deleting a project.

use std::sync::Arc;

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::*;
use threadlane_desktop::{Activity, can_confirm_deletion, describe};

use super::menu_item;
use crate::components::{button, field, meta};
use crate::icons::AppIcon;
use crate::tokens::{MD, SM};
use crate::{Desktop, Screen};

pub(super) fn delete_project_item(
    view: &WeakEntity<Desktop>,
    space: String,
    project: String,
    cx: &App,
) -> PopupMenuItem {
    menu_item(view, "Delete project…", move |this, window, cx| {
        this.confirm_project_deletion(space.clone(), project.clone(), window, cx);
    })
    .icon(Icon::new(AppIcon::Trash).text_color(cx.theme().danger))
}

impl Desktop {
    pub(super) fn confirm_project_deletion(
        &mut self,
        space: String,
        project: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) || !self.state.offers_delete(&space, &project) {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Exact project name"));
        cx.observe_in(&input, window, |_, _, window, _| window.refresh())
            .detach();
        let preview = cx.new(|_| None);
        let core = Arc::clone(&self.core);
        let (at, name) = (space.clone(), project.clone());
        let loaded = preview.clone();
        self.run(
            async move { core.read().await.delete_preview(Some(&at), &name).await },
            move |_, result, cx| {
                loaded.update(cx, |preview, cx| {
                    *preview = Some(result.map_err(|error| describe(&error)));
                    cx.notify();
                })
            },
            cx,
        );
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let preview = preview.read(cx);
            let counts = preview.as_ref().and_then(|result| result.as_ref().ok());
            let description = match preview.as_ref() {
                Some(Ok(counts)) => format!("This removes {} from Threadlane for good.", counts.inventory()),
                Some(Err(error)) => error.clone(),
                None => "Loading deletion preview…".to_owned(),
            };
            let typed = input.read(cx).value();
            let enabled = can_confirm_deletion(&project, &typed, counts);
            let (view, space, project) = (view.clone(), space.clone(), project.clone());
            let confirmation = input.clone();
            dialog
                .title(format!("Delete {project}?"))
                .overlay_closable(false)
                .child(v_flex().gap(MD)
                    .child(description)
                    .child("Branches and pull requests on GitHub are not touched.")
                    .child(meta(format!("Type {project:?} to confirm."), cx))
                    .child(field(&input).disabled(counts.is_none())))
                .footer(h_flex().justify_end().gap(SM)
                    .child(button("cancel-project-deletion").label("Cancel")
                        .on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(button("confirm-project-deletion").danger().label("Delete project")
                        .disabled(!enabled)
                        .on_click(move |event, window, cx| {
                            if !enabled || matches!(event, ClickEvent::Keyboard(key) if matches!(key.button, KeyboardButton::Enter)) {
                                return;
                            }
                            let typed = confirmation.read(cx).value();
                            if typed.as_str() != project {
                                return;
                            }
                            window.close_dialog(cx);
                            let _gone = view.update(cx, |this, cx| {
                                this.delete_project(space.clone(), project.clone(), typed.to_string(), cx);
                            });
                        })))
        });
    }

    fn delete_project(
        &mut self,
        space: String,
        project: String,
        typed: String,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        self.state
            .report(Activity::Working(format!("Deleting {project}…")));
        self.run(
            async move {
                let result = core
                    .read()
                    .await
                    .delete_project(Some(&space), &project, &typed)
                    .await;
                (space, project, result)
            },
            |this, (space, project, result), cx| match result {
                Ok((counts, leftovers)) => {
                    this.state
                        .project_deleted(&space, &project, &counts, &leftovers);
                    this.go(Screen::Home, cx);
                    this.refresh(cx);
                }
                Err(error) => {
                    this.state.report(Activity::Failed(describe(&error)));
                    this.refresh(cx);
                }
            },
            cx,
        );
    }
}
