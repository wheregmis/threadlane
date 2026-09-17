use gpui::{
    div, App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    Role, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{button::Button, ActiveTheme};
use threadlane_ui_state::AppState;

use crate::WorkspaceView;

/// Keeps the native window responsive while the existing startup loader reads projects.
pub struct StartupView {
    workspace: Option<Entity<WorkspaceView>>,
    failed: bool,
}

impl StartupView {
    pub fn build(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let mut view = Self {
                workspace: None,
                failed: false,
            };
            view.load(window, cx);
            view
        })
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.failed = false;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        // Filesystem and credential reads must not block the native event loop.
        // Use an OS thread rather than GPUI's small-stack GCD executor.
        let worker = std::thread::Builder::new()
            .name("workspace-startup".into())
            .spawn(move || {
                let _ = sender.send(AppState::load());
            });
        if worker.is_err() {
            self.failed = true;
            cx.notify();
            return;
        }
        self.receive(receiver, window, cx);
    }

    fn receive(
        &mut self,
        receiver: tokio::sync::oneshot::Receiver<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let result = receiver.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(state) => this.workspace = Some(WorkspaceView::build(state, window, cx)),
                    Err(_) => this.failed = true,
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for StartupView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(workspace) = &self.workspace {
            return workspace.clone().into_any_element();
        }
        let content = div()
            .id("startup-screen")
            .tab_group()
            .role(Role::Group)
            .aria_label("Workspace startup")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);
        if self.failed {
            content
                .child(
                    div()
                        .id("startup-error")
                        .role(Role::Alert)
                        .aria_label("Could not load your workspace.")
                        .child("Could not load your workspace."),
                )
                .child(
                    Button::new("retry-workspace-startup")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, window, cx| this.load(window, cx))),
                )
                .into_any_element()
        } else {
            content
                .child(
                    div()
                        .id("startup-status")
                        .role(Role::Status)
                        .aria_label("Loading projects…")
                        .child("Loading projects…"),
                )
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    #[gpui::test]
    fn pending_startup_renders_and_worker_failure_is_recoverable(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let (view, cx) = cx.add_window_view(move |window, cx| {
            let mut view = super::StartupView {
                workspace: None,
                failed: false,
            };
            view.receive(receiver, window, cx);
            view
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(!view.read(cx).failed);
            assert!(view.read(cx).workspace.is_none());
        });
        drop(sender);
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(view.read(cx).failed);
            assert!(view.read(cx).workspace.is_none());
            window.focus_next(cx);
            assert!(window.focused(cx).is_some(), "Retry must be keyboard reachable");
        });
    }
}
