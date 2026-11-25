//! Machines (wireframe 7): which spaces this app is paired with, which
//! machines help in each and what each can run, the client keys each hub
//! admitted, a join code for another app, and unpairing this app. Pairing
//! another space has its own screen.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::*;
use threadlane_core::{ids::MachineId, model::MachineStatus};
use threadlane_desktop::{
    Consequence, Destination, EmptyRegion, MachineRun, Part, Reach, When, count_form,
};

use super::{empty, gap_line, link};
use crate::components::{
    button, card, field, heading, icon_button, meta, page_header, section, status_badge,
};
use crate::icons::AppIcon;
use crate::tokens::{
    READING_WIDTH, ROW_RADIUS, SCROLLBAR_GUTTER, SM, SPACE_NAME_WIDTH, Status, TABULAR, TYPE_BODY,
    XL, XS,
};
use crate::{Desktop, Screen, Tab};

fn running_tasks_label(running: Option<usize>) -> String {
    running.map_or_else(
        || "Running task count unavailable".to_owned(),
        |count| format!("{count} {} running", count_form(count, "task", "tasks")),
    )
}

impl Desktop {
    pub(crate) fn machine(
        &self,
        space: &str,
        id: MachineId,
        when: &When,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut page = v_flex()
            .w_full()
            .max_w(READING_WIDTH)
            .mx_auto()
            .pr(SCROLLBAR_GUTTER)
            .pb_4()
            .gap(SM)
            .child(
                button("back-to-machines")
                    .ghost()
                    .small()
                    .self_start()
                    .tab_stop(true)
                    .icon(AppIcon::ArrowLeft)
                    .label("All machines")
                    .on_click(cx.listener(|this, _, _, cx| this.go(Screen::Machines, cx))),
            );
        let Some(detail) = self.state.machine_detail(space, id, when) else {
            let gap = self
                .state
                .space(space)
                .and_then(|space| space.gap(Part::Machines));
            return v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .id("machine-detail")
                .overflow_y_scrollbar()
                .child(page.child(match gap {
                    Some(gap) => gap_line(&gap, "the machine", space, cx),
                    None => empty(EmptyRegion::Machines, cx),
                }))
                .into_any_element();
        };
        let report = detail.report;
        page = page.child(heading(report.name)).child(
            h_flex()
                .gap(XS)
                .flex_wrap()
                .child(meta(space.to_owned(), cx))
                .child(status_badge(
                    detail.status.to_string(),
                    detail.status.into(),
                    cx,
                ))
                .children(detail.local.then(|| meta("This computer", cx)))
                .children(detail.platform.map(|platform| meta(platform, cx)))
                .children(
                    detail
                        .last_seen
                        .map(|seen| meta(format!("Last seen {seen}"), cx)),
                ),
        );
        let step = |name: &'static str, icon, slots: Option<u32>| {
            let space = space.to_owned();
            icon_button(name, icon, name.into())
                .tab_stop(true)
                .disabled(!detail.limit_writable || self.machine_limit_saving || slots.is_none())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(slots) = slots {
                        this.set_task_limit(space.clone(), id, slots, cx);
                    }
                }))
        };
        let mut capacity = card(cx)
            .child(section(running_tasks_label(detail.running_tasks)))
            .children(detail.occupancy.map(|value| {
                let label = format!("{} (including lead runs)", report.load);
                v_flex().gap(XS).child(meta(label.clone(), cx)).child(
                    Progress::new("machine-occupancy")
                        .value(value)
                        .accessibility_label(label),
                )
            }))
            .child(
                h_flex()
                    .gap(SM)
                    .flex_wrap()
                    .child("Task limit")
                    .child(step(
                        "Decrease task limit",
                        AppIcon::Minus,
                        detail.slots.checked_sub(1).filter(|slots| *slots > 0),
                    ))
                    .child(
                        div()
                            .font_features(TABULAR.clone())
                            .child(if detail.slots == 0 {
                                "—".to_owned()
                            } else {
                                detail.slots.to_string()
                            }),
                    )
                    .child(step(
                        "Increase task limit",
                        AppIcon::Plus,
                        detail.slots.checked_add(1),
                    ))
                    .children(self.machine_limit_saving.then(|| meta("Saving…", cx))),
            );
        if detail.runner_slots > 0 {
            capacity = capacity.child(meta("Tasks and lead conversations share these slots.", cx));
        }
        if let Some(reason) = if detail.status == MachineStatus::Revoked {
            Some("This machine's access has been revoked.")
        } else if !detail.limit_writable {
            Some("Current capacity is unavailable. Reconnect to the space to edit the limit.")
        } else {
            None
        } {
            capacity = capacity.child(meta(reason, cx));
        }
        if let Some((_, _, error)) = self
            .machine_limit_error
            .as_ref()
            .filter(|(failed_space, failed_id, _)| failed_space == space && *failed_id == id)
        {
            capacity = capacity.child(meta(error.clone(), cx).text_color(cx.theme().danger));
        }
        page = page.child(capacity);
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
        let page = page
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
            );
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .id("machine-detail")
            .overflow_y_scrollbar()
            .child(page)
            .into_any_element()
    }

    fn set_task_limit(
        &mut self,
        space: String,
        machine: MachineId,
        slots: u32,
        cx: &mut Context<Self>,
    ) {
        if self.machine_limit_saving {
            return;
        }
        self.machine_limit_saving = true;
        self.machine_limit_error = None;
        let core = self.core.clone();
        self.run(
            async move {
                let result = core
                    .read()
                    .await
                    .set_machine_slots(Some(&space), machine, slots)
                    .await;
                (space, result)
            },
            move |this, (space, result), cx| {
                this.machine_limit_saving = false;
                match result {
                    Ok(()) => {
                        this.state.machine_slots_changed(&space, machine, slots);
                        this.refresh(cx);
                    }
                    Err(error) => {
                        this.machine_limit_error = Some((
                            space,
                            machine,
                            format!(
                                "Could not save the task limit. {}",
                                threadlane_desktop::describe(&error)
                            ),
                        ))
                    }
                }
            },
            cx,
        );
        cx.notify();
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
        let border = cx.theme().border;
        let summary = self.state.local_running_tasks().map(|running| {
            meta(
                format!("{} on this computer", running_tasks_label(Some(running))),
                cx,
            )
        });
        let mut screen = v_flex()
            .w_full()
            .max_w(READING_WIDTH)
            .gap_6()
            .pr_2()
            .pb_2()
            .child(page_header("Machines", summary, None));
        if self.state.spaces().is_empty() {
            screen = screen.child(empty(EmptyRegion::Pairing, cx));
        }
        for space in self.state.spaces() {
            let machine_gap = space.gap(Part::Machines);
            let availability = if machine_gap.is_none() && !space.machines.is_empty() {
                Some(if space.reach == Reach::Reachable {
                    format!(
                        "{} of {} {} online",
                        space.online(),
                        space.machines.len(),
                        count_form(space.machines.len(), "machine", "machines")
                    )
                } else {
                    "Space unavailable. Showing last known machines.".to_owned()
                })
            } else {
                None
            };

            let mut part = v_flex()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(border)
                .child(
                    h_flex()
                        .flex_wrap()
                        .justify_between()
                        .gap_x_4()
                        .gap_y_1()
                        .px_4()
                        .py_3()
                        .bg(cx.theme().group_box)
                        .rounded_t(cx.theme().radius)
                        .child(section(space.name.clone()))
                        .children(availability.map(|label| meta(label, cx))),
                );
            if let Some(gap) = machine_gap {
                part = part.child(gap_line(&gap, "machines", &space.name, cx).px_4().py_3());
            } else if space.machines.is_empty() {
                part = part.child(meta(EmptyRegion::Machines.sentence(), cx).px_4().py_3());
            }
            for machine in &space.machines {
                let open = Screen::Machine {
                    space: space.name.clone(),
                    machine: machine.id,
                };
                let task_limit = if machine.max_parallel > 0 {
                    format!("Task limit {}", machine.max_parallel)
                } else {
                    "Task limit".to_owned()
                };
                let last_seen = machine
                    .last_seen
                    .filter(|_| machine.status == MachineStatus::Offline)
                    .map(|seen| format!("Last seen {}", when.moment(seen)));
                let role = machine.is_hub.then(|| {
                    if space.is_local_machine(machine) {
                        "Hub on this computer"
                    } else {
                        "Hub"
                    }
                });
                part = part.child(
                    h_flex()
                        .flex_wrap()
                        .gap_x_4()
                        .gap_y_2()
                        .px_4()
                        .py_3()
                        .border_t_1()
                        .border_color(border)
                        .child(
                            h_flex()
                                .flex_1()
                                .flex_basis(rems(10.))
                                .min_w_0()
                                .gap_3()
                                .child(
                                    Icon::new(AppIcon::Monitor)
                                        .text_color(cx.theme().muted_foreground)
                                        .flex_none(),
                                )
                                .child(
                                    v_flex()
                                        .min_w_0()
                                        .gap_1()
                                        .child(
                                            div()
                                                .whitespace_normal()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(machine.name.clone()),
                                        )
                                        .child(
                                            h_flex()
                                                .flex_wrap()
                                                .gap_x_2()
                                                .children(role.map(|label| meta(label, cx)))
                                                .children(last_seen.map(|seen| meta(seen, cx))),
                                        ),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .child(status_badge(
                                    machine.status.to_string(),
                                    machine.status.into(),
                                    cx,
                                ))
                                .child(meta(
                                    running_tasks_label(space.machine_running_tasks(machine)),
                                    cx,
                                ))
                                .child(
                                    button(format!("machine-{}-{}", space.name, machine.id))
                                        .small()
                                        .outline()
                                        .label(task_limit.clone())
                                        .child(Icon::new(AppIcon::ChevronRight).size_4())
                                        .tooltip("View machine details")
                                        .accessibility_label(format!(
                                            "View details for {} in {}. {task_limit}.",
                                            machine.name, space.name,
                                        ))
                                        .tab_stop(true)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.go(open.clone(), cx)
                                        })),
                                ),
                        ),
                );
            }

            if !space.accounts.is_empty() {
                part = part.child(
                    v_flex()
                        .gap_2()
                        .border_t_1()
                        .border_color(border)
                        .p_4()
                        .child(section("Model accounts"))
                        .children(space.accounts.iter().map(|account| {
                            let limit = account
                                .cooldown_until
                                .filter(|until| *until > when.now)
                                .map_or_else(
                                    || "Available".to_owned(),
                                    |until| format!("Limit reached until {}", when.moment(until)),
                                );
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .child(account.label.clone())
                                .child(meta(account.backend.to_string(), cx))
                                .child(meta(limit, cx))
                        })),
                );
            }

            let client_gap = space.gap(Part::Clients);
            let name = space.name.clone();
            let unpair = button(format!("unpair-{name}"))
                .ghost()
                .icon(AppIcon::Unlink)
                .label("Unpair this app…")
                .accessibility_label(format!("Unpair this app from {name}"))
                .tab_stop(true)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.confirm_action(Consequence::Unpair(name.clone()), window, cx);
                }));
            let name = space.name.clone();
            let mut paired = v_flex()
                .gap_2()
                .border_t_1()
                .border_color(border)
                .p_4()
                .child(
                h_flex()
                    .flex_wrap()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .flex_basis(rems(12.))
                            .min_w_0()
                            .child(section("Paired apps"))
                            .children(
                                (client_gap.is_none() && space.clients.is_empty())
                                    .then(|| meta("No apps paired over the network.", cx)),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                button(format!("code-{name}"))
                                    .icon(AppIcon::KeyRound)
                                    .label("New join code")
                                    .accessibility_label(format!("New join code for {name}"))
                                    .tooltip(
                                        "Pair another app with this space using a single-use code",
                                    )
                                    .tab_stop(true)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.issue_code(name.clone(), cx);
                                    })),
                            )
                            .child(unpair),
                    ),
            );
            if let Some(gap) = client_gap {
                paired = paired.child(gap_line(&gap, "paired apps", &space.name, cx).py_1());
            }
            for client in &space.clients {
                let state = if client.is_active() {
                    "Admitted"
                } else {
                    "Revoked"
                };
                paired = paired.child(
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

            if let Some(code) = self.state.code(&space.name) {
                paired = paired.child(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().muted)
                        .child(
                            div()
                                .whitespace_normal()
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(code.code.clone()),
                        )
                        .child(meta(
                            format!(
                                "Run `threadlane hub join <name> <code>` on the other computer before {}. The code works once.",
                                when.time(code.expires_at)
                            ),
                            cx,
                        )),
                );
            }
            screen = screen.child(part.child(paired));
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .id("machines")
            .overflow_y_scrollbar()
            .child(screen.mx_auto())
            .into_any_element()
    }

    /// The pairing screen the space switcher opens.
    pub(crate) fn add_space(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = v_flex()
            .w_full()
            .max_w(READING_WIDTH)
            .mx_auto()
            .pr(SCROLLBAR_GUTTER)
            .pb_4()
            .gap(SM)
            .child(heading("Add space"))
            .child(card(cx).child(self.join_form(cx)));
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .id("add-space")
            .overflow_y_scrollbar()
            .child(content)
            .into_any_element()
    }

    fn choose_hub_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = selection.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.first() {
                        this.hub_dir.update(cx, |field, cx| {
                            field.set_value(path.display().to_string(), window, cx)
                        });
                    }
                }
                Ok(Err(error)) => {
                    this.state
                        .report(threadlane_desktop::Activity::Failed(error.to_string()));
                    cx.notify();
                }
                _ => {}
            });
        })
        .detach();
    }

    /// Names a space and pairs it by join code or by local data directory.
    fn join_form(&self, cx: &mut Context<Self>) -> Div {
        let remote = self.remote_pairing;
        let mut locations = h_flex().gap(XS);
        for (id, label, other) in [
            ("local-hub", "This computer", false),
            ("remote-hub", "Another computer", true),
        ] {
            locations = locations.child(
                button(id)
                    .small()
                    .outline()
                    .label(label)
                    .selected(remote == other)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remote_pairing = other;
                        cx.notify();
                    })),
            );
        }
        v_flex()
            .gap(SM)
            .child(meta("Where is the hub?", cx))
            .child(locations)
            .child(meta("Space name", cx))
            .child(div().max_w(SPACE_NAME_WIDTH).child(field(&self.space_name)))
            .child(meta(
                if remote {
                    "Join code"
                } else {
                    "Hub data directory"
                },
                cx,
            ))
            .child(if remote {
                field(&self.join_code).into_any_element()
            } else {
                h_flex()
                    .gap(SM)
                    .child(div().min_w_0().flex_1().child(field(&self.hub_dir)))
                    .child(
                        button("choose-hub-directory")
                            .label("Choose folder…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_hub_directory(window, cx)
                            })),
                    )
                    .into_any_element()
            })
            .children(self.join_error.as_ref().map(|error| {
                div()
                    .text_size(TYPE_BODY)
                    .text_color(cx.theme().danger)
                    .child(error.clone())
            }))
            .child(
                button("connect-space")
                    .primary()
                    .self_start()
                    .label("Connect")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if remote {
                            this.join_space(cx);
                        } else {
                            this.pair_directory(cx);
                        }
                    })),
            )
    }

    /// What the app shows while no hub is paired: how to pair a space
    /// (by its data directory or a join code) and start a hub for it.
    pub(crate) fn first_run(&self, cx: &mut Context<Self>) -> AnyElement {
        let command = first_run_hub_command(
            self.state.client_dir(),
            &self.space_name.read(cx).value(),
            &self.hub_dir.read(cx).value(),
        );
        let join_form = self.join_form(cx);
        let muted = cx.theme().muted_foreground;
        let step = |title: &'static str, text: &'static str| {
            card(cx).child(section(title)).child(meta(text, cx))
        };
        let content = v_flex()
            .w_full()
            .max_w(READING_WIDTH)
            .mx_auto()
            .pr(SCROLLBAR_GUTTER)
            .pb_4()
            .gap(XL)
            .child(heading("Pair this app with a hub"))
            .child(div().text_color(muted).child(
                "A hub keeps one space's projects, agents and memory. This app shows every \
                 hub it is paired with.",
            ))
            .child(step(FIRST_RUN_PAIR_STEP.0, FIRST_RUN_PAIR_STEP.1).child(join_form))
            .child(
                step(FIRST_RUN_START_STEP.0, FIRST_RUN_START_STEP.1).child(
                    div()
                        .p(SM)
                        .text_size(TYPE_BODY)
                        .font_family(cx.theme().mono_font_family.clone())
                        .bg(cx.theme().muted)
                        .rounded(ROW_RADIUS)
                        .child(command),
                ),
            );
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .id("first-run")
            .overflow_y_scrollbar()
            .child(content)
            .into_any_element()
    }
}

