//! Controlled Agent & Fusion settings; hosts own catalog discovery and persistence.
use gpui::{prelude::*, *};
use gpui_component::button::Button;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, Icon};
use std::rc::Rc;
use threadlane_protocol::{OrchestratorMode, ReasoningEffort};

/// Captured catalog entry. No credentials or provider services cross this seam.
#[derive(Clone)]
pub struct SettingsAgentModel {
    pub id: String,
    pub label: String,
    pub icon_path: Option<SharedString>,
}

/// Read-only presentation snapshot. `None` selections inherit the parent model/effort.
pub struct SettingsAgent {
    pub models: Vec<SettingsAgentModel>,
    pub model: Option<String>,
    pub model_label: String,
    pub effort: Option<ReasoningEffort>,
    /// `None` hides effort for models that do not support reasoning.
    pub efforts: Option<Vec<ReasoningEffort>>,
    pub mode: OrchestratorMode,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsAgentAction {
    Model(Option<String>),
    Effort(Option<ReasoningEffort>),
    Mode(OrchestratorMode),
}
type Callback = Rc<dyn Fn(SettingsAgentAction, &mut Window, &mut App)>;

fn picker(id: &'static str, title: &str, label: impl Into<SharedString>) -> Button {
    let label = label.into();
    Button::new(id)
        .debug_selector(move || id.into())
        .accessibility_label(format!("{title}: {label}"))
        .tooltip(format!("{title}: {label}"))
        .label(label)
        .dropdown_caret(true)
        .w_full()
}

fn field(
    id: &'static str,
    title: &'static str,
    description: &'static str,
    control: impl IntoElement,
    stacked: bool,
    cx: &App,
) -> Stateful<Div> {
    super::settings_group(cx)
        .id(id)
        .debug_selector(move || id.into())
        .flex_row()
        .items_center()
        .gap_4()
        .when(stacked, |row| row.flex_col().items_start())
        .child(
            // Auto basis: a zero basis (`flex_1`) can leave a wrapping
            // description at zero width, painting one glyph per line.
            div()
                .flex_auto()
                .min_w_0()
                .when(stacked, |label| label.flex_none().w_full())
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                ),
        )
        .child(
            div()
                .flex_none()
                .min_w_0()
                .w(rems(14.0))
                .when(stacked, |lane| lane.w_full())
                .child(control),
        )
}

/// Render a project-scoped settings page. Empty state has no active controls.
/// The callback requests a change; only the host commits and publishes its result.
pub fn settings_agent(
    settings: Option<&SettingsAgent>,
    on_action: impl Fn(SettingsAgentAction, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let Some(settings) = settings else {
        return div()
            .debug_selector(|| "settings-agent-empty".into())
            .mt_5()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Attach a project to configure Agent and Fusion modes.")
            .into_any_element();
    };
    let callback: Callback = Rc::new(on_action);
    let models = settings.models.clone();
    let selected = settings.model.clone();
    let model_callback = callback.clone();
    let model = picker(
        "fast-model-picker",
        "Fusion model",
        settings.model_label.clone(),
    )
    .dropdown_menu(move |menu, _, _| {
        let callback = model_callback.clone();
        let menu = menu
            .check_side(gpui_component::Side::Right)
            .scrollable(true)
            .item(
                PopupMenuItem::new("Same as parent")
                    .checked(selected.is_none())
                    .on_click(move |_, window, cx| {
                        callback(SettingsAgentAction::Model(None), window, cx)
                    }),
            );
        models.iter().cloned().fold(menu, |menu, model| {
            let callback = model_callback.clone();
            menu.item(
                PopupMenuItem::new(model.label)
                    .checked(selected.as_deref() == Some(model.id.as_str()))
                    .when_some(model.icon_path, |item, path| {
                        item.icon(Icon::default().path(path))
                    })
                    .on_click(move |_, window, cx| {
                        callback(
                            SettingsAgentAction::Model(Some(model.id.clone())),
                            window,
                            cx,
                        )
                    }),
            )
        })
    });
    let selected_mode = settings.mode;
    let effort_callback = callback.clone();
    let mode = picker("orchestrator-picker", "Session mode", settings.mode.label()).dropdown_menu(
        move |menu, _, _| {
            [OrchestratorMode::Normal, OrchestratorMode::Fusion]
                .into_iter()
                .fold(menu, |menu, mode| {
                    let callback = callback.clone();
                    menu.item(
                        PopupMenuItem::new(mode.label())
                            .checked(mode == selected_mode)
                            .on_click(move |_, window, cx| {
                                callback(SettingsAgentAction::Mode(mode), window, cx)
                            }),
                    )
                })
        },
    );
    let stacked = window.viewport_size().width < window.rem_size() * 52.0;
    div()
        .mt_5()
        .flex()
        .flex_col()
        .gap_4()
        .child(field(
            "fusion-model-field",
            "Fusion model",
            "Model used for delegated work in Fusion mode.",
            model,
            stacked,
            cx,
        ))
        .children(settings.efforts.as_ref().map(|efforts| {
            let options = efforts.clone();
            let selected = settings.effort;
            let control = picker(
                "fast-reasoning-picker",
                "Fusion reasoning effort",
                selected.map(ReasoningEffort::label).unwrap_or("Same as parent"),
            )
            .dropdown_menu(move |menu, _, _| {
                std::iter::once(None)
                    .chain(options.iter().copied().map(Some))
                    .fold(menu, |menu, effort| {
                        let callback = effort_callback.clone();
                        menu.item(
                            PopupMenuItem::new(effort.map(ReasoningEffort::label).unwrap_or("Same as parent"))
                                .checked(effort == selected)
                                .on_click(move |_, window, cx| {
                                    callback(SettingsAgentAction::Effort(effort), window, cx)
                                }),
                        )
                    })
            });
            field(
                "fusion-effort-field",
                "Fusion reasoning effort",
                "Reasoning effort for delegated work in Fusion mode.",
                control,
                stacked,
                cx,
            )
        }))
        .child(field(
            "fusion-mode-field",
            "Session mode",
            "Agent runs on the selected model; Fusion delegates work to the configured Fusion model. Also switchable from the composer Mode dropdown.",
            mode,
            stacked,
            cx,
        ))
        .when_some(settings.error.clone(), |page, error| {
            page.child(div().debug_selector(|| "settings-agent-error".into()).text_sm().text_color(cx.theme().danger).child(error))
        })
        .into_any_element()
}
