//! Machines (wireframe 7): which spaces this app is paired with, which
//! machines help in each and what each can run, the client keys each hub
//! admitted, a join code for another app, and unpairing this app. Pairing
//! another space has its own screen.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::*;
use threadlane_core::ids::MachineId;
use threadlane_desktop::{
    Consequence, Destination, EmptyRegion, MachineRun, Part, When, count_form, machine_row,
};

use super::{empty, gap_line, link};
use crate::components::{button, card, field, heading, meta, section, status_badge, status_dot};
use crate::icons::AppIcon;
use crate::tokens::{
    KEY_RADIUS, READING_WIDTH, ROW_RADIUS, SM, SPACE_NAME_WIDTH, Status, TYPE_BODY, TYPE_META, XL,
    XS,
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
                link("back-to-machines", "Machines".to_owned())
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
        let step = |name: &'static str, icon, slots: Option<u32>| {
            let space = space.to_owned();
            crate::components::icon_button(name, icon, name.into())
                .disabled(slots.is_none())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let Some(slots) = slots else { return };
                    let core = this.core.clone();
                    let space = space.clone();
                    this.run(
                        async move {
                            core.read()
                                .await
                                .set_machine_slots(Some(&space), id, slots)
                                .await
                        },
                        |this, result, cx| match result {
                            Ok(()) => this.refresh(cx),
                            Err(error) => this.state.report(threadlane_desktop::Activity::Failed(
                                threadlane_desktop::describe(&error),
                            )),
                        },
                        cx,
                    );
                }))
        };
        page = page.child(
            h_flex()
                .gap(XS)
                .items_center()
                .child(step(
                    "Fewer run slots",
                    AppIcon::Minus,
                    (detail.slots > 1).then(|| detail.slots - 1),
                ))
                .child(meta(detail.slots.to_string(), cx))
                .child(step(
                    "More run slots",
                    AppIcon::Plus,
                    (detail.slots < detail.runner_slots).then(|| detail.slots + 1),
                )),
        );
        page = if let Some(value) = detail.occupancy {
            page.child(meta(report.load.clone(), cx)).child(
                Progress::new("machine-occupancy")
                    .value(value)
                    .accessibility_label(report.load),
            )
        } else {
            page.child(meta("This machine has not reported its run slots.", cx))
        };
        page = page.child(meta("To raise the runner ceiling, increase runner.max_parallel on this machine and restart its runner.", cx));
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
            .child(heading("Machines"));
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
        screen.into_any_element()
    }

    /// The pairing screen the space switcher opens.
    pub(crate) fn add_space(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .overflow_y_scrollbar()
            .id("add-space")
            .gap(SM)
            .child(heading("Add space"))
            .child(card(cx).child(self.join_form(cx)))
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
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(READING_WIDTH)
            .self_center()
            .overflow_y_scrollbar()
            .id("first-run")
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
            )
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
