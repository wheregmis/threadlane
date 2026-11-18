//! An agent's edit view, opened from its card on the Agents screen: name,
//! role, backends, a model per backend, and the instructions with a
//! Markdown preview; beside it, where the agent is bound. "Delete agent…"
//! asks for the typed name, as project deletion does.

use std::sync::Arc;

use gpui_kit::component::button::{ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{InputState, Textarea};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
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

/// Backend order and per-backend models, shared by the create and edit forms.
pub(super) fn backend_picker(
    id: &'static str,
    picked: usize,
    saved_label: Option<String>,
    model_backends: &[BackendKind],
    inputs: &[Entity<InputState>],
    on_pick: fn(&mut Desktop, usize),
    cx: &mut Context<Desktop>,
) -> Div {
    let kinds = BACKENDS.get(picked).map_or(&[][..], |(_, kinds)| *kinds);
    let view = cx.entity().downgrade();
    let backend =
        |label: &'static str, current: SharedString, choices: Vec<(SharedString, usize)>| {
            let view = view.clone();
            h_flex().gap(SM).child(meta(label, cx)).child(
                button(SharedString::from(format!("{id}-{label}")))
                    .outline()
                    .label(current.clone())
                    .accessibility_label(format!("{label}, {current}"))
                    .dropdown_menu(move |mut menu, _, _| {
                        for (name, index) in &choices {
                            let (view, index) = (view.clone(), *index);
                            menu = menu.item(
                                PopupMenuItem::new(name.clone())
                                    .checked(*name == current)
                                    .on_click(move |_, _, cx| {
                                        let _ = view.update(cx, |this, cx| {
                                            on_pick(this, index);
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
        };
    let model = |kind: BackendKind| {
        let index = BackendKind::ALL
            .iter()
            .position(|known| *known == kind)
            .unwrap();
        let input = &inputs[index];
        let presets: &[&str] = match kind {
            BackendKind::Antigravity => &[],
            BackendKind::Claude => &["sonnet", "opus", "haiku"],
            BackendKind::Codex => &["gpt-6.2-sol", "gpt-6.2-astra", "gpt-6.1-terra"],
        };
        let target = input.clone();
        h_flex()
            .gap(SM)
            .child(meta(format!("{kind} model"), cx))
            .child(div().flex_1().child(field(input)))
            .child(
                button(SharedString::from(format!("{id}-{kind}-models")))
                    .outline()
                    .label("Choose")
                    .accessibility_label(format!("Choose {kind} model"))
                    .dropdown_menu(move |mut menu, _, cx| {
                        let selected = target.read(cx).value();
                        for value in std::iter::once("").chain(presets.iter().copied()) {
                            let target = target.clone();
                            menu = menu.item(
                                PopupMenuItem::new(if value.is_empty() {
                                    "Default model"
                                } else {
                                    value
                                })
                                .checked(selected.trim() == value)
                                .on_click(move |_, window, cx| {
                                    target
                                        .update(cx, |input, cx| input.set_value(value, window, cx))
                                }),
                            );
                        }
                        menu
                    }),
            )
    };
    let preferred = kinds.first();
    let mut choices: Vec<_> = BACKENDS
        .iter()
        .enumerate()
        .filter(|(_, (_, order))| order.is_empty() || order.len() == kinds.len().max(1))
        .map(|(index, (_, order))| {
            (
                order.first().map_or("Any backend", |k| k.as_str()).into(),
                index,
            )
        })
        .collect();
    let mut current = SharedString::from(preferred.map_or("Any backend", |k| k.as_str()));
    if let Some(label) = saved_label {
        let label = SharedString::from(label);
        if picked == BACKENDS.len() {
            current = label.clone();
        }
        choices.push((label, BACKENDS.len()));
    }
    let mut rows = v_flex()
        .gap(SM)
        .child(backend("Preferred backend", current, choices));
    if let Some(first) = preferred {
        let choices = BACKENDS
            .iter()
            .enumerate()
            .filter(|(_, (_, order))| order.first() == preferred)
            .map(|(index, (_, order))| (order.get(1).map_or("None", |k| k.as_str()).into(), index))
            .collect();
        rows = rows.child(model(*first)).child(backend(
            "Fallback backend",
            kinds.get(1).map_or("None", |k| k.as_str()).into(),
            choices,
        ));
        if let Some(second) = kinds.get(1) {
            rows = rows.child(model(*second));
        }
    } else {
        for kind in model_backends {
            rows = rows.child(model(*kind));
        }
    }
    rows
}

impl Desktop {
    pub(super) fn skill_picker(&self, editing: bool, cx: &mut Context<Self>) -> Div {
        let form = self.state.agent_form();
        let (picked, catalog) = if editing && let Some(edit) = self.state.agent_edit() {
            (&edit.skills, &edit.installed_skills)
        } else {
            (&form.skills, &form.installed_skills)
        };
        let installed = catalog.as_ref().and_then(|r| r.as_ref().ok());
        let mut choices = installed.cloned().unwrap_or_default();
        for skill in picked {
            if !choices.contains(skill) {
                choices.push(skill.clone());
            }
        }
        let note = match catalog {
            None => Some("Loading installed skills…".to_owned()),
            Some(Err(e)) => Some(format!("Could not list installed skills: {e}")),
            Some(Ok(list)) if list.is_empty() => {
                Some("No personal skills installed on this hub.".into())
            }
            _ => None,
        };
        let mut rows = v_flex().gap(SM);
        for kind in BackendKind::ALL {
            let view = cx.entity().downgrade();
            let choices: Vec<_> = choices
                .iter()
                .filter(|s| s.backend == *kind)
                .map(|s| {
                    let missing = installed.is_some_and(|list| !list.contains(s));
                    let label = if missing {
                        format!("{} (missing on hub)", s.name)
                    } else {
                        s.name.clone()
                    };
                    (s.clone(), label, picked.contains(s))
                })
                .collect();
            let count = picked.iter().filter(|s| s.backend == *kind).count();
            rows = rows.child(
                button(SharedString::from(format!(
                    "profile-skills-{editing}-{kind}"
                )))
                .outline()
                .label(format!("{kind} skills ({count})"))
                .disabled(choices.is_empty())
                .dropdown_menu(move |mut menu, _, _| {
                    for (skill, label, selected) in &choices {
                        let (skill, view) = (skill.clone(), view.clone());
                        menu = menu.item(
                            PopupMenuItem::new(label.clone())
                                .checked(*selected)
                                .on_click(move |_, _, cx| {
                                    let _ = view.update(cx, |this, cx| {
                                        let picked = if editing {
                                            let Some(edit) = this.state.agent_edit_mut() else {
                                                return;
                                            };
                                            &mut edit.skills
                                        } else {
                                            &mut this.state.agent_form_mut().skills
                                        };
                                        if picked.contains(&skill) {
                                            picked.retain(|s| s != &skill)
                                        } else {
                                            picked.push(skill.clone())
                                        }
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                    menu
                }),
            );
        }
        if let Some(note) = note {
            rows = rows.child(meta(note, cx));
        }
        rows
    }

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
        self.load_agent_skills(space.into(), Some(agent.into()), cx);
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
            edit.saved_backend_label(),
            &edit.model_backends(),
            &self.agent_edit_models,
            |this, index| {
                if let Some(edit) = this.state.agent_edit_mut() {
                    edit.backends = index;
                }
            },
            cx,
        );
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
            .child(backends)
            .child(self.skill_picker(true, cx))
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
                                tab: Tab::Setup,
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
