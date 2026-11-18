//! An agent's edit view, opened from its card on the Agents screen: name,
//! role, backends, a model per backend, and the instructions with a
//! Markdown preview; beside it, where the agent is bound. "Delete agent…"
//! asks for the typed name, as project deletion does.

use std::sync::Arc;

use gpui_kit::component::button::{ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{InputState, Textarea};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::*;
use threadlane_core::model::BackendKind;
use threadlane_desktop::{Activity, AgentFields, BACKENDS, Tab, describe};

use super::link;
use super::project::mode_name;
use crate::components::{button, card, field, meta, section};
use crate::icons::AppIcon;
use crate::tokens::{FIELD_RADIUS, FORM_WIDTH, MD, ROW_RADIUS, SM, TYPE_BODY};
use crate::{Desktop, Screen};

/// The backends picker the new-agent form and the edit view share.
pub(super) fn backend_picker(
    id: &'static str,
    picked: usize,
    on_pick: impl Fn(&mut Desktop, usize) + 'static,
    cx: &mut Context<Desktop>,
) -> ButtonGroup {
    ButtonGroup::new(id)
        .outline()
        .compact()
        .children(BACKENDS.iter().enumerate().map(|(index, (label, _))| {
            button(SharedString::from(format!("{id}-{index}")))
                .label(*label)
                .selected(picked == index)
        }))
        .on_click(cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
            if let Some(index) = clicked.first() {
                on_pick(this, *index);
                cx.notify();
            }
        }))
}

impl Desktop {
    /// Opens `agent`'s edit view and fills its fields.
    pub(super) fn open_agent_edit(
        &mut self,
        space: &str,
        agent: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(fields) = self.state.open_agent_edit(space, agent) else {
            return;
        };
        for (input, value) in [
            (&self.agent_edit_name, fields.name),
            (&self.agent_edit_role, fields.role),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        for (input, (_, value)) in self.agent_edit_models.iter().zip(fields.models) {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.agent_edit_instructions.update(cx, |input, cx| {
            input.set_value(fields.instructions, window, cx);
        });
        cx.notify();
    }

    fn agent_fields(&self, cx: &App) -> AgentFields {
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        AgentFields {
            name: text(&self.agent_edit_name),
            role: text(&self.agent_edit_role),
            instructions: self.agent_edit_instructions.read(cx).value().to_string(),
            models: BackendKind::ALL
                .iter()
                .copied()
                .zip(self.agent_edit_models.iter().map(text))
                .collect(),
        }
    }

    /// Sends what changed in the edit view to the hub.
    fn save_agent(&mut self, cx: &mut Context<Self>) {
        let request = match self.state.agent_update(&self.agent_fields(cx)) {
            Err(why) => {
                self.state.report(Activity::Failed(why));
                cx.notify();
                return;
            }
            Ok(None) => {
                self.state.close_agent_edit();
                cx.notify();
                return;
            }
            Ok(Some(request)) => request,
        };
        let Some(edit) = self.state.agent_edit().cloned() else {
            return;
        };
        let core = Arc::clone(&self.core);
        self.state
            .report(Activity::Working(format!("Saving {}…", edit.agent)));
        self.run(
            async move {
                let saved = core
                    .read()
                    .await
                    .update_agent(Some(&edit.space), request)
                    .await;
                (edit, saved)
            },
            |this, (edit, saved), cx| match saved {
                Ok(agent) => {
                    this.state.report(Activity::Done(format!(
                        "Saved {}. It applies from its next run.",
                        agent.name
                    )));
                    this.state.agent_updated(&edit, agent);
                    this.refresh(cx);
                }
                Err(error) => this.state.report(Activity::Failed(describe(&error))),
            },
            cx,
        );
        cx.notify();
    }

    /// Asks for the agent's exact name before deleting it.
    fn confirm_agent_deletion(
        &mut self,
        space: String,
        agent: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) || !self.state.offers_agent_delete(&space, &agent) {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Exact agent name"));
        cx.observe_in(&input, window, |_, _, window, _| window.refresh())
            .detach();
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let typed = input.read(cx).value();
            let enabled = typed.as_str() == agent;
            let (view, space, agent) = (view.clone(), space.clone(), agent.clone());
            let confirmation = input.clone();
            dialog
                .title(format!("Delete {agent}?"))
                .overlay_closable(false)
                .child(
                    v_flex()
                        .gap(MD)
                        .child(format!(
                            "This removes {agent}, its bindings and its standing rules from \
                             {space}. Its finished tasks keep its name."
                        ))
                        .child(meta(format!("Type {agent:?} to confirm."), cx))
                        .child(field(&input)),
                )
                .footer(
                    h_flex()
                        .justify_end()
                        .gap(SM)
                        .child(
                            button("cancel-agent-deletion")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            button("confirm-agent-deletion")
                                .danger()
                                .label("Delete agent")
                                .disabled(!enabled)
                                .on_click(move |event, window, cx| {
                                    if !enabled
                                        || matches!(event, ClickEvent::Keyboard(key) if matches!(key.button, KeyboardButton::Enter))
                                    {
                                        return;
                                    }
                                    let typed = confirmation.read(cx).value();
                                    if typed.as_str() != agent {
                                        return;
                                    }
                                    window.close_dialog(cx);
                                    let _gone = view.update(cx, |this, cx| {
                                        this.delete_agent(space.clone(), agent.clone(), cx);
                                    });
                                }),
                        ),
                )
        });
    }

