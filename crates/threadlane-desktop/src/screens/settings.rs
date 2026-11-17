//! Settings (the sidebar entry): what each hub reports about itself and how
//! this app reaches it, this app's client directory, which system
//! notifications it raises, and its version.

use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

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
                    )),
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
