//! Local shell fixtures for shared terminal chrome; no PTY or command execution.
use gpui::{prelude::*, *};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, Disableable, Selectable, Sizable, WindowExt};
use threadlane_ui_kit::{self as kit, TerminalAction, TerminalTabAction};

pub enum TerminalPreviewEvent {
    Hide,
    AddSelection(String),
}

struct SampleShell {
    id: u64,
    output: String,
    find: Option<SampleFind>,
    link_epoch: u64,
    screen: vt100::Screen,
    screen_revision: u64,
    cue_text: Option<String>,
    cue_row: Option<u16>,
}

struct SampleFind {
    input: Entity<InputState>,
    query: String,
    hits: Vec<(usize, String)>,
    selected: Option<usize>,
    previous_focus: Option<FocusHandle>,
    _subscription: Subscription,
    mode: SampleSearchMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SampleSearchMode {
    Live,
    Searching,
    Failed,
    FullScreen,
}

impl SampleFind {
    fn status(&self) -> kit::TerminalFindStatus {
        match self.mode {
            SampleSearchMode::Searching => kit::TerminalFindStatus::Searching,
            SampleSearchMode::Failed => kit::TerminalFindStatus::Failed,
            SampleSearchMode::FullScreen => kit::TerminalFindStatus::Unavailable,
            SampleSearchMode::Live if self.query.is_empty() => kit::TerminalFindStatus::Empty,
            SampleSearchMode::Live => {
                kit::TerminalFindStatus::results(self.hits.len(), self.hits.len(), self.selected)
            }
        }
    }
}

pub struct TerminalPreview {
    project: String,
    shells: Vec<SampleShell>,
    selected: u64,
    next_id: u64,
    close_armed: Option<u64>,
    selection: bool,
    selection_anchor: Option<(u16, u16)>,
    selection_head: Option<(u16, u16)>,
    geometry: Option<kit::TerminalGridGeometry>,
    link_press: Option<(u64, u64, String, Point<Pixels>)>,
    context_link: Option<(u64, u64, String)>,
    font_size: f32,
    compact: bool,
    blend: bool,
    status: Option<(String, bool)>,
    unavailable: bool,
    feedback: Option<String>,
    focus: FocusHandle,
    link_menu: Option<Entity<PopupMenu>>,
    _link_subscription: Option<Subscription>,
}

impl EventEmitter<TerminalPreviewEvent> for TerminalPreview {}

impl TerminalPreview {
    pub fn new(project: String, cx: &mut Context<Self>) -> Self {
        Self {
            project,
            shells: vec![
                SampleShell {
                    id: 1,
                    output: sample_output(),
                    find: None,
                    link_epoch: 0,
                    screen: sample_screen(&sample_output(), 30, 120),
                    screen_revision: 0,
                    cue_text: None,
                    cue_row: None,
                },
                SampleShell {
                    id: 2,
                    output: sample_output(),
                    find: None,
                    link_epoch: 0,
                    screen: sample_screen(&sample_output(), 30, 120),
                    screen_revision: 0,
                    cue_text: None,
                    cue_row: None,
                },
            ],
            selected: 1,
            next_id: 3,
            close_armed: None,
            selection: false,
            selection_anchor: None,
            selection_head: None,
            geometry: None,
            link_press: None,
            context_link: None,
            font_size: kit::TERMINAL_FONT_SIZE,
            compact: false,
            blend: false,
            status: None,
            unavailable: false,
            feedback: None,
            focus: cx.focus_handle(),
            link_menu: None,
            _link_subscription: None,
        }
    }

    fn sample_link_at(&self, position: Point<Pixels>) -> Option<String> {
        let cell = self.geometry.as_ref()?.link_cell_at(position)?;
        let shell = self.shells.iter().find(|shell| shell.id == self.selected)?;
        sample_grid_links(&shell.screen)
            .into_iter()
            .find(|(_, cells)| cells.contains(&cell))
            .map(|(url, _)| url)
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        if !self.selection {
            return None;
        }
        let shell = self.shells.iter().find(|shell| shell.id == self.selected)?;
        kit::terminal_selected_excerpt(
            &shell.screen,
            self.selection_anchor,
            self.selection_head,
            shell.screen.size().1,
        )
    }

