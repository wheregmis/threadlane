//! Local palette host: captured conversation search and preview navigation only.
use super::SessionPreview;
use gpui::{prelude::*, *};
use gpui_component::{command::CommandGroup, IconName, WindowExt};
use threadlane_ui_kit as kit;

actions!(ui_kit_preview_palette, [TogglePreviewPalette]);
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", TogglePreviewPalette, None),
        KeyBinding::new("ctrl-k", TogglePreviewPalette, None),
    ]);
}

impl SessionPreview {
    pub(super) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_open {
            self.close_palette(window, cx);
        } else {
            self.palette_previous_focus = window.focused(cx);
            self.palette_search = false;
            self.palette_sessions = false;
            self.palette_open = true;
            self.palette_matches.clear();
            self.command_state.update(cx, |state, cx| {
                state.set_query("", window, cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = false;
        self.palette_search = false;
        self.palette_sessions = false;
        self.palette_matches.clear();
        if let Some(focus) = self.palette_previous_focus.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn palette_action(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if key != "go_task" {
            self.palette_recent.retain(|item| *item != key);
            self.palette_recent.insert(0, key);
            self.palette_recent.truncate(5);
        }
        match key {
            "go_task" | "search_conversations" => {
                self.palette_previous_focus = window.focused(cx);
                self.palette_open = true;
                self.palette_search = key == "search_conversations";
                self.palette_sessions = key == "go_task";
                self.command_state.update(cx, |state, cx| {
                    state.set_query("", window, cx);
                    state.focus(window, cx);
                });
            }
            "settings" => { self.gallery_open = false; self.settings_open = true; }
            "github" | "open_issue" => {
                self.gallery_open = false; self.settings_open = false;
                self.automations_open = false; self.github_open = Some(false);
            }
            "automations" => {
                self.gallery_open = false; self.settings_open = false;
                self.github_open = None; self.automations_open = true;
            }
            "panel" => self.agents_open = !self.agents_open,
            "sidebar" => self.sidebar_collapsed = !self.sidebar_collapsed,
            "run_terminal" => self.terminal_open = !self.terminal_open,
            "open_terminal_link" | "add_terminal_selection" => {
                let action = if key == "open_terminal_link" { kit::TerminalAction::OpenLinks }
                    else { kit::TerminalAction::AddSelectionToChat };
                self.terminal.update(cx, |terminal, cx| terminal.request(action, window, cx));
            }
            "open_file" => {
                self.gallery_open = false; self.settings_open = false;
                self.github_open = None; self.automations_open = false;
                self.editor_open = true;
                self.editor.update(cx, |editor, cx| editor.open_sample(cx));
            }
            "ask_agent" | "goal" | "model" | "compact" => {
                self.gallery_open = false; self.settings_open = false;
                self.github_open = None; self.automations_open = false; self.editor_open = false;
                self.input.update(cx, |input, cx| {
                    if key != "ask_agent" { input.set_value(format!("/{key} "), window, cx); }
                    input.focus(window, cx);
                });
            }
            _ => window.push_notification("This command uses desktop services. The local preview keeps the captured session unchanged.", cx),
        }
        if !self.chat_is_active() { self.clear_conversation_find(); self.clear_prompt_navigation(); }
        cx.notify();
    }

    pub(super) fn render_palette(&self, cx: &mut Context<Self>) -> AnyElement {
        let view_cancel = cx.weak_entity();
        let view_confirm = cx.weak_entity();
        let view_backdrop = cx.weak_entity();
        let command = if self.palette_sessions {
            let searching = !self.command_state.read(cx).query(cx).trim().is_empty();
            let query_view = cx.weak_entity();
            let clear_view = cx.weak_entity();
            kit::session_switcher_command(
                &self.command_state,
                vec![kit::palette_session_item("Review navigation", "Threadlane", Some("review/session-switcher"), "review"),
                     kit::palette_session_item("会話の切り替えを検証する", "Desktop", Some("feature/長いブランチ名"), "unicode")],
                vec![kit::palette_session_item("Implement navigation", "Threadlane", Some("feature/session-switcher"), "implementation")],
                searching, "Deterministic preview sessions · no saved session is changed",
            )
            .on_query(move |_, _, cx| { let _ = query_view.update(cx, |_, cx| cx.notify()); })
            .empty(move |_, _, cx| {
                let clear = clear_view.clone();
                kit::session_switcher_empty(searching, move |window, cx| {
                    let _ = clear.update(cx, |host, cx| {
                        host.command_state.update(cx, |state, cx| state.set_query("", window, cx));
                        cx.notify();
                    });
                }, cx)
            })
            .on_confirm(move |_, window, cx| {
                let _ = view_confirm.update(cx, |host, cx| {
                    host.close_palette(window, cx);
                    window.push_notification("Preview session selected. The captured session is unchanged.", cx);
                });
            })
        } else if self.palette_search {
            let query_hint = self.command_state.read(cx).query(cx).trim().chars().count() < 2;
            let mut results = CommandGroup::new().label("Conversations");
            for hit in &self.palette_matches {
                results = results.item(kit::palette_conversation_item(
                    self.session.title.clone(),
                    self.session
                        .git_branch
                        .clone()
                        .unwrap_or_else(|| "Captured session".into()),
                    hit.excerpt.clone(),
                ));
            }
            let scope = format!(
                "Conversations in {} · saved user and assistant messages",
                self.session
                    .work_dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            let footer = if query_hint {
                "Type at least 2 characters · 1 captured session".to_string()
            } else {
                format!(
                    "{} matching {} · 1 captured session · Local preview",
                    self.palette_matches.len(),
                    if self.palette_matches.len() == 1 {
                        "message"
                    } else {
                        "messages"
                    }
                )
            };
            let view_query = cx.weak_entity();
            let matches = self.palette_matches.clone();
            kit::workspace_palette_command(&self.command_state)
                .placeholder("Search messages in this project's conversations…")
                .filterable(false)
                .header(move |_, _, cx| kit::palette_scope(scope.clone(), cx))
                .group(results)
                .empty(move |_, _, cx| {
                    kit::palette_empty(
                        if query_hint {
                            "Type at least 2 characters to search saved conversations"
                        } else {
                            "No matching conversations"
                        },
                        cx,
                    )
                })
                .footer(move |_, _, cx| kit::palette_footer(footer.clone(), cx))
                .on_query(move |query, _, cx| {
                    let _ = view_query.update(cx, |host, cx| {
                        host.palette_matches = if query.trim().chars().count() < 2 {
                            Vec::new()
                        } else {
                            kit::transcript::find_conversation_messages(
                                &host.messages,
                                false,
                                query.trim(),
                            )
                        };
                        cx.notify();
                    });
                })
                .on_confirm(move |index, window, cx| {
                    let _ = view_confirm.update(cx, |host, cx| {
                        let hit = matches.get(index.row).filter(|hit| matches!(
                            host.transcript.rows.get(hit.row_index),
                            Some(kit::transcript::TranscriptRow::Message(message))
                                if host.messages.get(*message).is_some_and(|message| message.id == hit.message_id)
                        )).cloned();
                        let query = host.command_state.read(cx).query(cx).to_string();
                        host.close_palette(window, cx);
                        if let Some(hit) = hit {
                            host.gallery_open = false;
                            host.settings_open = false;
                            host.github_open = None;
                            host.automations_open = false;
                            host.editor_open = false;
                            host.agents_open = false;
                            host.seed_conversation_find(query, hit.message_id, window, cx);
                        }
                    });
                })
        } else {
            let commands = kit::workspace_commands();
            let selection = self
                .terminal_open
                .then(|| self.terminal.read(cx).selected_text())
                .flatten();
            let disabled_reason = |key: &str| {
                if key == "add_terminal_selection" {
                    if !self.terminal_open {
                        Some("No terminal is visible")
                    } else if selection.is_none() {
                        Some("Select text in the terminal first")
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            let recent_keys = self.palette_recent.clone();
            let mut recent = CommandGroup::new().label("Recently used");
            for key in &recent_keys {
                if let Some(command) = commands.iter().find(|command| command.key() == *key) {
                    recent = recent.item(command.item(disabled_reason(command.key())));
                }
            }
            let mut actions = CommandGroup::new().label("Commands & Actions");
            for command in &commands {
                actions = actions.item(command.item(disabled_reason(command.key())));
            }
            let settings_query = !self.command_state.read(cx).query(cx).trim().is_empty();
            let settings =
                CommandGroup::new()
                    .label("Settings")
                    .items(kit::SETTINGS_SEARCH_ITEMS.iter().map(|item| {
                        kit::palette_item(
                            item.title,
                            format!("Settings · {}", item.page),
                            IconName::Settings,
                        )
                        .keywords(item.keywords.iter().copied())
                    }));
            let mut sessions = CommandGroup::new().label("Sessions");
            if !self.sidebar_removed {
                sessions = sessions.item(
                    kit::palette_item(
                        self.session.title.clone(),
                        self.session
                            .work_dir
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        IconName::SquareTerminal,
                    )
                    .keywords([
                        self.session.id.clone(),
                        self.session.git_branch.clone().unwrap_or_default(),
                    ]),
                );
            }
            kit::workspace_palette_command(&self.command_state)
                .group(recent)
                .group(actions)
                .when(settings_query, |command| command.group(settings))
                .group(sessions)
                .on_confirm(move |index, window, cx| {
                    let _ = view_confirm.update(cx, |host, cx| {
                        host.close_palette(window, cx);
                        if index.section == 0 {
                            if let Some(key) = recent_keys.get(index.row) {
                                host.palette_action(key, window, cx);
                            }
                        } else if index.section == 1 {
                            if let Some(command) = commands.get(index.row) {
                                host.palette_action(command.key(), window, cx);
                            }
                        } else if settings_query && index.section == 2 {
                            if let Some(item) = kit::SETTINGS_SEARCH_ITEMS.get(index.row) {
                                host.gallery_open = false;
                                host.settings_open = true;
                                host.clear_conversation_find(); host.clear_prompt_navigation();
                                host.settings.update(cx, |settings, cx| {
                                    settings.open_search_destination(item.id, cx)
                                });
                            }
                        } else if index.section == if settings_query { 3 } else { 2 } {
                            host.gallery_open = false;
                            host.settings_open = false;
                            host.github_open = None;
                            host.automations_open = false;
                            host.editor_open = false;
                        }
                        cx.notify();
                    });
                })
        };
        let command = command.on_cancel(move |window, cx| {
            let _ = view_cancel.update(cx, |host, cx| host.close_palette(window, cx));
        });
        if self.palette_sessions {
            return kit::session_switcher_frame(command, move |window, cx| {
                let _ = view_backdrop.update(cx, |host, cx| host.close_palette(window, cx));
            }, cx).into_any_element();
        }
        kit::workspace_palette_frame(
            command,
            move |window, cx| {
                let _ = view_backdrop.update(cx, |host, cx| host.close_palette(window, cx));
            },
            cx,
        )
        .into_any_element()
    }
}