const SOCKET_FILE: &str = "hub.sock";

const FIRST_RUN_PAIR_STEP: (&str, &str) = (
    "1. Pair a space",
    "Name the space and connect. Leave the directory blank to create the suggested folder, \
     choose an existing hub folder, or use a join code from another computer.",
);

const FIRST_RUN_START_STEP: (&str, &str) = (
    "2. Start the hub",
    "If no hub is running in that folder yet, start one in a terminal. It keeps running there.",
);

/// Trims the directory field from the pairing form and expands a leading `~/`.
/// Whitespace-only input means "use the suggested space directory".
pub(crate) fn parse_hub_dir(
    raw: &str,
    home: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    let path = raw.trim();
    if path.is_empty() {
        return None;
    }
    // `~` is the shell's; expand it here as the shell would have.
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => Some(std::path::PathBuf::from(home).join(rest)),
        _ => Some(std::path::PathBuf::from(path)),
    }
}

fn shell_quote_path(path: &str) -> String {
    let safe = !path.is_empty()
        && path.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'/' | b'.' | b'_' | b'-' | b'~' | b':' | b'@' | b'+' | b'='
                )
        });
    if safe {
        return path.to_owned();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("~/'{}'", rest.replace('\'', "'\\''"));
    }
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn hub_run_command(dir: &str) -> String {
    format!("threadlane-hub --data-dir {} run", shell_quote_path(dir))
}

