//! Settings (the sidebar entry): what each hub reports about itself and how
//! this app reaches it, this app's client directory, which system
//! notifications it raises, and its version.

use gpui_kit::component::button::{ButtonGroup, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::*;
use threadlane_core::model::Mode;
use threadlane_desktop::{Appearance, Consequence, PACING_FIELDS};

use super::gap_line;

use super::project::mode_name;
use crate::Desktop;
use crate::components::{button, card, field, heading, meta, section};
use crate::icons::AppIcon;
use crate::menus::{DarkAppearance, LightAppearance, SystemAppearance};
use crate::tokens::{LABEL_WIDTH, MD, READING_WIDTH, SCROLLBAR_GUTTER, SM, TYPE_BODY, XL, XS};

impl Desktop {
    pub(crate) fn settings(
        &self,
        scope: Option<&str>,
        when: &threadlane_desktop::When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let line = |label: &'static str, value: String| {
            h_flex()
                .gap(MD)
                .items_start()
                .text_size(TYPE_BODY)
                .child(
                    div()
                        .w(LABEL_WIDTH)
                        .flex_none()
                        .text_color(muted)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().text_left().child(value))
        };
        let unknown = || "not reported yet".to_owned();
        let mut screen = v_flex()
            .w_full()
            .max_w(READING_WIDTH)
            .mx_auto()
            .pr(SCROLLBAR_GUTTER)
            .pb_4()
            .gap(XL)
            .child(heading(if scope.is_some() {
                "Space settings"
            } else {
                "App settings"
            }));
        for row in self
            .state
            .settings_rows()
            .into_iter()
            .filter(|row| scope == Some(row.paired_as.as_str()))
        {
            let ceiling = self.space_ceiling(&row.paired_as, cx);
            let pacing = self.space_pacing(&row.paired_as, cx);
            let mut metrics = v_flex().gap(XS).pt(SM).child(section("Metrics"));
            metrics = match self.state.metrics_rows(&row.paired_as, when) {
                Ok(rows) => {
                    metrics.children(rows.into_iter().map(|(label, value)| line(label, value)))
                }
                Err(_) => metrics.child(gap_line(
                    &self.state.metrics_gap(&row.paired_as),
                    "metrics",
                    &row.paired_as,
                    cx,
                )),
            };
            screen = screen.child(
                card(cx)
                    .child(section(row.paired_as.clone()))
                    .child(line("Space", row.space.unwrap_or_else(unknown)))
                    .child(line(
                        "Ceiling",
                        row.ceiling
                            .map_or_else(unknown, |mode| mode_name(mode).to_owned()),
                    ))
                    .child(line(
                        "Schema version",
                        row.schema_version
                            .map_or_else(unknown, |version| version.to_string()),
                    ))
                    .child(line("Hub build", row.build))
                    .child(line("Hosted on", row.host.unwrap_or_else(unknown)))
                    .child(line(
                        "Reached by",
                        if row.reached_by.is_empty() {
                            unknown()
                        } else {
                            row.reached_by
                        },
                    ))
                    .child(self.update_controls(
                        Some(&row.paired_as),
                        row.update_version.as_deref(),
                        cx,
                    ))
                    .child(ceiling)
                    .child(pacing)
                    .child(metrics),
            );
        }
        if scope.is_some() {
            return v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .id("settings")
                .overflow_y_scrollbar()
                .child(screen)
                .into_any_element();
        }
        let notifications = self.state.notifications();
        let toggle = |id: &'static str, label: &'static str, on: bool, decisions: bool| {
            Switch::new(id)
                .checked(on)
                .label(label)
                .accessibility_label(label)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    this.set_notifications(decisions, *checked, cx);
                }))
        };
        let dir = self
            .state
            .client_dir()
            .map_or_else(unknown, |dir| dir.display().to_string());
        let appearance = self.state.appearance();
        let appearance_control = ButtonGroup::new("app-appearance")
            .outline()
            .compact()
            .child(
                button("appearance-system")
                    .label("System")
                    .selected(appearance == Appearance::System),
            )
            .child(
                button("appearance-light")
                    .label("Light")
                    .selected(appearance == Appearance::Light),
            )
            .child(
                button("appearance-dark")
                    .label("Dark")
                    .selected(appearance == Appearance::Dark),
            )
            .on_click(|clicked: &Vec<usize>, window, cx| {
                let action: Option<Box<dyn Action>> = match clicked.first().copied() {
                    Some(0) => Some(Box::new(SystemAppearance)),
                    Some(1) => Some(Box::new(LightAppearance)),
                    Some(2) => Some(Box::new(DarkAppearance)),
                    _ => None,
                };
                if let Some(action) = action {
                    window.dispatch_action(action, cx);
                }
            });
        let screen = screen
            .child(
                card(cx).child(section("Appearance")).child(
                    h_flex()
                        .gap(MD)
                        .items_center()
                        .text_size(TYPE_BODY)
                        .child(
                            div()
                                .w(LABEL_WIDTH)
                                .flex_none()
                                .text_color(muted)
                                .child("Theme"),
                        )
                        .child(appearance_control),
                ),
            )
            .child(
                card(cx)
                    .child(section("Notifications"))
                    .child(toggle(
                        "notify-decisions",
                        "Notify me of new decisions",
                        notifications.decisions,
                        true,
                    ))
                    .child(toggle(
                        "notify-notices",
                        "Notify me of system notices",
                        notifications.notices,
                        false,
                    ))
                    .child(meta(
                        format!("Kept in {dir}/{}.", threadlane_desktop::SETTINGS_FILE),
                        cx,
                    )),
            )
            .child(
                card(cx)
                    .child(section("This app"))
                    .child(
                        Switch::new("hold-messages")
                            .checked(self.state.hold_messages())
                            .label("Hold messages for 3 seconds so a send can be undone")
                            .accessibility_label(
                                "Hold messages for 3 seconds so a send can be undone",
                            )
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_hold_messages(*checked, cx);
                            })),
                    )
                    .child(line("Client directory", dir))
                    .child(line("Build", crate::local_hub::app_build().to_string()))
                    .child(self.update_controls(None, None, cx)),
            );
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .id("settings")
            .overflow_y_scrollbar()
            .child(screen)
            .into_any_element()
    }
}

