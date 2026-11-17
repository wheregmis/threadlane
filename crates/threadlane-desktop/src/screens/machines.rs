//! Spaces and machines (wireframe 7): which spaces this app is paired with,
//! which machines help in each and what each can run, the client keys each
//! hub admitted, a join code for another app, unpairing this app, and
//! pairing it with another space.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_desktop::{Part, Reach, When, machine_row};

use super::{gap_line, heading, section};
use crate::Desktop;

impl Desktop {
    pub(crate) fn machines(&self, when: &When, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut screen = v_flex()
            .id("machines")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_5()
            .child(heading("Spaces and machines"));
        if self.state.spaces().is_empty() {
            screen = screen.child(div().text_color(muted).child("No hub is paired."));
        }
        for space in self.state.spaces() {
            let reach = match &space.reach {
                Reach::Unknown => "asking…".to_owned(),
                Reach::Reachable => "reachable".to_owned(),
                Reach::Unreachable(why) => format!("out of reach: {why}"),
            };
            let mut part = v_flex()
                .gap_2()
                .p_3()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .child(
                    h_flex()
                        .gap_2()
                        .child(section(space.name.clone()))
                        .child(div().text_sm().text_color(muted).child(reach)),
                )
                .child(match space.gap(Part::Machines) {
                    Some(gap) => gap_line(&gap, "the machines", cx),
                    None => div().text_sm().text_color(muted).child(format!(
                        "{} machines joined · {} running",
                        space.machines.len(),
                        space.running.len()
                    )),
                });

            for machine in &space.machines {
                let row = machine_row(machine, when);
                part = part.child(
                    v_flex()
                        .gap_1()
                        .pt_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(row.name))
                                .child(div().text_sm().text_color(muted).child(row.state)),
                        )
                        .child(div().text_sm().child(format!(
                            "Tools: {} · {}",
                            row.backends.join(" · "),
                            row.gh
                        )))
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child(format!("Load: {}", row.load)),
                        ),
                );
            }

            if !space.accounts.is_empty() {
                part = part.child(section("Model accounts"));
            }
            for account in &space.accounts {
                let limit = account
                    .cooldown_until
                    .filter(|until| *until > when.now)
                    .map_or_else(
                        || "available".to_owned(),
                        |until| format!("limit reached until {}", when.moment(until)),
                    );
                part = part.child(
                    div()
                        .text_sm()
                        .child(format!("{} · {} · {limit}", account.label, account.backend)),
                );
            }

            part = part.child(section("Paired apps"));
            if let Some(gap) = space.gap(Part::Clients) {
                part = part.child(gap_line(&gap, "the paired apps", cx));
            } else if space.clients.is_empty() {
                part = part.child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("None over the network; this app reaches it on this computer."),
                );
            }
            for client in &space.clients {
                let state = if client.is_active() {
                    "admitted"
                } else {
                    "revoked"
                };
                part = part.child(div().text_sm().child(format!(
                    "{} · {state} {}",
                    client.name,
                    when.moment(client.admitted_at)
                )));
            }

            let name = space.name.clone();
            part = part.child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("code-{name}")))
                            .label("New join code")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.issue_code(name.clone(), cx);
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("Pairs another app with this space. It works once."),
                    ),
            );
            if let Some(code) = self.state.code(&space.name) {
                part = part.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(code.code.clone()))
                        .child(div().text_xs().text_color(muted).child(format!(
                            "Run `threadlane hub join <name> <code>` on the other computer before {}.",
                            when.time(code.expires_at)
                        ))),
                );
            }
            let name = space.name.clone();
            part = if self.state.unpairing() == Some(space.name.as_str()) {
                let cancel = Button::new(SharedString::from(format!("unpair-cancel-{name}")))
                    .label("Keep it")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.cancel_unpair();
                        cx.notify();
                    }));
                part.child(
                    v_flex()
                        .gap_2()
                        .pt_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(div().text_sm().child(format!(
                            "Unpair this app from {name}? It forgets the hub and its key here;                              the hub keeps its projects and memory. Pairing again needs a new                              join code."
                        )))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "unpair-confirm-{name}"
                                    )))
                                    .danger()
                                    .label(format!("Unpair {name}"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.unpair_space(name.clone(), cx);
                                    })),
                                )
                                .child(cancel),
                        ),
                )
            } else {
                part.child(
                    Button::new(SharedString::from(format!("unpair-{name}")))
                        .ghost()
                        .label("Unpair this app")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.ask_unpair(&name);
                            cx.notify();
                        })),
                )
            };
            screen = screen.child(part);
        }
        screen
            .child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .child(section("+ Add space"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("Pair this app with another hub using its join code."),
                    )
                    .child(self.join_form(cx)),
            )
            .into_any_element()
    }

    /// The name and join code of a space to pair, and Pair.
    fn join_form(&self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .gap_2()
            .child(div().w(px(180.)).child(Input::new(&self.space_name)))
            .child(div().flex_1().child(Input::new(&self.join_code)))
            .child(
                Button::new("pair-space")
                    .primary()
                    .label("Pair")
                    .on_click(cx.listener(|this, _, _, cx| this.join_space(cx))),
            )
    }

    /// What the app shows while no hub is paired: how to start one and
    /// pair with it, by its data directory or a join code.
    pub(crate) fn first_run(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let step = |title: &'static str, text: &'static str| {
            v_flex()
                .gap_1()
                .child(section(title))
                .child(div().text_sm().text_color(muted).child(text))
        };
        v_flex()
            .id("first-run")
            .flex_1()
            .min_h_0()
            .max_w(px(720.))
            .overflow_y_scroll()
            .gap_5()
            .child(heading("Pair this app with a hub"))
            .child(div().text_color(muted).child(
                "A hub keeps one space's projects, agents and memory. This app shows every \
                 hub it is paired with.",
            ))
            .child(
                step(
                    "1. Start a hub",
                    "On the computer that will run the work, start one with its own data \
                     directory. It keeps running in the terminal.",
                )
                .child(
                    div()
                        .p_2()
                        .text_sm()
                        .bg(cx.theme().muted)
                        .rounded(cx.theme().radius)
                        .child("threadlane-hub --data-dir ~/threadlane/work run"),
                ),
            )
            .child(
                step(
                    "2. Pair with it",
                    "Name the space, then give the hub's data directory if it runs on this \
                     computer, or a join code from `threadlane pair-code` on a computer already paired.",
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().w(px(180.)).child(Input::new(&self.space_name)))
                        .child(div().flex_1().child(Input::new(&self.hub_dir)))
                        .child(
                            Button::new("pair-directory")
                                .primary()
                                .label("Pair by directory")
                                .on_click(cx.listener(|this, _, _, cx| this.pair_directory(cx))),
                        ),
                )
                .child(div().text_sm().text_color(muted).child("Or with a join code:"))
                .child(self.join_form(cx)),
            )
            .into_any_element()
    }
}
