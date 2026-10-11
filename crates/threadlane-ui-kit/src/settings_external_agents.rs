//! Controlled external-agent settings. Hosts own discovery, inputs and configuration writes.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName};
use std::rc::Rc;

/// Presentation snapshot. `id` is the durable agent ID; scope participates in UI identity.
#[derive(Clone)]
pub struct SettingsExternalAgentRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub enabled: bool,
    pub global: bool,
    pub preset: bool,
    pub error: bool,
}
pub struct SettingsExternalAgents {
    pub rows: Vec<SettingsExternalAgentRow>,
    pub global: bool,
    pub has_project: bool,
    pub status: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsExternalAgentAction {
    Scope(bool),
    Refresh,
    Add {
        name: String,
        command: String,
    },
    Toggle {
        id: String,
        global: bool,
        preset: bool,
        enabled: bool,
    },
    Remove {
        id: String,
        global: bool,
    },
}
type Callback = Rc<dyn Fn(SettingsExternalAgentAction, &mut Window, &mut App)>;

/// Bundled presets (`threadlane-acp-engine::presets`) whose vendor has a vendored
/// mark get it; other and custom agents keep the generic ACP mark.
fn agent_icon(id: &str) -> &'static str {
    match id {
        "codex" => "icons/providers/openai.svg",
        "opencode2" => "icons/providers/opencode.svg",
        "antigravity" => "icons/providers/google.svg",
        _ => "icons/providers/acp.svg",
    }
}

pub fn settings_external_agent_row(
    row: &SettingsExternalAgentRow,
    has_project: bool,
    on_action: impl Fn(SettingsExternalAgentAction, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> Stateful<Div> {
    let callback: Callback = Rc::new(on_action);
    let scope = if row.global { "Global" } else { "Project" };
    let key = format!("{}-{}", scope.to_lowercase(), row.id);
    let disabled = !row.global && !has_project;
    let hint = if disabled {
        format!("Attach a project to manage {}", row.name)
    } else {
        format!(
            "{} {} ({scope})",
            if row.enabled { "Disable" } else { "Enable" },
            row.name
        )
    };
    let toggle = callback.clone();
    let id = row.id.clone();
    let global = row.global;
    let preset = row.preset;
    let selector: SharedString = format!("acp-toggle-{key}").into();
    let mut controls = div().flex().flex_none().items_center().gap_3().child(
        div()
            .debug_selector({
                let selector = selector.clone();
                move || selector.to_string()
            })
            .child(
                Switch::new(selector)
                    .checked(row.enabled)
                    .disabled(disabled)
                    .accessibility_label(hint.clone())
                    .tooltip(hint)
                    .on_click(move |enabled, window, cx| {
                        toggle(
                            SettingsExternalAgentAction::Toggle {
                                id: id.clone(),
                                global,
                                preset,
                                enabled: *enabled,
                            },
                            window,
                            cx,
                        )
                    }),
            ),
    );
    if !preset {
        let id = row.id.clone();
        let selector: SharedString = format!("acp-remove-{key}").into();
        let hint = if disabled {
            format!("Attach a project to remove {}", row.name)
        } else {
            format!("Remove {} ({scope})", row.name)
        };
        controls = controls.child(
            Button::new(selector.clone())
                .debug_selector(move || selector.to_string())
                .icon(IconName::Delete)
                .ghost()
                .w_8()
                .h_8()
                .disabled(disabled)
                .accessibility_label(hint.clone())
                .tooltip(hint)
                .on_click(move |_, window, cx| {
                    callback(
                        SettingsExternalAgentAction::Remove {
                            id: id.clone(),
                            global,
                        },
                        window,
                        cx,
                    )
                }),
        );
    }
    let stacked = window.viewport_size().width < window.rem_size() * 44.0;
    let selector: SharedString = format!("acp-row-{key}").into();
    super::settings_group(cx)
        .id(selector.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .items_center()
        .gap_3()
        .when(stacked, |row| row.flex_col().items_start())
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_start()
                .gap_3()
                .when(stacked, |row| row.flex_none().w_full())
                .child(
                    div()
                        .size_8()
                        .flex_none()
                        .rounded_md()
                        .bg(cx.theme().muted)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(Icon::default().path(agent_icon(&row.id))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(row.name.clone()),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(row.description.clone()),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(if row.error {
                                    cx.theme().danger
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(format!("{scope} · {}", row.status)),
                        ),
                ),
        )
        .child(controls.when(stacked, |controls| controls.w_full().justify_end()))
}

/// The same form and inventory run on native and web; callbacks report intent only.
pub fn settings_external_agents(
    agents: &SettingsExternalAgents,
    name: &Entity<InputState>,
    command: &Entity<InputState>,
    on_action: impl Fn(SettingsExternalAgentAction, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let callback: Callback = Rc::new(on_action);
    let scope = callback.clone();
    let refresh = callback.clone();
    let add = callback.clone();
    let name_input = name.clone();
    let command_input = command.clone();
    let available = agents.global || agents.has_project;
    div()
        .mt_5()
        .flex()
        .flex_col()
        .gap_3()
        .when_some(agents.status.clone(), |page, status| {
            page.child(
                div()
                    .id("acp-status")
                    .debug_selector(|| "acp-status".into())
                    .role(Role::Alert)
                    .text_sm()
                    .child(status),
            )
        })
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_3()
                .child(super::settings_scope_picker(
                    "acp-scope",
                    agents.global,
                    move |global, window, cx| {
                        scope(SettingsExternalAgentAction::Scope(global), window, cx)
                    },
                ))
                .child(
                    Button::new("acp-refresh")
                        .debug_selector(|| "acp-refresh".into())
                        .icon(IconName::Redo)
                        .label("Refresh")
                        .outline()
                        .on_click(move |_, window, cx| {
                            refresh(SettingsExternalAgentAction::Refresh, window, cx)
                        }),
                ),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child("Quick setup"),
        )
        .children(agents.rows.iter().filter(|row| row.preset).map(|row| {
            let callback = callback.clone();
            settings_external_agent_row(
                row,
                agents.has_project,
                move |action, window, cx| callback(action, window, cx),
                window,
                cx,
            )
        }))
        .child(
            super::settings_group(cx)
                .debug_selector(|| "acp-custom-form".into())
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Custom agent"),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_xs().child("Name"))
                        .child(Input::new(name).aria_label("Custom agent name")),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_xs().child("Command"))
                        .child(Input::new(command).aria_label("Custom agent command")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if available {
                            "Run a local command that supports ACP over stdio."
                        } else {
                            "Choose Global scope or attach a project to add an agent."
                        }),
                )
                .child(
                    div().flex().justify_end().child(
                        Button::new("acp-add")
                            .debug_selector(|| "acp-add".into())
                            .icon(IconName::Plus)
                            .label("Add agent")
                            .outline()
                            .disabled(!available)
                            .accessibility_label(if available {
                                "Add custom agent"
                            } else {
                                "Choose Global scope or attach a project to add an agent"
                            })
                            .on_click(move |_, window, cx| {
                                add(
                                    SettingsExternalAgentAction::Add {
                                        name: name_input.read(cx).value().to_string(),
                                        command: command_input.read(cx).value().to_string(),
                                    },
                                    window,
                                    cx,
                                )
                            }),
                    ),
                ),
        )
        .children(agents.rows.iter().filter(|row| !row.preset).map(|row| {
            let callback = callback.clone();
            settings_external_agent_row(
                row,
                agents.has_project,
                move |action, window, cx| callback(action, window, cx),
                window,
                cx,
            )
        }))
        .into_any_element()
}
