//! Skills and extensions share inventory presentation. Hosts perform discovery and IO.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::switch::Switch;
use gpui_component::tag::{Tag, TagVariant};
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsCatalogKind {
    Skills,
    Extensions,
}
impl SettingsCatalogKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Skills => "skill",
            Self::Extensions => "extension",
        }
    }
    fn icon(self) -> IconName {
        match self {
            Self::Skills => IconName::BookOpen,
            Self::Extensions => IconName::HardDrive,
        }
    }
}
#[derive(Clone, Copy)]
pub enum SettingsCatalogStatus {
    Enabled,
    Active,
    Disabled,
    Invalid,
    Overridden,
}
impl SettingsCatalogStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Enabled => "Enabled",
            Self::Active => "Active",
            Self::Disabled => "Disabled",
            Self::Invalid => "Invalid",
            Self::Overridden => "Overridden",
        }
    }
    /// Enabled/Disabled repeat the row's switch, so only states the switch
    /// cannot show (active, invalid, overridden) get a tag.
    fn shows_tag(self) -> bool {
        !matches!(self, Self::Enabled | Self::Disabled)
    }
    fn variant(self) -> TagVariant {
        match self {
            Self::Enabled | Self::Active => TagVariant::Success,
            Self::Disabled => TagVariant::Secondary,
            Self::Invalid => TagVariant::Danger,
            Self::Overridden => TagVariant::Warning,
        }
    }
}
/// Captured row; `id` must identify the object and scope, not its display name.
#[derive(Clone)]
pub struct SettingsCatalogRow {
    pub id: String,
    pub title: String,
    pub description: String,
    pub scope: String,
    pub status: SettingsCatalogStatus,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
}
/// Inventory and availability supplied by the host; never scans files in render.
pub struct SettingsCatalog {
    pub kind: SettingsCatalogKind,
    pub rows: Vec<SettingsCatalogRow>,
    pub has_project: bool,
    pub install_globally: bool,
    pub status: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsCatalogAction {
    Scope(bool),
    Refresh,
    Install,
    DisableAll,
    Toggle { id: String, enabled: bool },
    Remove { id: String },
}
type Callback = Rc<dyn Fn(SettingsCatalogAction, &mut Window, &mut App)>;

/// Selected installation scope. Shared with the ACP host's scope controls.
pub fn settings_scope_picker(
    prefix: &'static str,
    global: bool,
    on_scope: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> Div {
    let callback = Rc::new(on_scope);
    div()
        .flex()
        .gap_1()
        .children([false, true].into_iter().map(|is_global| {
            let callback = callback.clone();
            let id: SharedString =
                format!("{prefix}-{}", if is_global { "global" } else { "project" }).into();
            let selector = id.clone();
            let label = if is_global { "Global" } else { "Project" };
            Button::new(id)
                .debug_selector(move || selector.to_string())
                .icon(if is_global {
                    IconName::Globe
                } else {
                    IconName::Folder
                })
                .label(label)
                .ghost()
                .selected(global == is_global)
                .accessibility_label(format!(
                    "Install scope: {label}{}",
                    if global == is_global {
                        ", selected"
                    } else {
                        ""
                    }
                ))
                .on_click(move |_, window, cx| callback(is_global, window, cx))
        }))
}

/// Inventory row with a fixed icon slot and trailing controls. Narrow windows
/// move the controls below the description, retaining every action and label.
pub fn settings_catalog_row(
    kind: SettingsCatalogKind,
    row: &SettingsCatalogRow,
    on_action: impl Fn(SettingsCatalogAction, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> Stateful<Div> {
    let callback: Callback = Rc::new(on_action);
    let id = row.id.clone();
    let toggle_callback = callback.clone();
    let toggle_hint = format!(
        "{} {} {}",
        if row.enabled { "Disable" } else { "Enable" },
        kind.prefix(),
        row.title
    );
    let toggle_label = row.disabled_reason.as_ref().map_or_else(
        || toggle_hint.clone(),
        |reason| format!("{toggle_hint}: {reason}"),
    );
    let toggle_selector: SharedString = format!("{}-toggle-{}", kind.prefix(), row.id).into();
    let mut controls = div().flex().flex_none().items_center().gap_3().child(
        div()
            .flex_none()
            .debug_selector(move || toggle_selector.to_string())
            .child(
                Switch::new(SharedString::from(format!(
                    "{}-toggle-{}",
                    kind.prefix(),
                    row.id
                )))
                .checked(row.enabled)
                .disabled(row.disabled_reason.is_some())
                .accessibility_label(toggle_label.clone())
                .tooltip(toggle_label)
                .on_click(move |checked, window, cx| {
                    toggle_callback(
                        SettingsCatalogAction::Toggle {
                            id: id.clone(),
                            enabled: *checked,
                        },
                        window,
                        cx,
                    )
                }),
            ),
    );
    if kind == SettingsCatalogKind::Extensions {
        let id = row.id.clone();
        let selector: SharedString = format!("extension-remove-{id}").into();
        let hint = format!("Remove extension {}", row.title);
        controls = controls.child(
            Button::new(selector.clone())
                .debug_selector(move || selector.to_string())
                .icon(IconName::Delete)
                .ghost()
                .w_8()
                .h_8()
                .accessibility_label(hint.clone())
                .tooltip(hint)
                .on_click(move |_, window, cx| {
                    callback(SettingsCatalogAction::Remove { id: id.clone() }, window, cx)
                }),
        );
    }
    let stacked = window.viewport_size().width < window.rem_size() * 44.0;
    let selector: SharedString = format!("{}-row-{}", kind.prefix(), row.id).into();
    div()
        .id(selector.clone())
        .debug_selector(move || selector.to_string())
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().title_bar)
        .px_4()
        .py_3()
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
                        .child(Icon::new(kind.icon())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(row.title.clone()),
                        )
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .when(kind == SettingsCatalogKind::Extensions, |text| {
                                    text.truncate()
                                })
                                // Skill descriptions run to several sentences of model
                                // guidance; two lines identify the skill.
                                .when(kind == SettingsCatalogKind::Skills, |text| {
                                    text.line_clamp(2).text_ellipsis()
                                })
                                .child(row.description.clone()),
                        )
                        .child(
                            div()
                                .mt_1()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(
                                    Tag::new()
                                        .child(row.scope.clone())
                                        .with_variant(TagVariant::Secondary)
                                        .small(),
                                )
                                .children(row.status.shows_tag().then(|| {
                                    Tag::new()
                                        .child(row.status.label())
                                        .with_variant(row.status.variant())
                                        .small()
                                })),
                        ),
                ),
        )
        .child(controls.when(stacked, |controls| controls.w_full().justify_end()))
}

