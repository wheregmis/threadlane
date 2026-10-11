//! Right-panel chrome. Hosts choose available surfaces and own navigation and refresh.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable};
use std::rc::Rc;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RightPanelSurface {
    Trajectory,
    Agents,
    Review,
    Files,
    Browser,
}

impl RightPanelSurface {
    pub const ALL: [Self; 5] = [
        Self::Trajectory,
        Self::Agents,
        Self::Review,
        Self::Files,
        Self::Browser,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Trajectory => "Trajectory",
            Self::Agents => "Agents",
            Self::Review => "Review",
            Self::Files => "Files",
            Self::Browser => "Browser",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Trajectory => IconName::GalleryVerticalEnd,
            Self::Agents => IconName::Bot,
            Self::Review => IconName::File,
            Self::Files => IconName::Folder,
            Self::Browser => IconName::Globe,
        }
    }
}

pub fn right_panel_header(
    active: Option<RightPanelSurface>,
    surfaces: &[RightPanelSurface],
    refresh: Option<Button>,
    on_select: impl Fn(&RightPanelSurface, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme().colors;
    let selected = active.and_then(|active| surfaces.iter().position(|surface| *surface == active));
    let surfaces = surfaces.to_vec();
    div()
        .flex_none()
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(theme.title_bar_border)
        .bg(theme.title_bar)
        .child(
            div()
                .h(threadlane_ui_theme::WINDOW_CONTROLS_CLEARANCE)
                .flex_none()
                .flex()
                .items_center()
                .px_3()
                // Workspace window controls occupy the trailing edge.
                .pr_16()
                .child(
                    div()
                        .id("right-panel-title")
                        .debug_selector(|| "right-panel-title".into())
                        .role(Role::Heading)
                        .aria_label(active.map_or("Tools", RightPanelSurface::label))
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(active.map_or("Tools", RightPanelSurface::label)),
                ),
        )
        // Switching tabs only once something is open: with nothing open the
        // chooser below is the one control, and a tab strip would both repeat
        // it and mark its first entry as selected.
        .when(active.is_some(), |header| header.child(
            div()
                .flex_none()
                .min_h(rems(2.0))
                .flex()
                .items_center()
                .px_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .w_full()
                        .child(
                            TabBar::new("right-panel-surface-tabs")
                                .flex_1()
                                .min_w_0()
                                .segmented()
                                .small()
                                .selected_index(selected.unwrap_or_default())
                                .children(surfaces.iter().map(|surface| {
                                    let label = surface.label();
                                    Tab::new()
                                        .icon(surface.icon())
                                        .debug_selector(move || format!("right-panel-tab-{label}"))
                                        .aria_label(format!("{label} panel"))
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(label).build(window, cx)
                                        })
                                }))
                                .on_click(move |ix, window, cx| {
                                    if let Some(surface) = surfaces.get(*ix) {
                                        on_select(surface, window, cx);
                                    }
                                }),
                        )
                        .children(refresh),
                ),
        ))
}

/// The host supplies refresh eligibility and its callback.
pub fn right_panel_refresh_button() -> Button {
    Button::new("right-panel-refresh")
        .accessibility_label("Refresh surface")
        .icon(Icon::default().path("icons/refresh-cw.svg"))
        .tooltip("Refresh surface")
        .ghost()
        .xsmall()
}

pub fn right_panel_chooser(
    surfaces: &[RightPanelSurface],
    on_select: impl Fn(&RightPanelSurface, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let on_select = Rc::new(on_select);
    div()
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p_6()
        .child(
            div()
                .w_full()
                .max_w(rems(26.0))
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Open a panel"),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child("Pick what to show beside the chat"),
                )
                .child(div().mt_4().w_full().flex().flex_col().gap_2().children(
                    surfaces.iter().copied().map(|surface| {
                        let on_select = on_select.clone();
                        let label = surface.label();
                        Button::new(SharedString::from(format!(
                            "right-panel-card-{}",
                            label.to_lowercase()
                        )))
                        .accessibility_label(label)
                        .debug_selector(move || format!("right-panel-choice-{label}"))
                        .icon(surface.icon())
                        .label(label)
                        .outline()
                        .w_full()
                        .justify_start()
                        .on_click(move |_, window, cx| on_select(&surface, window, cx))
                    }),
                )),
        )
}
