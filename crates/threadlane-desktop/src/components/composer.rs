//! Shared composer and the adapter between GPUI inputs and draft state.

use std::{rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState, Undo};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex};
use gpui_kit::*;
use threadlane_core::time::{Clock as _, SystemClock};
use threadlane_desktop::{Activity, AppState, Destination, Draft, Outgoing, sends_on_enter};

use super::{button, chat_images, composer_surface, icon_button, meta};
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
            .auto_grow(2, COMPOSER_MAX_ROWS)
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
            Screen::Home => self
                .state
                .target()
                .map(|(space, project)| Destination::Lead(space.to_owned(), project.to_owned())),
            Screen::Project {
                space,
                project,
                tab: Tab::Conversation,
            } if !self.state.is_archived(space, project) => {
                let (space, project) = self.state.chat_target(space, project);
                Some(Destination::Lead(space.to_owned(), project.to_owned()))
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

    /// The composer's "To:" picker over every lead, showing `current`.
    /// `choose` records a pick; the draft being typed follows it.
    pub(crate) fn to_picker(
        &self,
        current: (&str, &str),
        choose: impl Fn(&mut AppState, &str, &str) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = self.state.to_label(current.0, current.1);
        let lead = label.trim_start_matches("To: ");
        let accessibility = format!("Send to {lead} — choose who receives this message");
        let items: Vec<(String, String, String, bool)> = self
            .state
            .lead_targets()
            .into_iter()
            .map(|(space, project)| {
                let name = format!("{space} · {}", self.state.lead_name(space, project));
                (
                    name,
                    space.to_owned(),
                    project.to_owned(),
                    (space, project) == current,
                )
            })
            .collect();
        let locked = self
            .composer_destination()
            .is_some_and(|from| !self.state.can_retarget(&from));
        let accessibility = if locked {
            format!("Sending to {lead} — wait for the send to finish to choose another lead")
        } else {
            accessibility
        };
        let choose = Rc::new(choose);
        let view = cx.entity().downgrade();
        button("target")
            .ghost()
            .small()
            .label(label)
            .dropdown_caret(true)
            .tab_stop(true)
            .disabled(locked)
            .accessibility_label(accessibility.clone())
            .tooltip(accessibility)
            .dropdown_menu(move |mut menu, _, _| {
                for (name, space, project, checked) in &items {
                    let (space, project) = (space.clone(), project.clone());
                    let (view, choose) = (view.clone(), Rc::clone(&choose));
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(*checked).on_click(
                        move |_, _, cx| {
                            let _gone = view.update(cx, |this, cx| {
                                let from = this.composer_destination();
                                if from
                                    .as_ref()
                                    .is_some_and(|from| !this.state.can_retarget(from))
                                {
                                    return;
                                }
                                choose(&mut this.state, &space, &project);
                                if let (Some(from), Some(to)) = (from, this.composer_destination())
                                {
                                    this.state.move_draft(&from, &to);
                                }
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            })
            .into_any_element()
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
        let loading_images = destination
            .as_ref()
            .is_some_and(|target| self.state.is_loading_images(target));
        let attach_to = destination.clone().filter(|target| {
            matches!(target, Destination::Lead(space, project) if !self.state.is_archived(space, project))
        });
        let paste_to = attach_to.clone();
        let view = cx.weak_entity();
        let surface = cx.theme().muted;
        let images = attach_to.as_ref().and_then(|target| {
            let images = self.state.draft_images(target);
            (!images.is_empty()).then(|| {
                h_flex()
                    .h_auto()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .overflow_x_scrollbar()
                    .id("composer-attachments")
                    .gap(XS)
                    .children(images.iter().map(|image| {
                        use std::hash::{BuildHasher as _, RandomState};
                        use std::sync::LazyLock;
                        static HASHER: LazyLock<RandomState> = LazyLock::new(RandomState::new);
                        let target = target.clone();
                        let id = HASHER.hash_one(image) as usize;
                        let selected = image.clone();
                        h_flex()
                            .items_start()
                            .flex_none()
                            .child(chat_images::preview_button(
                                ("preview-image", id).into(),
                                image,
                                cx,
                            ))
                            .child(
                                icon_button(
                                    ("remove-image", id),
                                    AppIcon::X,
                                    format!("Remove {}", image.name()),
                                )
                                .tab_stop(true)
                                .outline()
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.state.remove_image(&target, &selected);
                                        cx.notify();
                                    },
                                )),
                            )
                    }))
            })
        });
        composer_surface(cx)
            .children(images)
            .child(
                Textarea::new(self.draft_input(draft))
                    .rounded(FIELD_RADIUS)
                    .bg(surface)
                    .border_color(surface)
                    .text_size(TYPE_BODY)
                    .disabled(destination.is_none())
                    .aria_label(label)
                    .on_paste(move |item, _, cx| {
                        if let Some(target) = &paste_to {
                            let _ = view.update(cx, |this, cx| {
                                this.paste_images(target, item, cx);
                            });
                        }
                        false
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .children(attach_to.map(|target| {
                        icon_button("attach-images", AppIcon::Plus, "Attach images…".to_owned())
                            .tab_stop(true)
                            .disabled(loading_images)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.choose_images(target.clone(), window, cx);
                            }))
                    }))
                    .children(
                        picker.map(|picker| div().min_w_0().max_w(relative(0.55)).child(picker)),
                    )
                    .child(div().flex_1())
                    .child(
                        button("send")
                            .primary()
                            .flex_none()
                            .tab_stop(true)
                            .icon(AppIcon::Send)
                            .label(if sending { "Sending…" } else { "Send" })
                            .loading(sending)
                            .disabled(!enabled)
                            .tooltip(crate::keyboard::send_hint(cx))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.submit_draft(window, cx);
                                this.draft_input(draft).focus_handle(cx).focus(window, cx);
                            })),
                    ),
            )
            .children(loading_images.then(|| meta("Preparing images…", cx)))
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
            .children(
                destination
                    .is_none()
                    .then(|| meta("Choose a conversation to send a message.", cx)),
            )
    }

    fn choose_images(&mut self, target: Destination, window: &mut Window, cx: &mut Context<Self>) {
        self.state.loading_images(&target);
        cx.notify();
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach images".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let images = match selection.await {
                Ok(Ok(Some(paths))) => {
                    cx.background_spawn(async move { chat_images::read_images(paths) })
                        .await
                }
                Ok(Ok(None)) | Err(_) => Ok(Vec::new()),
                Ok(Err(error)) => Err(format!("Could not open image files. {error}")),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.state.attach_images(&target, images);
                if this.composer_destination().as_ref() == Some(&target) {
                    this.draft_input(target.draft())
                        .focus_handle(cx)
                        .focus(window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn paste_images(&mut self, target: &Destination, item: &ClipboardItem, cx: &mut Context<Self>) {
        let images: Vec<_> = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image),
                _ => None,
            })
            .collect();
        if images.is_empty() {
            return;
        }
        self.state.loading_images(target);
        let images = match chat_images::copy_clipboard_images(&images) {
            Ok(images) => images,
            Err(error) => {
                self.state.attach_images(target, Err(error));
                cx.notify();
                return;
            }
        };
        let target = target.clone();
        cx.notify();
        let checked =
            cx.background_spawn(async move { chat_images::prepare_clipboard_images(images) });
        cx.spawn(async move |this, cx| {
            let images = checked.await;
            let _ = this.update(cx, |this, cx| {
                this.state.attach_images(&target, images);
                cx.notify();
            });
        })
        .detach();
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
