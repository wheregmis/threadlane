//! Spaces and machines (wireframe 7): which spaces this app is paired with,
//! which machines help in each and what each can run, the client keys each
//! hub admitted, a join code for another app, unpairing this app, and
//! pairing it with another space.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use threadlane_core::ids::MachineId;
use threadlane_desktop::{
    Consequence, Destination, EmptyRegion, MachineRun, Part, When, count_form, machine_row,
};

use super::{empty, gap_line, link};
use crate::components::{button, card, field, heading, meta, section, status_badge, status_dot};
use crate::icons::AppIcon;
use crate::tokens::{
    JOIN_NAME_WIDTH, KEY_RADIUS, READING_WIDTH, ROW_RADIUS, SM, SPACE_NAME_WIDTH, Status,
    TYPE_BODY, TYPE_META, XL, XS,
};
use crate::{Desktop, Screen, Tab};

impl Desktop {
    pub(crate) fn machine(
        &self,
        space: &str,
        id: MachineId,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut page = v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .gap(SM)
            .overflow_y_scrollbar()
            .id("machine-detail")
            .child(
                link("back-to-machines", "Spaces and machines".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Machines, cx))),
            );
        let Some(detail) = self.state.machine_detail(space, id, when) else {
            let gap = self
                .state
                .space(space)
                .and_then(|space| space.gap(Part::Machines));
            return page
                .child(match gap {
                    Some(gap) => gap_line(&gap, "the machine", space, cx),
                    None => empty(EmptyRegion::Machines, cx),
                })
                .into_any_element();
        };
        let report = detail.report;
        page = page
            .child(heading(report.name))
            .child(
                h_flex()
                    .gap(XS)
                    .child(status_dot(detail.status.into(), cx))
                    .child(meta(detail.status.to_string(), cx)),
            )
            .children(detail.platform.map(|platform| meta(platform, cx)))
            .children(
                detail
                    .last_seen
                    .map(|seen| meta(format!("Last seen {seen}"), cx)),
            )
            .child(section("Tools"))
            .child(if detail.tools_reported {
                h_flex().gap(XS).flex_wrap().children(
                    report
                        .backends
                        .into_iter()
                        .chain([report.gh])
                        .map(|tool| status_badge(tool, Status::Idle, cx)),
                )
            } else {
                empty(EmptyRegion::Tools, cx)
            })
            .children((!detail.accounts.is_empty()).then(|| section("Model accounts")))
            .children(
                detail
                    .accounts
                    .into_iter()
                    .map(|account| status_badge(account, Status::Idle, cx)),
            )
            .child(section("Run slots"));
        page = if let Some(value) = detail.occupancy {
            page.child(meta(report.load.clone(), cx)).child(
                Progress::new("machine-occupancy")
                    .value(value)
                    .accessibility_label(report.load),
            )
        } else {
            page.child(meta("This machine has not reported its run slots.", cx))
        };
        page = page.child(meta("The hub has no memory or disk measurements.", cx));
        match self.state.machine_runs(space, id) {
            Ok((active, recent)) => {
                for (title, rows, empty_region) in [
                    ("Running now", active, EmptyRegion::MachineActive),
                    ("Recent runs", recent, EmptyRegion::MachineRecent),
                ] {
                    page = page.child(section(title));
                    if rows.is_empty() {
                        page = page.child(empty(empty_region, cx));
                    }
                    page = page.children(rows.into_iter().map(|run| Self::machine_run(run, cx)));
                }
            }
            Err(gap) => {
                page = page
                    .child(section("Runs"))
                    .child(gap_line(&gap, "machine runs", space, cx))
            }
        }
        page.into_any_element()
    }

    fn machine_run(run: &MachineRun, cx: &mut Context<Self>) -> Button {
        let screen = match &run.destination {
            Destination::Task(space, task) => Screen::Task {
                space: space.clone(),
                task: *task,
            },
            Destination::Lead(space, project) => Screen::Project {
                space: space.clone(),
                project: project.clone(),
                tab: Tab::Conversation,
            },
        };
        link(format!("machine-run-{}", run.id()), run.label.clone())
            .on_click(cx.listener(move |this, _, _, cx| this.go(screen.clone(), cx)))
    }

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
            screen = screen.child(empty(EmptyRegion::Pairing, cx));
        }
        for space in self.state.spaces() {
            let mut part = card(cx).child(section(space.name.clone()));
            part = match space.gap(Part::Machines) {
                Some(gap) => part.child(gap_line(&gap, "machines", &space.name, cx)),
                None if space.machines.is_empty() => part.child(empty(EmptyRegion::Machines, cx)),
                None => part.child(meta(
                    format!(
                        "{} {} joined · {} running",
                        space.machines.len(),
                        count_form(space.machines.len(), "machine", "machines"),
                        space.running.len()
                    ),
                    cx,
                )),
            };

            for machine in &space.machines {
                let row = machine_row(machine, when);
                let open = Screen::Machine {
                    space: space.name.clone(),
                    machine: machine.id,
                };
                part = part.child(
                    v_flex()
                        .gap(XS)
                        .pt(SM)
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .gap(SM)
                                .child(
                                    link(
                                        format!("machine-{}-{}", space.name, machine.id),
                                        row.name,
                                    )
                                    .flex_1()
                                    .min_w_0()
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.go(open.clone(), cx)
                                        }),
                                    ),
                                )
                                .child(
                                    status_badge(row.state, machine.status.into(), cx).flex_none(),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap(SM)
                                .flex_wrap()
                                .child(div().text_size(TYPE_BODY).child(format!(
                                    "Tools: {} · {}",
                                    row.backends.join(" · "),
                                    row.gh
                                )))
                                .child(meta(format!("Load: {}", row.load), cx)),
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
                        .text_size(TYPE_BODY)
                        .child(format!("{} · {} · {limit}", account.label, account.backend)),
                );
            }

            part = part.child(section("Paired apps"));
            if let Some(gap) = space.gap(Part::Clients) {
                part = part.child(gap_line(&gap, "paired apps", &space.name, cx));
            } else if space.clients.is_empty() {
                part = part.child(empty(EmptyRegion::Clients, cx));
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
            part = part.child(
                button(SharedString::from(format!("unpair-{name}")))
                    .small()
                    .ghost()
                    .self_start()
                    .icon(AppIcon::Unlink)
                    .label("Unpair this app…")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_action(Consequence::Unpair(name.clone()), window, cx);
                    })),
            );
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
            .child(empty(EmptyRegion::Pairing, cx))
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
