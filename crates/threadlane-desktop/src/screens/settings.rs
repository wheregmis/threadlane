//! Settings (the sidebar entry): what each hub reports about itself and how
//! this app reaches it, this app's client directory, which system
//! notifications it raises, and its version.

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::model::Mode;
use threadlane_desktop::mode_label;

use super::project::mode_name;
use super::{heading, section};
use crate::Desktop;

impl Desktop {
    pub(crate) fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let line = |label: &'static str, value: String| {
            h_flex()
                .gap_3()
                .text_sm()
                .child(div().w(px(150.)).flex_none().text_color(muted).child(label))
                .child(div().min_w_0().child(value))
        };
        let unknown = || "not reported yet".to_owned();
        let mut screen = v_flex()
            .id("settings")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_3()
            .child(heading("Settings"))
            .child(section("Spaces"));
        for row in self.state.settings_rows() {
            let ceiling = self.space_ceiling(&row.paired_as, cx);
            screen = screen.child(
                v_flex()
                    .gap_1()
                    .p_3()
                    .max_w(px(640.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(row.paired_as.clone()),
                    )
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
                    .child(line("Hosted on", row.host.unwrap_or_else(unknown)))
                    .child(line(
                        "Reached by",
                        if row.reached_by.is_empty() {
                            unknown()
                        } else {
                            row.reached_by
                        },
                    ))
                    .child(ceiling),
            );
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
        screen
            .child(section("Notifications").pt_2())
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
            .child(div().text_xs().text_color(muted).child(format!(
                "Kept in {dir}/{}.",
                threadlane_desktop::SETTINGS_FILE
            )))
            .child(section("This app").pt_2())
            .child(line("Client directory", dir))
            .child(line("Version", env!("CARGO_PKG_VERSION").to_owned()))
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
                Button::new(SharedString::from(format!(
                    "space-ceiling-{space}-{}",
                    choice.mode
                )))
                .label(choice.label)
                .accessibility_label(format!("Set the ceiling of {space} to {}", choice.label))
                .selected(choice.selected)
            }))
            .on_click(cx.listener(move |this, clicked: &Vec<usize>, _, cx| {
                if let Some(mode) = clicked.first().and_then(|index| modes.get(*index)) {
                    this.state.ask_space_ceiling(&named, *mode);
                    cx.notify();
                }
            }));
        let mut part = v_flex().gap_1().pt_2().child(
            h_flex()
                .gap_3()
                .text_sm()
                .child(
                    div()
                        .w(px(150.))
                        .flex_none()
                        .text_color(muted)
                        .child("Space ceiling"),
                )
                .child(control),
        );
        if let Some((mode, question)) = self.state.space_ceiling_confirm(space) {
            let named = space.to_owned();
            part = part.child(div().text_sm().child(question)).child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("space-ceiling-set-{space}")))
                            .primary()
                            .small()
                            .label(format!("Set to {}", mode_label(mode)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_space_ceiling(named.clone(), mode, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("space-ceiling-keep-{space}")))
                            .small()
                            .label("Keep it")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.keep_space_ceiling();
                                cx.notify();
                            })),
                    ),
            );
        }
        part
    }
}
