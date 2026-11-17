//! The agents of each space: the definitions half of wireframe 6, without
//! the modes grid (milestone 10).

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

use super::{heading, section};
use crate::Desktop;

impl Desktop {
    pub(crate) fn agents(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut screen = v_flex()
            .id("agents")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_4()
            .child(heading("Agents"))
            .child(div().text_sm().text_color(muted).child(
                "Agents are defined per space. Change an agent's mode in a project's Agents tab.",
            ));
        for space in self.state.spaces() {
            let mut part = v_flex()
                .gap_2()
                .child(section(format!("Definitions in {}", space.name)));
            for agent in &space.agents {
                let backends = if agent.backends.is_empty() {
                    "Either backend".to_owned()
                } else {
                    agent
                        .backends
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" then ")
                };
                part = part.child(
                    h_flex()
                        .gap_4()
                        .child(
                            div()
                                .w(px(160.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(agent.name.clone()),
                        )
                        .child(div().w(px(240.)).text_sm().child(agent.role.clone()))
                        .child(div().text_sm().text_color(muted).child(backends)),
                );
            }
            screen = screen.child(part);
        }
        screen.into_any_element()
    }
}