    fn select_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.selection_head = self
            .geometry
            .as_ref()
            .and_then(|geometry| geometry.cell_at(position));
        let cols = self
            .shells
            .iter()
            .find(|shell| shell.id == self.selected)
            .map_or(0, |shell| shell.screen.size().1);
        self.selection =
            kit::terminal_selection_present(self.selection_anchor, self.selection_head, cols);
        cx.notify();
    }

    fn update_grid(
        &mut self,
        bounds: Bounds<Pixels>,
        metrics: kit::TerminalTextMetrics,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let size = metrics.grid_size(bounds.size, window);
        self.geometry = Some(kit::TerminalGridGeometry::new(
            bounds,
            size,
            metrics.cell_width(),
            metrics.row_height(),
            metrics.inset(window),
        ));
        if let Some(shell) = self
            .shells
            .iter_mut()
            .find(|shell| shell.id == self.selected)
        {
            let cue = shell
                .find
                .as_ref()
                .and_then(|find| find.selected.and_then(|index| find.hits.get(index)))
                .map(|(_, text)| text.clone());
            if shell.screen.size() != size
                || shell.screen_revision != shell.link_epoch
                || shell.cue_text != cue
            {
                let (screen, row) =
                    sample_visible_screen(&shell.output, size.0, size.1, cue.as_deref());
                shell.screen = screen;
                shell.cue_text = cue;
                shell.cue_row = row;
                shell.screen_revision = shell.link_epoch;
                self.link_press = None;
                self.selection = false;
                self.selection_anchor = None;
                self.selection_head = None;
                cx.notify();
            }
        }
    }

    fn selection_available(&self) -> bool {
        !self.unavailable
            && self
                .shells
                .iter()
                .any(|shell| shell.id == self.selected && !shell.output.is_empty())
    }

    fn new_tab(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        self.shells.push(SampleShell {
            id,
            output: String::new(),
            find: None,
            link_epoch: 0,
            screen: sample_screen("", 30, 120),
            screen_revision: 0,
            cue_text: None,
            cue_row: None,
        });
        self.selected = id;
        self.selection = false;
    }

    fn request_tab(&mut self, id: u64, action: TerminalTabAction, cx: &mut Context<Self>) {
        let Some(ix) = self.shells.iter().position(|shell| shell.id == id) else {
            return;
        };
        match action {
            TerminalTabAction::Select => {
                self.selected = id;
                self.selection = false;
            }
            TerminalTabAction::Close => {
                if !self.shells[ix].output.is_empty() && self.close_armed != Some(id) {
                    self.close_armed = Some(id);
                    cx.notify();
                    return;
                }
                self.close_armed = None;
                self.shells.remove(ix);
                if self.shells.is_empty() {
                    self.new_tab();
                    cx.emit(TerminalPreviewEvent::Hide);
                } else if self.selected == id {
                    self.selected = self.shells[ix.min(self.shells.len() - 1)].id;
                }
                self.selection = false;
            }
            TerminalTabAction::CloseOthers => {
                self.shells.retain(|shell| shell.id == id);
                self.selected = id;
                self.selection = false;
            }
            TerminalTabAction::Restart => {
                self.status = None;
                self.shells[ix].output = sample_output();
                self.shells[ix].link_epoch += 1;
                self.feedback = Some("Sample shell reset locally.".into());
                self.selection = false;
            }
            TerminalTabAction::NewTab => self.new_tab(),
        }
        self.refresh_find(id);
        cx.notify();
    }