pub fn settings_catalog(
    catalog: &SettingsCatalog,
    on_action: impl Fn(SettingsCatalogAction, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let callback: Callback = Rc::new(on_action);
    let refresh = callback.clone();
    let kind = catalog.kind;
    let mut actions = div().flex().flex_wrap().gap_2().child(
        Button::new(if catalog.kind == SettingsCatalogKind::Skills {
            "skills-refresh"
        } else {
            "extension-refresh"
        })
        .debug_selector(move || {
            if kind == SettingsCatalogKind::Skills {
                "skills-refresh".into()
            } else {
                "extension-refresh".into()
            }
        })
        .icon(IconName::Redo)
        .label("Refresh")
        .outline()
        .on_click(move |_, window, cx| refresh(SettingsCatalogAction::Refresh, window, cx)),
    );
    let toolbar = if kind == SettingsCatalogKind::Skills {
        let disable = callback.clone();
        actions = actions.child(
            Button::new("skills-disable-all")
                .debug_selector(|| "skills-disable-all".into())
                .label("Disable all")
                .outline()
                .disabled(!catalog.has_project || !catalog.rows.iter().any(|row| row.enabled))
                .accessibility_label("Disable all project skills")
                .tooltip(if catalog.has_project {
                    "Disable all project skills"
                } else {
                    "Attach a project to manage skills"
                })
                .on_click(move |_, window, cx| {
                    disable(SettingsCatalogAction::DisableAll, window, cx)
                }),
        );
        div().flex().justify_end().child(actions)
    } else {
        let scope = callback.clone();
        let install = callback.clone();
        let hint = if !catalog.install_globally && !catalog.has_project {
            "Choose Global scope or attach a project to install an extension"
        } else {
            "Install a compiled WASI extension"
        };
        actions = actions.child(
            Button::new("extension-install")
                .debug_selector(|| "extension-install".into())
                .icon(IconName::Plus)
                .label("Install .wasm…")
                .outline()
                .disabled(!catalog.install_globally && !catalog.has_project)
                .accessibility_label(hint)
                .tooltip(hint)
                .on_click(move |_, window, cx| install(SettingsCatalogAction::Install, window, cx)),
        );
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_3()
            .child(settings_scope_picker(
                "extension-scope",
                catalog.install_globally,
                move |global, window, cx| scope(SettingsCatalogAction::Scope(global), window, cx),
            ))
            .child(actions)
    };
    div()
        .mt_5()
        .flex()
        .flex_col()
        .gap_3()
        .when_some(catalog.status.clone(), |page, status| {
            page.child(
                div()
                    .debug_selector(|| "settings-catalog-status".into())
                    .text_sm()
                    .child(status),
            )
        })
        .child(toolbar)
        .children(catalog.rows.iter().map(|row| {
            let callback = callback.clone();
            settings_catalog_row(
                kind,
                row,
                move |action, window, cx| callback(action, window, cx),
                window,
                cx,
            )
        }))
        .when(catalog.rows.is_empty(), |page| {
            page.child(
                super::settings_group(cx)
                    .debug_selector(|| "settings-catalog-empty".into())
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if kind == SettingsCatalogKind::Skills {
                        "No skills found. Attach a project to discover skills."
                    } else {
                        "No WASI extensions found. Use Install .wasm to add one."
                    }),
            )
        })
        .into_any_element()
}