impl Desktop {
    /// The space's own ceiling control, and while you are asked, what the
    /// new ceiling permits with "Set" and "Keep".
    fn space_ceiling(&self, space: &str, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let choices = self.state.space_ceiling_choices(space);
        let named = space.to_owned();
        let modes: Vec<Mode> = choices.iter().map(|choice| choice.mode).collect();
        let control = ButtonGroup::new(SharedString::from(format!("space-ceiling-{space}")))
            .outline()
            .compact()
            .children(choices.iter().map(|choice| {
                button(SharedString::from(format!(
                    "space-ceiling-{space}-{}",
                    choice.mode
                )))
                .label(choice.label)
                .accessibility_label(format!("Set the ceiling of {space} to {}", choice.label))
                .selected(choice.selected)
            }))
            .on_click(cx.listener(move |this, clicked: &Vec<usize>, window, cx| {
                if let Some(mode) = clicked.first().and_then(|index| modes.get(*index)) {
                    this.confirm_action(
                        Consequence::SetCeiling(named.clone(), None, *mode),
                        window,
                        cx,
                    );
                }
            }));
        v_flex().gap(XS).pt(SM).child(
            h_flex()
                .gap(MD)
                .text_size(TYPE_BODY)
                .child(
                    div()
                        .w(LABEL_WIDTH)
                        .flex_none()
                        .text_color(muted)
                        .child("Space ceiling"),
                )
                .child(control),
        )
    }
}

impl Desktop {
    fn space_pacing(&self, space: &str, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let mut panel = v_flex()
            .gap(SM)
            .pt(MD)
            .items_start()
            .child(section("Delivery pacing"));
        let editing = self.state.pacing_editor(space);
        let values = match self.state.pacing_values(space) {
            Ok(values) => values,
            Err(_) => {
                panel = panel.child(gap_line(
                    &self.state.pacing_gap(space),
                    "delivery pacing",
                    space,
                    cx,
                ));
                if editing.is_none() {
                    return panel;
                }
                Default::default()
            }
        };
        for (index, (spec, value)) in PACING_FIELDS.iter().zip(values).enumerate() {
            if editing.is_none() && value.is_empty() {
                continue;
            }
            let label = div()
                .text_size(TYPE_BODY)
                .font_weight(FontWeight::MEDIUM)
                .child(spec.label);
            let mut row = v_flex().gap(XS).w_full().child(label);
            if !spec.explanation.is_empty() {
                row = row.child(
                    div()
                        .text_size(TYPE_BODY)
                        .text_color(muted)
                        .child(spec.explanation),
                );
            }
            panel = panel.child(match editing {
                Some(saving) => row.child(
                    field(&self.pacing_inputs[index])
                        .aria_label(format!("{} for {space}", spec.label))
                        .disabled(saving),
                ),
                None => row.child(div().text_size(TYPE_BODY).child(value)),
            });
        }
        let space = space.to_owned();
        let controls = match editing {
            Some(saving) => h_flex()
                .gap(SM)
                .child(
                    button(SharedString::from(format!("save-pacing-{space}")))
                        .small()
                        .primary()
                        .tab_stop(true)
                        .disabled(saving)
                        .icon(AppIcon::Save)
                        .label(if saving { "Saving…" } else { "Save pacing" })
                        .accessibility_label(format!("Save delivery pacing for {space}"))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.save_pacing(space.clone(), cx)),
                        ),
                )
                .child(
                    button("keep-pacing")
                        .small()
                        .ghost()
                        .tab_stop(true)
                        .disabled(saving)
                        .icon(AppIcon::X)
                        .label("Cancel")
                        .accessibility_label("Cancel delivery pacing edit")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.keep_pacing();
                            cx.notify();
                        })),
                ),
            None => h_flex().child(
                button(SharedString::from(format!("edit-pacing-{space}")))
                    .small()
                    .outline()
                    .tab_stop(true)
                    .icon(AppIcon::Pencil)
                    .label("Edit pacing")
                    .disabled(!self.state.can_edit_pacing())
                    .accessibility_label(format!("Edit delivery pacing for {space}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(values) = this.state.edit_pacing(&space) {
                            for (input, value) in this.pacing_inputs.iter().zip(values) {
                                input.update(cx, |input, cx| input.set_value(value, window, cx));
                            }
                            cx.notify();
                        }
                    })),
            ),
        };
        panel.child(controls)
    }
}