    pub(super) fn request(&mut self, action: TerminalAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            TerminalAction::Hide => cx.emit(TerminalPreviewEvent::Hide),
            TerminalAction::NewTab => self.new_tab(),
            TerminalAction::Clear => {
                if let Some(shell) = self
                    .shells
                    .iter_mut()
                    .find(|shell| shell.id == self.selected)
                {
                    shell.output.clear();
                    shell.link_epoch += 1;
                }
                self.selection = false;
            }
            TerminalAction::Restart => {
                self.request_tab(self.selected, TerminalTabAction::Restart, cx)
            }
            TerminalAction::Find => self.open_find(window, cx),
            TerminalAction::OpenLinks => self.open_links(window, cx),
            TerminalAction::AddSelectionToChat if self.selection && self.selection_available() => {
                if let Some(text) = self.selected_text() {
                    cx.emit(TerminalPreviewEvent::AddSelection(format!(
                        "Sample terminal selection:\n\n```text\n{text}\n```"
                    )));
                }
                self.feedback =
                    Some("Sample selection added to the local draft. Nothing was sent.".into());
            }
            TerminalAction::RecreateWorktree | TerminalAction::UseProjectFolder => {
                self.unavailable = false;
                self.feedback = Some("Sample checkout made available locally.".into());
            }
            _ => {}
        }
        self.refresh_find(self.selected);
        cx.notify();
    }
}