    fn delete_agent(&mut self, space: String, agent: String, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.state
            .report(Activity::Working(format!("Deleting {agent}…")));
        self.run(
            async move {
                let result = core
                    .read()
                    .await
                    .delete_agent(Some(&space), &agent, &agent)
                    .await;
                (space, result)
            },
            |this, (space, result), cx| {
                match result {
                    Ok(agent) => this.state.agent_deleted(&space, &agent),
                    Err(error) => this.state.report(Activity::Failed(describe(&error))),
                }
                this.refresh(cx);
            },
            cx,
        );
        cx.notify();
    }

    /// The open edit view, with the agent's bindings beside it.
    pub(super) fn agent_edit_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        let edit = self.state.agent_edit()?.clone();
        let muted = cx.theme().muted_foreground;
        let label = |text: &'static str| div().text_size(TYPE_BODY).text_color(muted).child(text);
        let backends = backend_picker(
            "agent-edit-backends",
            edit.backends,
            |this, index| {
                if let Some(edit) = this.state.agent_edit_mut() {
                    edit.backends = index;
                }
            },
            cx,
        );
        let mut models = v_flex().gap(SM);
        for backend in edit.model_backends() {
            let Some(index) = BackendKind::ALL.iter().position(|known| *known == backend) else {
                continue;
            };
            models = models.child(
                h_flex()
                    .gap(SM)
                    .items_center()
                    .child(
                        div()
                            .w(px(64.))
                            .text_size(TYPE_BODY)
                            .child(backend.to_string()),
                    )
                    .child(div().flex_1().child(field(&self.agent_edit_models[index]))),
            );
        }
        let preview = edit.preview;
        let mode = ButtonGroup::new("agent-edit-instructions-mode")
            .outline()
            .compact()
            .child(button("agent-edit-write").label("Write").selected(!preview))
            .child(
                button("agent-edit-preview")
                    .label("Preview")
                    .selected(preview),
            )
            .on_click(cx.listener(|this, clicked: &Vec<usize>, _, cx| {
                if let Some(edit) = this.state.agent_edit_mut() {
                    edit.preview = clicked.first() == Some(&1);
                    cx.notify();
                }
            }));
        let instructions = if preview {
            let text = self.agent_edit_instructions.read(cx).value().to_string();
            div()
                .p(SM)
                .min_h(px(240.))
                .text_size(TYPE_BODY)
                .bg(cx.theme().muted)
                .rounded(ROW_RADIUS)
                .whitespace_normal()
                .child(self.prose(
                    format!("agent-edit-preview-{}-{}", edit.space, edit.agent),
                    &edit.space,
                    text,
                    cx,
                ))
                .into_any_element()
        } else {
            Textarea::new(&self.agent_edit_instructions)
                .rounded(FIELD_RADIUS)
                .into_any_element()
        };

        let mut actions = h_flex()
            .gap(SM)
            .child(
                button("agent-edit-save")
                    .primary()
                    .icon(AppIcon::Check)
                    .label("Save")
                    .on_click(cx.listener(|this, _, _, cx| this.save_agent(cx))),
            )
            .child(
                button("agent-edit-cancel")
                    .icon(AppIcon::X)
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.close_agent_edit();
                        cx.notify();
                    })),
            );
        if self.state.offers_agent_delete(&edit.space, &edit.agent) {
            let (space, agent) = (edit.space.clone(), edit.agent.clone());
            actions = actions.child(div().flex_1()).child(
                button("agent-edit-delete")
                    .danger()
                    .icon(AppIcon::Trash)
                    .label("Delete agent…")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_agent_deletion(space.clone(), agent.clone(), window, cx);
                    })),
            );
        }
        let editor = card(cx)
            .flex_1()
            .max_w(FORM_WIDTH)
            .child(section(format!("Edit {} in {}", edit.agent, edit.space)))
            .child(field(&self.agent_edit_name))
            .child(field(&self.agent_edit_role))
            .child(label("Backends, in order of preference"))
            .child(backends)
            .child(label("Model for each backend"))
            .child(models)
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(label("Instructions, in Markdown"))
                    .child(mode),
            )
            .child(instructions)
            .child(meta(
                "Changes apply from its next run; a run already going keeps what it started with.",
                cx,
            ))
            .child(actions);

        let mut bound = v_flex().gap(SM).child(section("Bound in"));
        let bindings = self.state.agent_bindings(&edit.space, &edit.agent);
        if bindings.is_empty() {
            bound = bound.child(meta("No project yet: pick its mode in the grid below.", cx));
        }
        for row in bindings {
            let lead = if row.binding.is_lead { ", lead" } else { "" };
            let text = format!("{}: {}{lead}", row.project, mode_name(row.binding.mode));
            let (space, project) = (edit.space.clone(), row.project.clone());
            bound = bound.child(
                link(format!("agent-edit-binding-{}", row.project), text).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.go(
                            Screen::Project {
                                space: space.clone(),
                                project: project.clone(),
                                tab: Tab::Agents,
                            },
                            cx,
                        );
                    },
                )),
            );
        }
        Some(
            h_flex()
                .gap(MD)
                .items_start()
                .flex_wrap()
                .child(editor)
                .child(card(cx).w(px(240.)).child(bound)),
        )
    }
}