/// The command shown in Step 2 of the first-run screen so it matches the folder
/// Step 1 will pair: the explicit directory if entered, otherwise the suggested
/// `<client-root>/spaces/<name>` directory.
pub(crate) fn first_run_hub_command(
    client_dir: Option<&std::path::Path>,
    space_name: &str,
    hub_dir: &str,
) -> String {
    let entered = hub_dir.trim();
    if !entered.is_empty() {
        let path = std::path::Path::new(entered);
        let dir = if path.file_name() == Some(std::ffi::OsStr::new(SOCKET_FILE))
            && !parse_hub_dir(entered, std::env::var_os("HOME").as_deref())
                .as_deref()
                .unwrap_or(path)
                .is_dir()
        {
            match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.display().to_string(),
                _ => ".".to_owned(),
            }
        } else {
            entered.to_owned()
        };
        return hub_run_command(&dir);
    }
    if let Some(root) = client_dir {
        let dirs = threadlane_client::ClientDirs::new(root);
        let dir = dirs
            .space_dir(space_name.trim())
            .or_else(|_| dirs.space_dir("work"));
        if let Ok(dir) = dir {
            return hub_run_command(&dir.display().to_string());
        }
    }
    hub_run_command("~/threadlane/work")
}

/// Status message after pairing a local directory: if no hub is running there
/// yet, prints the command to start one.
pub(crate) fn paired_directory_message(
    name: &str,
    data_dir: Option<&std::path::Path>,
    hub_running: bool,
) -> String {
    if !hub_running && let Some(dir) = data_dir {
        return format!(
            "Paired {name}. Start a hub with: {}",
            hub_run_command(&dir.display().to_string())
        );
    }
    format!("Paired {name}.")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    use threadlane_client::{ClientCore, ClientDirs, ClientError, Reach};

    use super::{
        FIRST_RUN_PAIR_STEP, FIRST_RUN_START_STEP, SOCKET_FILE, first_run_hub_command,
        paired_directory_message, parse_hub_dir,
    };

    #[test]
    fn first_run_command_matches_the_suggested_or_entered_directory() {
        let client = Path::new("/Users/alice/.config/threadlane");
        assert_eq!(
            first_run_hub_command(Some(client), "", ""),
            "threadlane-hub --data-dir /Users/alice/.config/threadlane/spaces/work run"
        );
        assert_eq!(
            first_run_hub_command(Some(client), "personal", "   "),
            "threadlane-hub --data-dir /Users/alice/.config/threadlane/spaces/personal run"
        );
        assert_eq!(
            first_run_hub_command(Some(client), "..", ""),
            "threadlane-hub --data-dir /Users/alice/.config/threadlane/spaces/work run"
        );
        assert_eq!(
            first_run_hub_command(Some(client), "work", "  ~/threadlane/custom  "),
            "threadlane-hub --data-dir ~/threadlane/custom run"
        );
        assert_eq!(
            first_run_hub_command(Some(client), "work", "/srv/work/hub.sock"),
            "threadlane-hub --data-dir /srv/work run"
        );
        let spaced = Path::new("/Users/alice/Application Support/threadlane");
        assert_eq!(
            first_run_hub_command(Some(spaced), "work", ""),
            "threadlane-hub --data-dir '/Users/alice/Application Support/threadlane/spaces/work' run"
        );
        assert_eq!(
            first_run_hub_command(None, "work", ""),
            "threadlane-hub --data-dir ~/threadlane/work run"
        );
    }

    #[test]
    fn first_run_command_keeps_an_existing_directory_named_hub_sock() {
        let temp = tempfile::tempdir().unwrap();
        let sock_dir = temp.path().join(SOCKET_FILE);
        std::fs::create_dir(&sock_dir).unwrap();
        let entered = sock_dir.display().to_string();
        assert_eq!(
            first_run_hub_command(Some(temp.path()), "work", &entered),
            format!(
                "threadlane-hub --data-dir {} run",
                super::shell_quote_path(&entered)
            )
        );
    }

    #[test]
    fn first_run_documented_order_pairs_then_starts_hub_and_keeps_collision_protection() {
        assert!(FIRST_RUN_PAIR_STEP.0.starts_with("1."));
        assert!(FIRST_RUN_START_STEP.0.starts_with("2."));

        let temp = tempfile::tempdir().unwrap();
        let dirs = ClientDirs::new(temp.path().join("client"));
        let mut core = ClientCore::load(dirs.clone()).unwrap();

        // Step 1: pair with a blank directory to create the suggested folder.
        let parsed = parse_hub_dir("   ", None);
        assert_eq!(parsed, None);
        core.pair_directory("work", parsed).unwrap();

        let suggested = dirs.space_dir("work").unwrap();
        assert!(suggested.is_dir());
        let expected_cmd = first_run_hub_command(Some(&dirs.root), "work", "");
        assert_eq!(
            paired_directory_message("work", Some(&suggested), false),
            format!("Paired work. Start a hub with: {expected_cmd}")
        );

        // Step 2: start the hub in that folder (which creates hub.sock inside it).
        std::fs::create_dir_all(&suggested).unwrap();
        let socket = suggested.join(SOCKET_FILE);
        let _listener = UnixListener::bind(&socket).unwrap();
        assert_eq!(core.hubs().len(), 1);
        assert_eq!(core.hubs()[0].reach, Reach::Socket { socket });

        // Implicit pairing still refuses an already-existing suggested folder.
        let taken = dirs.space_dir("taken").unwrap();
        std::fs::create_dir_all(&taken).unwrap();
        assert!(matches!(
            core.pair_directory("taken", None),
            Err(ClientError::DirectoryTaken(path)) if path == taken
        ));
    }

    #[test]
    fn paired_directory_message_prompts_to_start_a_hub_when_none_is_running() {
        let dir = Path::new("/Users/alice/.config/threadlane/spaces/work");
        assert_eq!(
            paired_directory_message("work", Some(dir), false),
            "Paired work. Start a hub with: threadlane-hub --data-dir /Users/alice/.config/threadlane/spaces/work run"
        );
        assert_eq!(
            paired_directory_message("work", Some(dir), true),
            "Paired work."
        );
        assert_eq!(
            paired_directory_message("work", None, false),
            "Paired work."
        );
    }

    #[test]
    fn parse_hub_dir_trims_whitespace_and_expands_home() {
        let home = Some(OsStr::new("/Users/alice"));
        assert_eq!(parse_hub_dir("", home), None);
        assert_eq!(parse_hub_dir("   \t\n ", home), None);
        assert_eq!(
            parse_hub_dir("  ~/threadlane/work  ", home),
            Some(PathBuf::from("/Users/alice/threadlane/work"))
        );
        assert_eq!(
            parse_hub_dir("  /srv/my space/hub  ", home),
            Some(PathBuf::from("/srv/my space/hub"))
        );
    }
}
