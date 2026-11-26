//! Shared composer and the adapter between GPUI inputs and draft state.

use std::{rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{
    Enter, Escape, IndentInline, InputEvent, MoveDown, MoveUp, Textarea, TextareaState, Undo,
};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use threadlane_core::memory::parse_skill_mentions;
use threadlane_core::time::{Clock as _, SystemClock};
use threadlane_desktop::{
    Activity, AppState, Destination, Draft, HeldPaste, MentionItem, MentionKind, MentionQuery,
    Outgoing, complete_mention, load_modes, mention_at, remove_skill_mention, sends_on_enter,
    step_selection, toggle_skill_mention,
};

use super::{button, chat_images, icon_button, meta, status_dot};
use crate::icons::AppIcon;
use crate::tokens::{COMPOSER_MAX_ROWS, SM, Status, TYPE_BODY, TYPE_META, XS, XXS};
use crate::{Desktop, Screen, Tab};

/// The open `@`/`#` popover of one composer: the token being typed and the
/// highlighted row. Esc closes it until the token starts somewhere else.
#[derive(Debug, Default)]
pub(crate) struct MentionPopover {
    open: Option<(Draft, MentionQuery)>,
    selected: usize,
    dismissed: Option<(Draft, usize)>,
}

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

/// Compact auto-growing input for the docked task composer.
pub(crate) fn task_input(
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TextareaState> {
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder(placeholder)
            .auto_grow(1, 6)
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
                    this.track_mention(draft, &destination, input, cx);
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
        if self.screen == Screen::Home && !self.inbox.composing() {
            return None;
        }
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
            .max_w_full()
            .min_w_0()
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

    /// Opens, narrows or closes the mention popover after an edit.
    fn track_mention(
        &mut self,
        draft: Draft,
        destination: &Destination,
        input: &Entity<TextareaState>,
        cx: &mut Context<Self>,
    ) {
        let (text, cursor) = {
            let input = input.read(cx);
            (input.value().to_string(), input.cursor())
        };
        let mention = mention_at(&text, cursor)
            .filter(|mention| self.mention.dismissed != Some((draft, mention.range.start)));
        let was_open = self.mention.open.is_some();
        if mention.as_ref().map(|m| &m.query)
            != self.mention.open.as_ref().map(|(_, open)| &open.query)
        {
            self.mention.selected = 0;
        }
        if mention.is_none() {
            self.mention.dismissed = None;
        }
        let opened_for_agents = !was_open
            && mention
                .as_ref()
                .is_some_and(|m| m.kind == MentionKind::Agent);
        self.mention.open = mention.map(|mention| (draft, mention));
        // Home and the task screen don't load bindings; fetch the project's on open.
        if opened_for_agents && let Some((space, project)) = self.state.mention_project(destination)
        {
            let core = Arc::clone(&self.core);
            self.run(
                async move { load_modes(&*core.read().await, vec![(space, project)]).await },
                |this, loaded, _| this.state.apply_modes(loaded),
                cx,
            );
        }
    }

    /// The rows the popover shows for `draft` now, if it is open there.
    fn mention_rows(&self, draft: Draft) -> Option<(MentionQuery, Vec<MentionItem>)> {
        let (open, mention) = self.mention.open.as_ref()?;
        let destination = self.composer_destination()?;
        (*open == draft && destination.draft() == draft).then(|| {
            (
                mention.clone(),
                self.state.mention_items(&destination, mention),
            )
        })
    }

    /// Puts the picked row into the draft in place of the token.
    fn pick_mention(
        &mut self,
        draft: Draft,
        item: &MentionItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, mention)) = self.mention.open.take() else {
            return;
        };
        let input = self.draft_input(draft).clone();
        let (text, cursor) = complete_mention(&input.read(cx).value(), &mention, &item.insert());
        input.update(cx, |input, cx| {
            input.set_value(text.clone(), window, cx);
            input.set_selected_range(cursor..cursor, cx);
        });
        if let Some(destination) = self.composer_destination() {
            self.state.edit_draft(&destination, &text);
            if let MentionItem::Agent(agent) = item {
                self.state
                    .pick_agent(&destination, &agent.name, mention.range.start);
            }
        }
        cx.notify();
    }

    /// Arrow, Enter, Tab and Esc while the popover is open; anything else,
    /// or a closed popover, goes on to the field.
    fn mention_key(
        &mut self,
        draft: Draft,
        key: MentionKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((mention, rows)) = self.mention_rows(draft) else {
            return false;
        };
        if rows.is_empty() && key != MentionKey::Close {
            return false;
        }
        match key {
            MentionKey::Up | MentionKey::Down => {
                self.mention.selected =
                    step_selection(self.mention.selected, rows.len(), key == MentionKey::Down);
            }
            MentionKey::Pick => {
                let item = rows[self.mention.selected.min(rows.len() - 1)].clone();
                self.pick_mention(draft, &item, window, cx);
            }
            MentionKey::Close => {
                self.mention.open = None;
                self.mention.dismissed = Some((draft, mention.range.start));
            }
        }
        cx.notify();
        true
    }

    fn mention_popover(&self, draft: Draft, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (mention, rows) = self.mention_rows(draft)?;
        let empty = match mention.kind {
            MentionKind::Agent => "No enabled agent matches",
            MentionKind::Task => "No task matches",
        };
        let heading = match mention.kind {
            MentionKind::Agent => "Dispatch an agent",
            MentionKind::Task => "Reference a task",
        };
        let selected = self.mention.selected.min(rows.len().saturating_sub(1));
        let list = v_flex()
            .id("mention-popover")
            .absolute()
            .bottom_full()
            .left_0()
            .mb(XS)
            .w(rems(20.))
            .max_w_full()
            .p(XS)
            .gap(XXS)
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .shadow_md()
            .child(
                div()
                    .px(SM)
                    .py(XS)
                    .text_size(TYPE_META)
                    .text_color(cx.theme().muted_foreground)
                    .child(heading),
            )
            .children(
                rows.is_empty()
                    .then(|| div().px(SM).py(XS).child(meta(empty, cx))),
            )
            .children(rows.into_iter().enumerate().map(|(index, item)| {
                let row = h_flex()
                    .id(("mention", index))
                    .gap(SM)
                    .px(SM)
                    .py(XS)
                    .rounded(cx.theme().radius)
                    .min_w_0()
                    .when(index == selected, |row| row.bg(cx.theme().muted))
                    .hover(|row| row.bg(cx.theme().muted));
                let row = match &item {
                    MentionItem::Agent(agent) => row
                        .child(status_dot(
                            if agent.busy {
                                Status::Running
                            } else {
                                Status::Done
                            },
                            cx,
                        ))
                        .child(div().flex_none().child(agent.name.clone()))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(cx.theme().muted_foreground)
                                .child(if agent.busy {
                                    format!("{} · busy", agent.role)
                                } else {
                                    agent.role.clone()
                                }),
                        ),
                    MentionItem::Task(id, title) => row
                        .child(
                            div()
                                .flex_none()
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(id.to_string()),
                        )
                        .child(div().min_w_0().truncate().child(title.clone())),
                };
                row.on_click(cx.listener(move |this, _, window, cx| {
                    this.pick_mention(draft, &item, window, cx);
                    this.draft_input(draft).focus_handle(cx).focus(window, cx);
                }))
            }));
        Some(deferred(list).into_any_element())
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
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        self.render_composer_inner(draft, label, picker, false, window, cx)
    }

    /// Compact, cleanly bordered composer docked to the task detail main column.
    pub(crate) fn render_task_composer(
        &self,
        label: impl Into<SharedString>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        self.render_composer_inner(Draft::Task, label, None, true, window, cx)
    }

    fn render_composer_inner(
        &self,
        draft: Draft,
        label: impl Into<SharedString>,
        picker: Option<AnyElement>,
        compact: bool,
        window: &Window,
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
        let skill_target = destination.clone().filter(|target| match target {
            Destination::Lead(space, project) => !self.state.is_archived(space, project),
            Destination::Task(space, task) => !self.state.task_is_archived(space, *task),
        });
        let active_space = skill_target.as_ref().map(|target| match target {
            Destination::Lead(space, _) | Destination::Task(space, _) => space.clone(),
        });
        let draft_text = self.draft_input(draft).read(cx).value();
        let mentioned_skills = parse_skill_mentions(&draft_text);
        let active_skill_chips = skill_target
            .as_ref()
            .filter(|_| !mentioned_skills.is_empty())
            .map(|target| {
                let space_skills = active_space
                    .as_deref()
                    .map(|s| self.state.space_skills(s))
                    .unwrap_or_default();
                h_flex()
                    .gap(XS)
                    .flex_wrap()
                    .children(mentioned_skills.iter().map(|slug| {
                        let target = target.clone();
                        let remove_slug = slug.clone();
                        let title = space_skills
                            .iter()
                            .find(|skill| &skill.slug == slug)
                            .map_or_else(|| format!("/skill:{slug}"), |skill| skill.title.clone());
                        button(SharedString::from(format!("composer-active-skill-{slug}")))
                            .xsmall()
                            .outline()
                            .icon(AppIcon::X)
                            .label(title)
                            .tooltip(format!("Remove /skill:{slug} from message"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let current = this.state.draft(&target).to_owned();
                                let next = remove_skill_mention(&current, &remove_slug);
                                this.state.edit_draft(&target, &next);
                                this.sync_drafts(window, cx);
                                cx.notify();
                            }))
                    }))
            });
        let skill_picker = skill_target.as_ref().and_then(|target| {
            let space = active_space.as_deref()?;
            let available = self.state.space_skills(space);
            if available.is_empty() {
                return None;
            }
            let target = target.clone();
            let view = cx.entity().downgrade();
            let items: Vec<(String, String, bool)> = available
                .iter()
                .map(|skill| {
                    let active = mentioned_skills.iter().any(|s| s == &skill.slug);
                    (skill.slug.clone(), skill.title.clone(), active)
                })
                .collect();
            Some(
                button("composer-skill-picker")
                    .ghost()
                    .small()
                    .tab_stop(true)
                    .icon(AppIcon::BookOpen)
                    .label("Skill")
                    .dropdown_caret(true)
                    .tooltip("Attach a space skill playbook (/skill:<slug>)")
                    .dropdown_menu(move |mut menu, _, _| {
                        for (slug, title, active) in &items {
                            let (slug, target, view) = (slug.clone(), target.clone(), view.clone());
                            menu = menu.item(
                                PopupMenuItem::new(format!("{title}  (/skill:{slug})"))
                                    .checked(*active)
                                    .on_click(move |_, window, cx| {
                                        let _gone = view.update(cx, |this, cx| {
                                            let current = this.state.draft(&target).to_owned();
                                            let next = toggle_skill_mention(&current, &slug);
                                            this.state.edit_draft(&target, &next);
                                            this.sync_drafts(window, cx);
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
        });
        let paste_to = attach_to.clone();
        let view = cx.weak_entity();
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
        let mention_hint = destination
            .as_ref()
            .filter(|target| matches!(target, Destination::Lead(..)))
            .and_then(|target| self.state.mention_hint(target));
        let focused = self.draft_input(draft).focus_handle(cx).is_focused(window);
        let container = v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .px_3()
            .when(compact, |el| el.py_2())
            .when(!compact, |el| el.py_3())
            .border_1()
            .border_color(if focused {
                cx.theme().ring
            } else {
                cx.theme().input
            })
            .bg(cx.theme().muted.opacity(0.35))
            .rounded(cx.theme().radius_lg);
        let textarea = Textarea::new(self.draft_input(draft))
            .appearance(false)
            .bordered(false)
            .text_size(TYPE_BODY);
        let send_button = button("send")
            .primary()
            .when(compact, |btn| btn.small())
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
            }));
        container
            .relative()
            .capture_action(cx.listener(move |this, _: &MoveUp, window, cx| {
                if this.mention_key(draft, MentionKey::Up, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, _: &MoveDown, window, cx| {
                if this.mention_key(draft, MentionKey::Down, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, enter: &Enter, window, cx| {
                if !enter.shift && this.mention_key(draft, MentionKey::Pick, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, _: &IndentInline, window, cx| {
                if this.mention_key(draft, MentionKey::Pick, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, _: &Escape, window, cx| {
                if this.mention_key(draft, MentionKey::Close, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .children(self.mention_popover(draft, cx))
            .children(images)
            .children(active_skill_chips)
            .child(
                textarea
                    .disabled(destination.is_none())
                    .aria_label(label)
                    .on_paste(move |item, window, cx| {
                        paste_to.as_ref().is_some_and(|target| {
                            view.update(cx, |this, cx| this.paste_images(target, item, window, cx))
                                .unwrap_or(false)
                        })
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .children(attach_to.map(|target| {
                        icon_button("attach-images", AppIcon::Plus, "Attach images…".to_owned())
                            .tab_stop(true)
                            .disabled(loading_images)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.choose_images(target.clone(), window, cx);
                            }))
                    }))
                    .children(skill_picker)
                    .children(
                        picker.map(|picker| div().min_w_0().max_w(relative(0.55)).child(picker)),
                    )
                    .child(div().flex_1())
                    .child(send_button),
            )
            .children(mention_hint.map(|hint| meta(hint, cx)))
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
                if this.visible_draft().is_some()
                    && this.composer_destination().as_ref() == Some(&target)
                {
                    this.draft_input(target.draft())
                        .focus_handle(cx)
                        .focus(window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Attaches pasted images, or the image file a pasted path names. Returns
    /// whether the paste was consumed, so its text is not inserted.
    fn paste_images(
        &mut self,
        target: &Destination,
        item: &ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let images: Vec<_> = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image),
                _ => None,
            })
            .collect();
        if images.is_empty() {
            return self.paste_image_path(target, item, window, cx);
        }
        // Clipboard images keep any text alongside them.
        self.state.loading_images(target);
        let images = match chat_images::copy_clipboard_images(&images) {
            Ok(images) => images,
            Err(error) => {
                self.state.attach_images(target, Err(error));
                cx.notify();
                return false;
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
        false
    }

    /// Holds back a paste naming an image file while a worker reads it, then
    /// attaches the image or pastes the held text where it would have gone.
    fn paste_image_path(
        &mut self,
        target: &Destination,
        item: &ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(text) = item.text() else {
            return false;
        };
        let Some(path) = chat_images::pasted_image_path(&text) else {
            return false;
        };
        let input = self.draft_input(target.draft()).read(cx);
        let paste = HeldPaste {
            text,
            draft: input.value().to_string(),
            selection: input.selected_range(),
        };
        self.state.loading_images(target);
        let target = target.clone();
        cx.notify();
        let read = cx.background_spawn(async move { chat_images::read_pasted_image(path) });
        cx.spawn_in(window, async move |this, cx| {
            let images = read.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(cursor) = this.state.finish_path_paste(&target, images, paste) {
                    this.sync_drafts(window, cx);
                    if this.composer_shown.as_ref() == Some(&target) {
                        this.draft_input(target.draft()).update(cx, |input, cx| {
                            input.set_selected_range(cursor..cursor, cx);
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
        true
    }

    pub(crate) fn submit_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        if !outgoing.dispatch.is_empty() {
            self.run(
                async move {
                    let (started, result) = outgoing.dispatch(&*core.read().await).await;
                    (outgoing, started, result)
                },
                |this, (outgoing, started, result), cx| {
                    this.state.finish_dispatch(outgoing, &started, result);
                    this.load_screen(cx);
                },
                cx,
            );
            return;
        }
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
        if self.screen == Screen::Home && !self.inbox.composing() {
            self.focus_composer(window, cx);
            return true;
        }
        self.draft_input(destination.draft())
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
        true
    }

    /// Adds a review reference to the end of the task draft and focuses it.
    pub(crate) fn quote_to_composer(
        &mut self,
        reference: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(destination) = self.composer_destination() else {
            return;
        };
        self.state.quote_into_draft(&destination, reference);
        self.sync_drafts(window, cx);
        self.draft_input(destination.draft())
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    /// Whether the task's draft can be sent now.
    pub(crate) fn task_draft_ready(&self, space: &str, task: threadlane_core::ids::TaskId) -> bool {
        self.state
            .is_send_enabled(&Destination::Task(space.to_owned(), task))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MentionKey {
    Up,
    Down,
    Pick,
    Close,
}