impl TerminalPreview {
    fn refresh_find(&mut self, id: u64) {
        let Some(shell) = self.shells.iter_mut().find(|shell| shell.id == id) else {
            return;
        };
        let Some(find) = &mut shell.find else {
            return;
        };
        let selected = find
            .selected
            .and_then(|index| find.hits.get(index))
            .cloned();
        find.hits = if find.query.is_empty() {
            Vec::new()
        } else {
            sample_plain_output(&shell.output)
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains(&find.query))
                .map(|(row, line)| (row, line.to_owned()))
                .collect()
        };
        find.selected =
            selected.and_then(|selected| find.hits.iter().position(|hit| *hit == selected));
    }

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            cx.propagate();
            return;
        }
        if self.unavailable {
            return;
        }
        let id = self.selected;
        let Some(shell) = self.shells.iter_mut().find(|shell| shell.id == id) else {
            return;
        };
        if shell.find.is_none() {
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder("Find in retained output…"));
            let subscription = cx.subscribe(&input, move |host, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Some(find) = host
                        .shells
                        .iter_mut()
                        .find(|shell| shell.id == id)
                        .and_then(|shell| shell.find.as_mut())
                    {
                        find.query = input.read(cx).value().to_string();
                        find.selected = None;
                    }
                    host.refresh_find(id);
                    cx.notify();
                }
            });
            shell.find = Some(SampleFind {
                input,
                query: String::new(),
                hits: Vec::new(),
                selected: None,
                previous_focus: window.focused(cx),
                _subscription: subscription,
                mode: SampleSearchMode::Live,
            });
        }
        shell.find.as_ref().unwrap().input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn request_find(
        &mut self,
        action: kit::TerminalFindAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            cx.propagate();
            return;
        }
        let Some(shell) = self
            .shells
            .iter_mut()
            .find(|shell| shell.id == self.selected)
        else {
            return;
        };
        if action == kit::TerminalFindAction::Close {
            if let Some(find) = shell.find.take() {
                find.previous_focus
                    .unwrap_or_else(|| self.focus.clone())
                    .focus(window, cx);
            }
        } else if let Some(find) = &mut shell.find {
            match action {
                kit::TerminalFindAction::Next | kit::TerminalFindAction::Previous
                    if find.status().can_navigate() =>
                {
                    find.selected = kit::next_terminal_find_match(
                        find.selected,
                        find.hits.len(),
                        action == kit::TerminalFindAction::Previous,
                    );
                }
                kit::TerminalFindAction::Retry => {
                    find.mode = SampleSearchMode::Live;
                    self.refresh_find(self.selected);
                }
                _ => {}
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn render_find(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let find = self
            .shells
            .iter()
            .find(|shell| shell.id == self.selected)?
            .find
            .as_ref()?;
        let status = find.status();
        let owner = cx.weak_entity();
        Some(
            kit::TerminalFindStrip::new(&find.input, status)
                .excerpt(
                    find.selected
                        .and_then(|index| find.hits.get(index))
                        .map(|(_, line)| line.clone().into()),
                )
                .render(
                    move |action, window, cx| {
                        let _ = owner.update(cx, |host, cx| host.request_find(action, window, cx));
                    },
                    cx,
                )
                .into_any_element(),
        )
    }

    fn choose_link(
        &mut self,
        id: u64,
        epoch: u64,
        url: String,
        destination: kit::TerminalLinkDestination,
        cx: &mut Context<Self>,
    ) {
        if self.selected == id
            && self
                .shells
                .iter()
                .any(|shell| shell.id == id && shell.link_epoch == epoch)
        {
            self.feedback = Some(format!(
                "Sample link choice: {url} · {}. Nothing was opened.",
                if destination == kit::TerminalLinkDestination::Threadlane {
                    "Threadlane browser"
                } else {
                    "default browser"
                }
            ));
            cx.notify();
        }
    }

    fn open_links(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.unavailable {
            return;
        }
        let Some(shell) = self.shells.iter().find(|shell| shell.id == self.selected) else {
            return;
        };
        let id = shell.id;
        let epoch = shell.link_epoch;
        let urls = shell
            .output
            .lines()
            .filter(|line| line.starts_with("https://") || line.starts_with("http://"))
            .map(str::to_owned);
        let picker = kit::TerminalLinkPicker::new(urls).in_app_browser(true);
        let owner = cx.weak_entity();
        self.focus.focus(window, cx);
        let menu = PopupMenu::build(window, cx, |menu, _, _| {
            picker.render(
                menu.action_context(self.focus.clone()),
                move |url, destination, _, cx| {
                    let _ = owner.update(cx, |host, cx| {
                        host.choose_link(id, epoch, url, destination, cx);
                    });
                },
            )
        });
        self._link_subscription = Some(cx.subscribe(&menu, |host, _, _: &DismissEvent, cx| {
            host.link_menu = None;
            cx.notify();
        }));
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.link_menu = Some(menu);
        cx.notify();
    }
}

impl Render for TerminalPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let pane = if self.unavailable {
            kit::terminal_unavailable(
                move |action, window, cx| {
                    let _ = owner.update(cx, |host, cx| host.request(action, window, cx));
                },
                cx,
            )
        } else {
            let tabs = self
                .shells
                .iter()
                .enumerate()
                .map(|(ix, shell)| {
                    let id = shell.id;
                    let owner = cx.entity().downgrade();
                    kit::TerminalTab::new(
                        format!("sample-shell-{id}"),
                        format!("Shell {}", ix + 1),
                        format!("Shell {} · {}", ix + 1, self.project),
                    )
                    .selected(id == self.selected)
                    .close_armed(self.close_armed == Some(id))
                    .close_others(self.shells.len() > 1)
                    .render(move |action, _, cx| {
                        let _ = owner.update(cx, |host, cx| host.request_tab(id, action, cx));
                    })
                    .into_any_element()
                })
                .collect();
            let toolbar = kit::TerminalToolbar::new(self.project.clone())
                .tabs(tabs)
                .selection(
                    self.selection,
                    if self.selection {
                        "Add the sample terminal text to the local draft — nothing is sent"
                    } else {
                        "Select terminal output first"
                    },
                )
                .render(
                    move |action, window, cx| {
                        let _ = owner.update(cx, |host, cx| host.request(action, window, cx));
                    },
                    cx,
                );
            let shell = self
                .shells
                .iter()
                .find(|shell| shell.id == self.selected)
                .unwrap();
            let metrics =
                kit::TerminalTextMetrics::measure(self.font_size, self.compact, window, cx);
            let links = sample_grid_links(&shell.screen);
            let status = self.status.clone().map(|(message, is_error)| {
                let owner = cx.entity().downgrade();
                kit::terminal_status(
                    message,
                    is_error,
                    move |window, cx| {
                        let _ = owner.update(cx, |host, cx| {
                            host.request(TerminalAction::Restart, window, cx)
                        });
                    },
                    cx,
                )
            });
            let grid =
                kit::TerminalGrid::new("sample-terminal-grid", &shell.screen, metrics.clone())
                    .cursor(self.focus.is_focused(window))
                    .selection(
                        self.selection.then_some(self.selection_anchor).flatten(),
                        self.selection.then_some(self.selection_head).flatten(),
                    )
                    .find_cue(shell.cue_row)
                    .links(
                        links
                            .iter()
                            .map(|(url, cells)| (url.as_str(), cells.as_slice())),
                    )
                    .on_layout({
                        let owner = cx.weak_entity();
                        move |bounds, window, cx| {
                            let _ = owner.update(cx, |host, cx| {
                                host.update_grid(bounds, metrics.clone(), window, cx)
                            });
                        }
                    })
                    .render(cx)
                    .debug_selector(|| "sample-terminal-grid".into())
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|host, event: &MouseDownEvent, _, _| {
                            host.context_link =
                                host.sample_link_at(event.position).and_then(|url| {
                                    host.shells
                                        .iter()
                                        .find(|shell| shell.id == host.selected)
                                        .map(|shell| (shell.id, shell.link_epoch, url))
                                });
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|host, event: &MouseDownEvent, window, cx| {
                            host.link_press = if (event.modifiers.control
                                || event.modifiers.platform)
                                && event.click_count == 1
                            {
                                host.sample_link_at(event.position).and_then(|url| {
                                    host.shells
                                        .iter()
                                        .find(|shell| shell.id == host.selected)
                                        .map(|shell| {
                                            (shell.id, shell.link_epoch, url, event.position)
                                        })
                                })
                            } else {
                                None
                            };
                            host.selection_anchor = host
                                .geometry
                                .as_ref()
                                .and_then(|geometry| geometry.cell_at(event.position));
                            host.selection_head = host.selection_anchor;
                            host.selection = false;
                            host.focus.focus(window, cx);
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|host, event: &MouseMoveEvent, _, cx| {
                        if host.link_press.as_ref().is_some_and(|(_, _, _, start)| {
                            (event.position.x - start.x).abs() > px(3.)
                                || (event.position.y - start.y).abs() > px(3.)
                        }) {
                            host.link_press = None;
                        }
                        if event.dragging() && host.selection_anchor.is_some() {
                            host.select_to(event.position, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|host, event: &MouseUpEvent, _, cx| {
                            if let Some((id, epoch, url, start)) = host.link_press.take() {
                                if (event.modifiers.control || event.modifiers.platform)
                                    && id == host.selected
                                    && host
                                        .shells
                                        .iter()
                                        .any(|shell| shell.id == id && shell.link_epoch == epoch)
                                    && (event.position.x - start.x).abs() <= px(3.)
                                    && (event.position.y - start.y).abs() <= px(3.)
                                    && host.sample_link_at(event.position).as_ref() == Some(&url)
                                {
                                    host.feedback = Some(format!(
                                        "Sample link choice: {url}. Nothing was opened."
                                    ));
                                    host.selection = false;
                                    cx.notify();
                                    return;
                                }
                            }
                            host.select_to(event.position, cx);
                        }),
                    )
                    .context_menu({
                        let owner = cx.entity();
                        move |mut menu, window, cx| {
                            let host = owner.read(cx);
                            let selection = host.selected_text();
                            let output = host
                                .shells
                                .iter()
                                .find(|shell| shell.id == host.selected)
                                .map(|shell| shell.screen.contents())
                                .unwrap_or_default();
                            let presentation = kit::TerminalOutputMenu::new(host.font_size)
                                .compact(host.compact)
                                .blend(host.blend)
                                .selection(selection.is_some())
                                .paste(false);
                            if let Some((id, epoch, url)) = host.context_link.clone() {
                                let target = owner.clone();
                                menu = kit::terminal_link_commands(
                                    menu,
                                    url,
                                    true,
                                    move |url, destination, _, cx| {
                                        target.update(cx, |host, cx| {
                                            host.choose_link(id, epoch, url, destination, cx)
                                        });
                                    },
                                )
                                .separator();
                            }
                            let target = owner.clone();
                            presentation.render(
                                menu,
                                move |action, window, cx| {
                                    target.update(cx, |host, cx| {
                                        use kit::TerminalOutputAction as Action;
                                        match action {
                                            Action::CopySelection => {
                                                if let Some(text) = &selection {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(text.clone()),
                                                    );
                                                }
                                            }
                                            Action::CopyOutput => cx.write_to_clipboard(
                                                ClipboardItem::new_string(output.clone()),
                                            ),
                                            Action::FontSize(size) => host.font_size = size,
                                            Action::ToggleCompact => host.compact = !host.compact,
                                            Action::ToggleBackground => host.blend = !host.blend,
                                            Action::Find => host.open_find(window, cx),
                                            Action::SelectAll => {
                                                if let Some(shell) = host
                                                    .shells
                                                    .iter()
                                                    .find(|shell| shell.id == host.selected)
                                                {
                                                    let (rows, cols) = shell.screen.size();
                                                    host.selection_anchor = Some((0, 0));
                                                    host.selection_head =
                                                        Some((rows.saturating_sub(1), cols));
                                                    host.selection = true;
                                                }
                                            }
                                            Action::Clear => {
                                                host.request(TerminalAction::Clear, window, cx)
                                            }
                                            Action::Restart => {
                                                host.request(TerminalAction::Restart, window, cx)
                                            }
                                            Action::Paste => {} // This fixture has no PTY; the menu disables Paste.
                                        }
                                        host.link_press = None;
                                        cx.notify();
                                    });
                                },
                                window,
                                cx,
                            )
                        }
                    });
            kit::terminal_surface(cx).child(toolbar).child(
                kit::terminal_output_surface("sample-terminal-output", self.blend, cx)
                    .debug_selector(|| "sample-terminal-output".into())
                    .track_focus(&self.focus)
                    .children(self.render_find(cx))
                    .child(grid)
                    .children(status),
            )
        };
        div()
            .id("sample-terminal")
            .role(Role::Group)
            .key_context("Terminal")
            .on_action(
                cx.listener(|host, _: &kit::FindInTerminalOutput, window, cx| {
                    host.open_find(window, cx)
                }),
            )
            .on_action(cx.listener(|host, _: &kit::CloseTerminalFind, window, cx| {
                host.request_find(kit::TerminalFindAction::Close, window, cx)
            }))
            .on_action(cx.listener(|host, _: &kit::NextTerminalMatch, window, cx| {
                host.request_find(kit::TerminalFindAction::Next, window, cx)
            }))
            .on_action(
                cx.listener(|host, _: &kit::PreviousTerminalMatch, window, cx| {
                    host.request_find(kit::TerminalFindAction::Previous, window, cx)
                }),
            )
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(pane))
            .children(self.link_menu.as_ref().map(kit::terminal_link_overlay))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Sample output · No shell commands run"),
                    )
                    .child(
                        Button::new("terminal-preview-selection")
                            .debug_selector(|| "terminal-preview-selection".into())
                            .label("Sample selection")
                            .ghost()
                            .xsmall()
                            .selected(self.selection)
                            .disabled(!self.selection_available())
                            .on_click(cx.listener(|host, _, _, cx| {
                                if host.selection_available() {
                                    host.selection = !host.selection;
                                    if host.selection {
                                        if let Some(shell) = host
                                            .shells
                                            .iter()
                                            .find(|shell| shell.id == host.selected)
                                        {
                                            let text = shell.screen.contents();
                                            if let Some((row, line)) = text
                                                .lines()
                                                .enumerate()
                                                .find(|(_, line)| line.contains("Finished"))
                                            {
                                                host.selection_anchor = Some((row as u16, 0));
                                                host.selection_head =
                                                    Some((row as u16, line.chars().count() as u16));
                                            }
                                        }
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("terminal-preview-unavailable")
                            .debug_selector(|| "terminal-preview-unavailable".into())
                            .label("Unavailable checkout")
                            .ghost()
                            .xsmall()
                            .selected(self.unavailable)
                            .on_click(cx.listener(|host, _, _, cx| {
                                host.unavailable = !host.unavailable;
                                host.link_menu = None;
                                host._link_subscription = None;
                                host.selection = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("terminal-preview-output-state")
                            .label("Output state")
                            .ghost()
                            .xsmall()
                            .dropdown_menu({
                                let owner = cx.weak_entity();
                                move |mut menu, _, _| {
                                    for (label, state) in [
                                        ("Live shell", None),
                                        (
                                            "Shell exited",
                                            Some(("Shell exited — output is retained", false)),
                                        ),
                                        (
                                            "Read failed",
                                            Some(("Terminal read failed: sample error", true)),
                                        ),
                                    ] {
                                        let owner = owner.clone();
                                        menu = menu.item(PopupMenuItem::new(label).on_click(
                                            move |_, _, cx| {
                                                let _ = owner.update(cx, |host, cx| {
                                                    host.status =
                                                        state.map(|(message, is_error)| {
                                                            (message.into(), is_error)
                                                        });
                                                    cx.notify();
                                                });
                                            },
                                        ));
                                    }
                                    menu
                                }
                            }),
                    )
                    .children(
                        self.shells
                            .iter()
                            .find(|shell| shell.id == self.selected)
                            .and_then(|shell| shell.find.as_ref())
                            .map(|find| {
                                let id = self.selected;
                                let owner = cx.weak_entity();
                                let mode = find.mode;
                                Button::new("terminal-preview-search-state")
                                    .debug_selector(|| "terminal-preview-search-state".into())
                                    .label("Sample search state")
                                    .ghost()
                                    .xsmall()
                                    .dropdown_menu_with_anchor(
                                        Anchor::BottomRight,
                                        move |menu, _, _| {
                                            let mut menu = menu;
                                            for (label, requested) in [
                                                ("Live sample", SampleSearchMode::Live),
                                                ("Searching", SampleSearchMode::Searching),
                                                ("Search failed", SampleSearchMode::Failed),
                                                ("Full-screen app", SampleSearchMode::FullScreen),
                                            ] {
                                                let owner = owner.clone();
                                                menu = menu.item(
                                                    PopupMenuItem::new(label)
                                                        .checked(mode == requested)
                                                        .on_click(move |_, _, cx| {
                                                            let _ = owner.update(cx, |host, cx| {
                                                                if let Some(find) = host
                                                                    .shells
                                                                    .iter_mut()
                                                                    .find(|shell| shell.id == id)
                                                                    .and_then(|shell| {
                                                                        shell.find.as_mut()
                                                                    })
                                                                {
                                                                    find.mode = requested;
                                                                    cx.notify();
                                                                }
                                                            });
                                                        }),
                                                );
                                            }
                                            menu
                                        },
                                    )
                            }),
                    ),
            )
            .children(self.feedback.clone().map(|feedback| {
                div()
                    .text_xs()
                    .px_2()
                    .pb_1()
                    .text_color(cx.theme().muted_foreground)
                    .child(feedback)
            }))
    }
}

// Link positions describe only the known, local sample URLs. Native hosts keep their URL guard.
fn sample_grid_links(screen: &vt100::Screen) -> Vec<(String, Vec<(u16, u16)>)> {
    let cols = screen.size().1;
    screen
        .rows(0, cols)
        .enumerate()
        .filter(|(_, line)| line.starts_with("https://") || line.starts_with("http://"))
        .map(|(row, url)| {
            let cells = (0..url.chars().count().min(cols as usize) as u16)
                .map(|col| (row as u16, col))
                .collect();
            (url, cells)
        })
        .collect()
}

fn sample_screen(output: &str, rows: u16, cols: u16) -> vt100::Screen {
    let mut parser = vt100::Parser::new(rows, cols, 100);
    parser.process(output.replace('\n', "\r\n").as_bytes());
    parser.screen().clone()
}

fn sample_visible_screen(
    output: &str,
    rows: u16,
    cols: u16,
    cue: Option<&str>,
) -> (vt100::Screen, Option<u16>) {
    let mut parser = vt100::Parser::new(rows, cols, 100);
    parser.process(output.replace('\n', "\r\n").as_bytes());
    let mut cue_row = None;
    if let Some(cue) = cue {
        // Bounded local fixture history; no native search worker or service is run.
        for offset in 0..=100 {
            parser.screen_mut().set_scrollback(offset);
            cue_row = parser
                .screen()
                .rows(0, cols)
                .position(|line| line == cue)
                .map(|row| row as u16);
            if cue_row.is_some() {
                break;
            }
        }
    }
    if cue_row.is_none() {
        parser.screen_mut().set_scrollback(0);
    }
    (parser.screen().clone(), cue_row)
}

fn sample_plain_output(output: &str) -> String {
    sample_screen(output, 30, 120).contents()
}

fn sample_output() -> String {
    "$ cargo check\n    \x1b[36mChecking\x1b[0m threadlane-ui-kit\n    \x1b[1;32mFinished\x1b[0m dev profile — 0 errors\n\nhttps://gpui-kit.com\n$ ".into()
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
