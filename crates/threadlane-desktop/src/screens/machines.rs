//! Spaces and machines (wireframe 7): which spaces this app is paired with,
//! which machines help in each and what each can run, the client keys each
//! hub admitted, a join code for another app, unpairing this app, and
//! pairing it with another space.

use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_desktop::{Part, Reach, When, machine_row};

use super::gap_line;
use crate::Desktop;
use crate::components::{button, card, empty_state, field, heading, meta, section, status_badge};
use crate::icons::AppIcon;
use crate::tokens::{
    JOIN_NAME_WIDTH, KEY_RADIUS, READING_WIDTH, ROW_RADIUS, SM, SPACE_NAME_WIDTH, Status,
    TYPE_BODY, TYPE_META, XL, XS,
};

impl Desktop {
    pub(crate) fn machines(&self, when: &When, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut screen = v_flex()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .id("machines")
            .gap(XL)
            .child(heading("Spaces and machines"));
        if self.state.spaces().is_empty() {
            screen = screen.child(empty_state("No hub is paired.", None, cx));
        }
        for space in self.state.spaces() {
            let (reach, status) = match &space.reach {
                Reach::Unknown => ("asking…".to_owned(), Status::Idle),
                Reach::Reachable => ("reachable".to_owned(), Status::Done),
                Reach::Unreachable(why) => (format!("out of reach: {why}"), Status::Failed),
            };
            let mut part = card(cx)
                .child(section(space.name.clone()).child(status_badge(reach, status, cx).min_w_0()))
                .child(match space.gap(Part::Machines) {
                    Some(gap) => gap_line(&gap, "the machines", cx),
                    None => div().text_size(TYPE_BODY).text_color(muted).child(format!(
                        "{} machines joined · {} running",
                        space.machines.len(),
                        space.running.len()
                    )),
                });

            for machine in &space.machines {
                let row = machine_row(machine, when);
                part = part.child(
                    v_flex()
                        .gap(XS)
                        .pt(SM)
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .gap(SM)
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(row.name))
                                .flex_wrap()
                                .justify_between()
                                .child(status_badge(row.state, machine.status.into(), cx)),
                        )
                        .child(div().text_size(TYPE_BODY).child(format!(
                            "Tools: {} · {}",
                            row.backends.join(" · "),
                            row.gh
                        )))
                        .child(meta(format!("Load: {}", row.load), cx).text_right()),
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
                        .text_size(TYPE_BODY)
                        .child(format!("{} · {} · {limit}", account.label, account.backend)),
                );
            }

            part = part.child(section("Paired apps"));
            if let Some(gap) = space.gap(Part::Clients) {
                part = part.child(gap_line(&gap, "the paired apps", cx));
            } else if space.clients.is_empty() {
                part = part.child(
                    div()
                        .text_size(TYPE_BODY)
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
                part = part.child(
                    h_flex()
                        .gap(SM)
                        .flex_wrap()
                        .justify_between()
                        .child(client.name.clone())
                        .child(
                            h_flex()
                                .gap(SM)
                                .child(status_badge(
                                    state,
                                    if client.is_active() {
                                        Status::Done
                                    } else {
                                        Status::Failed
                                    },
                                    cx,
                                ))
                                .child(meta(when.moment(client.admitted_at), cx).text_right()),
                        ),
                );
            }

            let name = space.name.clone();
            part = part.child(
                h_flex()
                    .gap(SM)
                    .child(
                        button(SharedString::from(format!("code-{name}")))
                            .icon(AppIcon::KeyRound)
                            .label("New join code")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.issue_code(name.clone(), cx);
                            })),
                    )
                    .child(
                        div()
                            .text_size(TYPE_BODY)
                            .text_color(muted)
                            .child("Pairs another app with this space. It works once."),
                    ),
            );
            if let Some(code) = self.state.code(&space.name) {
                part = part.child(
                    v_flex()
                        .gap(XS)
                        .child(div().self_start().px(SM).py(XS).rounded(KEY_RADIUS)
                            .bg(cx.theme().muted).text_size(TYPE_BODY).child(code.code.clone()))
                        .child(div().text_size(TYPE_META).text_color(muted).child(format!(
                            "Run `threadlane hub join <name> <code>` on the other computer before {}.",
                            when.time(code.expires_at)
                        ))),
                );
            }
            let name = space.name.clone();
            part = if self.state.unpairing() == Some(space.name.as_str()) {
                let cancel = button(SharedString::from(format!("unpair-cancel-{name}")))
                    .label("Keep it")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.cancel_unpair();
                        cx.notify();
                    }));
                part.child(
                    v_flex()
                        .gap(SM)
                        .pt(SM)
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(div().text_size(TYPE_BODY).child(format!(
                            "Unpair this app from {name}? It forgets the hub and its key here;                              the hub keeps its projects and memory. Pairing again needs a new                              join code."
                        )))
                        .child(
                            h_flex()
                                .gap(SM)
                                .child(
                                    button(SharedString::from(format!(
                                        "unpair-confirm-{name}"
                                    )))
                                    .danger()
                                    .icon(AppIcon::Unlink).label(format!("Unpair {name}"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.unpair_space(name.clone(), cx);
                                    })),
                                )
                                .child(cancel),
                        ),
                )
            } else {
                part.child(
                    button(SharedString::from(format!("unpair-{name}")))
                        .ghost()
                        .icon(AppIcon::Unlink)
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
                card(cx)
                    .child(section("+ Add space"))
                    .child(
                        div()
                            .text_size(TYPE_BODY)
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
            .gap(SM)
            .child(
                div()
                    .w_full()
                    .max_w(JOIN_NAME_WIDTH)
                    .child(field(&self.space_name)),
            )
            .child(div().flex_1().child(field(&self.join_code)))
            .child(
                button("pair-space")
                    .primary()
                    .icon(AppIcon::Link)
                    .label("Pair")
                    .on_click(cx.listener(|this, _, _, cx| this.join_space(cx))),
            )
    }

    /// What the app shows while no hub is paired: how to start one and
    /// pair with it, by its data directory or a join code.
    pub(crate) fn first_run(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let step = |title: &'static str, text: &'static str| {
            card(cx).child(section(title)).child(meta(text, cx))
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full().max_w(READING_WIDTH).self_center()
            .overflow_y_scrollbar().id("first-run")
            .gap(XL)
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
                        .p(SM)
                        .text_size(TYPE_BODY)
                        .bg(cx.theme().muted)
                        .rounded(ROW_RADIUS)
                        .child("threadlane-hub --data-dir ~/threadlane/work run"),
                ),
            )
            .child(
                step(
                    "2. Pair with it",
                    "Name the space, then give the hub's data directory if it runs on this \
                     computer, or a join code from `threadlane pair-code` on a computer already paired.",
                )
                .child(div().w_full().max_w(SPACE_NAME_WIDTH).child(field(&self.space_name)))
                .child(
                    h_flex()
                        .gap(SM)
                        .child(div().flex_1().child(field(&self.hub_dir)))
                        .child(
                            button("pair-directory")
                                .primary()
                                .icon(AppIcon::FolderOpen).label("Pair by directory")
                                .on_click(cx.listener(|this, _, _, cx| this.pair_directory(cx))),
                        ),
                )
                .child(
                    h_flex()
                        .gap(SM)
                        .child(div().flex_1().child(field(&self.join_code)))
                        .child(
                            button("pair-code")
                                .primary()
                                .icon(AppIcon::Link).label("Pair by join code")
                                .on_click(cx.listener(|this, _, _, cx| this.join_space(cx))),
                        ),
                ),
            )
            .into_any_element()
    }
}
