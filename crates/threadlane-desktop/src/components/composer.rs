//! Shared composer and the adapter between GPUI inputs and draft state.

use std::{sync::Arc, time::Duration};

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState, Undo};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::time::{Clock as _, SystemClock};
use threadlane_desktop::{Activity, Destination, Draft, Outgoing, sends_on_enter};

use super::{button, composer_surface, meta};
use crate::icons::AppIcon;
use crate::tokens::{COMPOSER_MAX_ROWS, FIELD_RADIUS, SM, TYPE_BODY, XS};
use crate::{Desktop, Screen, Tab};

pub(crate) fn input(
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TextareaState> {
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder(placeholder)
            .auto_grow(1, COMPOSER_MAX_ROWS)
            .submit_on_enter(sends_on_enter(false, false))
    })
}

pub(crate) fn subscribe(
    draft: Draft,
    input: &Entity<TextareaState>,
    window: &mut Window,
    cx: &mut Context<Desktop>,
) -> Subscription {
    cx.subscribe_in(
        input,
        window,
        move |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                if let Some(destination) = this
                    .composer_destination()
                    .filter(|target| target.draft() == draft)
                    && this.composer_shown.as_ref() == Some(&destination)
                {
                    this.state.edit_draft(&destination, &input.read(cx).value());
                    cx.notify();
                }
            }
            InputEvent::PressEnter { secondary, shift }
                if sends_on_enter(*secondary, *shift) && this.visible_draft() == Some(draft) =>
            {
                this.submit_draft(window, cx);
            }
            _ => {}
        },
    )
}

impl Desktop {
    pub(crate) fn visible_draft(&self) -> Option<Draft> {
        self.composer_destination()
            .map(|destination| destination.draft())
    }

    fn composer_destination(&self) -> Option<Destination> {
        match &self.screen {
            Screen::Home => self.state.target().and_then(|(space, _)| {
                let project = self.state.space(space)?.main_chat_project()?;
                Some(Destination::Lead(space.to_owned(), project.to_owned()))
            }),
            Screen::Project {
                space,
                project,
                tab: Tab::Tasks | Tab::Conversation,
            } if !self.state.is_archived(space, project) => {
                Some(Destination::Lead(space.clone(), project.clone()))
            }
            Screen::Task { space, task }
                if self.state.task(space, *task).is_some()
                    && !self.state.task_is_archived(space, *task) =>
            {
                Some(Destination::Task(space.clone(), *task))
            }
            _ => None,
        }
    }

    fn draft_input(&self, draft: Draft) -> &Entity<TextareaState> {
        match draft {
            Draft::Lead => &self.composer,
            Draft::Task => &self.task_composer,
        }
    }

    pub(crate) fn sync_drafts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let destination = self.composer_destination();
        let switched = self.composer_shown != destination;
        for draft in [Draft::Lead, Draft::Task] {
            let input = self.draft_input(draft);
            let text = destination
                .as_ref()
                .filter(|target| target.draft() == draft)
                .map_or("", |destination| self.state.draft(destination));
            if switched || input.read(cx).value().as_str() != text {
                input.update(cx, |input, cx| {
                    input.set_value(text.to_owned(), window, cx);
                    input.set_selected_range(text.len()..text.len(), cx);
                });
            }
        }
        self.composer_shown = destination;
    }

    pub(crate) fn render_composer(
        &self,
        draft: Draft,
        label: impl Into<SharedString>,
        picker: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> Div {
        let destination = self.composer_destination();
        let held = destination
            .as_ref()
            .and_then(|destination| self.state.held_seconds(destination));
        let sending = held.is_none()
            && destination
                .as_ref()
                .is_some_and(|destination| self.state.is_sending(destination));
        let enabled = destination
            .as_ref()
            .is_some_and(|destination| self.state.is_send_enabled(destination));
        v_flex()
            .gap(XS)
            .child(
                composer_surface(cx)
                    .children(picker)
                    .child(
                        div().flex_1().min_w_0().child(
                            Textarea::new(self.draft_input(draft))
                                .rounded(FIELD_RADIUS)
                                .text_size(TYPE_BODY)
                                .aria_label(label),
                        ),
                    )
                    .child(
                        button("send")
                            .primary()
                            .tab_stop(true)
                            .icon(AppIcon::Send)
                            .label(if sending { "Sending…" } else { "Send" })
                            .loading(sending)
                            .disabled(!enabled)
                            .tooltip(crate::keyboard::send_hint(cx))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.submit_draft(window, cx)),
                            ),
                    ),
            )
            .children(held.map(|seconds| {
                h_flex()
                    .gap(SM)
                    .child(meta(format!("Sending in {seconds}…"), cx))
                    .child(
                        button("undo-send")
                            .small()
                            .ghost()
                            .tab_stop(true)
                            .label("Undo")
                            .tooltip(crate::menus::shortcut_hint(cx, &Undo).map_or_else(
                                || "Undo send".to_owned(),
                                |key| format!("Undo send  {key}"),
                            ))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.undo_draft(window, cx);
                            })),
                    )
            }))
            .children(
                destination
                    .as_ref()
                    .and_then(|destination| self.state.send_error(destination))
                    .map(|why| {
                        div()
                            .text_size(TYPE_BODY)
                            .text_color(cx.theme().danger)
                            .child(why.to_owned())
                    }),
            )
    }

    fn submit_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(destination) = self.composer_destination() else {
            return;
        };
        let request = self.state.request_send(destination, SystemClock.now());
        if request.held.is_some() || request.immediate.is_some() || request.flushed.is_some() {
            self.jump_to_latest(cx);
        }
        for outgoing in request.flushed.into_iter().chain(request.immediate) {
            self.deliver_draft(outgoing, cx);
        }
        if let Some(id) = request.held {
            let executor = cx.background_executor().clone();
            cx.spawn_in(window, async move |this, cx| {
                for _ in 0..threadlane_desktop::SEND_HOLD_SECONDS {
                    executor.timer(Duration::from_secs(1)).await;
                    let _gone = this.update(cx, |this, cx| {
                        if let Some(outgoing) = this.state.tick_send(id) {
                            this.deliver_draft(outgoing, cx);
                        }
                        cx.notify();
                    });
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn deliver_draft(&mut self, outgoing: Outgoing, cx: &mut Context<Self>) {
        self.state.sending(&outgoing);
        let core = Arc::clone(&self.core);
        self.run(
            async move {
                let result = outgoing.deliver(&*core.read().await).await;
                (outgoing, result)
            },
            |this, (outgoing, result), cx| {
                let task = matches!(outgoing.destination, Destination::Task(..));
                let sent = result.is_ok();
                this.state.finish_send(outgoing, result);
                if sent && task {
                    this.refresh(cx);
                } else if sent {
                    this.load_screen(cx);
                }
            },
            cx,
        );
    }

    pub(crate) fn undo_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(destination) = self.composer_destination() else {
            return false;
        };
        if !self.state.undo_send(&destination) {
            return false;
        }
        self.sync_drafts(window, cx);
        self.draft_input(destination.draft())
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
        true
    }

    pub(crate) fn reply_to(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(destination) = self.composer_destination() else {
            return;
        };
        self.state.reply_to_message(&destination, text);
        self.sync_drafts(window, cx);
        self.draft_input(destination.draft())
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(crate) fn set_hold_messages(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Err(why) = self.state.set_hold_messages(on) {
            self.state.report(Activity::Failed(why));
        }
        cx.notify();
    }
}
