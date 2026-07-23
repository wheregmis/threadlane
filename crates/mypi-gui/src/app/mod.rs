//! App shell: script_mod! DSL, startup/auth wiring, agent event pump.
//!
//! Chat, sessions, and command palette panels are modularized under `crate::panels`.

use crate::panels::chat::{
    accepts_generation_event, concise_status, draft_for_cancellation, submitted_draft, ChatList,
    ComposerState, ComposerStatus, GenerationEvent, ToolFoldHeader,
};
use crate::panels::command_palette::*;

use crate::panels::sessions::{
    ProjectRegistry, SessionContextMenu, SessionContextMenuAction, SessionList,
};
use crate::state::{
    active_session_entry, archive_session, builtin_commands, create_new_session, delete_session,
    is_project_working, is_session_working, project_work_dir_at_row, refresh_sessions,
    session_entry_at_row, set_active_project, set_active_session, set_session_context_target,
    set_session_working, truncate_chars, CommandInfo, GuiAgentEvent, MsgRole, SessionEntry,
    ToolStatus,
};
use crate::workspace::{AppState, SessionKey};
use base64::Engine as _;
use makepad_widgets::text::selection::Cursor;
use makepad_widgets::*;
use mypi_agent::{get_runtime, AgentEvent, ImageAttachment, ReasoningEffort};
use mypi_coding_agent::{
    discover_agents, AgentConfig, AgentScope, CodingAgent, CodingAgentOptions, ProjectContext,
    SkillMetadata,
};
use mypi_provider::auth;
use mypi_provider::openai::fetch_available_models;
use robius_file_picker::FileDialog;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

fn global_mypi_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".mypi")
}

fn project_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

fn compact_workspace_path(path: &Path, home: Option<&Path>) -> String {
    let (prefix, display_path) = match (path.is_absolute(), home) {
        (true, Some(home)) => match path.strip_prefix(home).ok() {
            Some(relative) => ("~", relative),
            None => ("", path),
        },
        _ if path.is_absolute() => ("", path),
        _ => ("", path),
    };
    let components: Vec<_> = display_path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();

    if components.is_empty() {
        return if prefix == "~" {
            "~".to_string()
        } else {
            path.display().to_string()
        };
    }

    let compacted = if components.len() > 3 {
        format!(
            "{}/…/{}/{}",
            components[0],
            components[components.len() - 2],
            components[components.len() - 1]
        )
    } else {
        components.join("/")
    };

    match prefix {
        "~" => format!("~/{compacted}"),
        _ if path.is_absolute() => format!("/{compacted}"),
        _ => compacted,
    }
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.components.*

    // -------------------------------------------------------------------
    // Chat message list: bubbles + markdown + streaming tail
    // -------------------------------------------------------------------
    let ChatList = #(ChatList::register_widget(vm)) {
        width: Fill
        height: Fill

        list := PortalList {
            width: Fill
            height: Fill
            flow: Down
            drag_scrolling: false
            auto_tail: true
            smooth_tail: true
            selectable: true
            reuse_items: false

            UserMsg := ChatBubble {
                margin: Inset{top: 6 bottom: 6 left: 50 right: 8}
                draw_bg +: {
                    color: #x2a3547
                    border_radius: 10.0
                }
            }

            AssistantMsg := ChatBubble {
                margin: Inset{top: 6 bottom: 6 left: 8 right: 40}
                draw_bg +: {
                    color: #x20252d
                    border_radius: 10.0
                    border_size: 1.0
                    border_color: #x2d3540
                }
            }

            EmptyChat := View {
                width: Fill
                height: Fit
                margin: Inset{top: 80 bottom: 20 left: 36 right: 36}
                flow: Down
                spacing: 10
                align: Align{x: 0.5}
                title_lbl := Label {
                    width: Fit
                    height: Fit
                    text: "What would you like to build?"
                    draw_text +: {
                        color: #xe7ebf0
                        text_style: theme.font_bold { font_size: 18.0 }
                    }
                }
                desc_lbl := Label {
                    width: Fit
                    height: Fit
                    text: "Ask mypi to inspect your project, explain code, edit files, or run tests."
                    draw_text +: {
                        color: #x9aa4b2
                        text_style +: { font_size: 11.0 }
                    }
                }
                examples_lbl := Label {
                    width: Fit
                    height: Fit
                    text: "Try:  Review the authentication flow   ·   Find unused dependencies   ·   Run the test suite"
                    draw_text +: {
                        color: #x6f7a88
                        text_style +: { font_size: 10.0 }
                    }
                }
                hint_lbl := Label {
                    width: Fit
                    height: Fit
                    text: "Type / for commands  ·  Enter to send  ·  Shift+Enter for a new line"
                    draw_text +: {
                        color: #x6fa8ff
                        text_style +: { font_size: 10.0 }
                    }
                }
            }

            SystemMsg := View {
                width: Fill
                height: Fit
                margin: Inset{top: 3 bottom: 3 left: 12 right: 12}
                lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #x8b93a0
                        text_style +: { font_size: 10.0 }
                    }
                }
            }

            ThinkingMsg := #(ToolFoldHeader::register_widget(vm)) {
                width: Fill
                height: Fit
                flow: Down
                body_walk: Walk{width: Fill, height: Fit}
                margin: Inset{top: 1 bottom: 1 left: 12 right: 12}
                opened: 0.0
                animator +: {
                    active: { default: @off }
                }
                header: ActivityHeader {
                    icon_tile +: {
                        icon_stack +: {
                            icon_generic +: { visible: false }
                            icon_thinking +: {
                                visible: true
                                icon +: { draw_icon +: { color: #x8d8aa3 } }
                            }
                        }
                    }
                    title_lbl +: {
                        width: 70
                        text: "Thinking"
                        draw_text +: { color: #xbcb8cf }
                    }
                    summary := View {
                        width: Fit
                        height: Fit
                        align: Align{y: 0.5}
                        preview_lbl := Label {
                            width: Fit
                            height: Fit
                            text: ""
                            draw_text +: {
                                color: #x8f98a6
                                text_style +: { font_size: 9.0 }
                            }
                        }
                    }
                }
                body: RoundedView {
                    width: Fill
                    height: Fit
                    padding: Inset{left: 30 top: 5 right: 24 bottom: 8}
                    draw_bg +: {
                        color: #x00000000
                        border_size: 0.0
                    }
                    md := Markdown {
                        width: Fill
                        height: Fit
                        selectable: true
                        use_code_block_widget: false
                        body: ""
                    }
                }
            }

            ToolMsg := #(ToolFoldHeader::register_widget(vm)) {
                width: Fill
                height: Fit
                flow: Down
                body_walk: Walk{width: Fill, height: Fit}
                margin: Inset{top: 1 bottom: 1 left: 12 right: 12}
                opened: 0.0
                animator +: {
                    active: { default: @off }
                }
                header: ActivityHeader {
                    summary := View {
                        width: Fit
                        height: Fit
                        flow: Right
                        spacing: 9
                        align: Align{y: 0.5}
                        preview_lbl := Label {
                            width: Fit
                            height: Fit
                            text: ""
                            draw_text +: {
                                color: #xc7d0dc
                                text_style: theme.font_code { font_size: 9.0 }
                            }
                        }

                        status_running_indicator := ActivityLoader {
                            width: 18
                            height: 10
                            visible: false
                            draw_bg +: {
                                dot_radius: 1.0
                                speed: 3.6
                            }
                        }

                        status_error_lbl := Label {
                            width: Fit
                            height: Fit
                            visible: false
                            text: "!"
                            draw_text +: {
                                color: #xe06c75
                                text_style: theme.font_bold { font_size: 8.0 }
                            }
                        }
                    }
                }
                body: RoundedView {
                    width: Fill
                    height: Fit
                    padding: Inset{left: 30 top: 4 right: 18 bottom: 7}
                    flow: Down
                    spacing: 5
                    draw_bg +: {
                        color: #x00000000
                        border_size: 0.0
                    }
                    details_row := View {
                        width: Fill
                        height: Fit
                        flow: Right
                        spacing: 8
                        meta_lbl := Label {
                            width: Fit
                            height: Fit
                            text: ""
                            draw_text +: {
                                color: #x8494a8
                                text_style: theme.font_code { font_size: 8.5 }
                            }
                        }
                        result_meta_lbl := Label {
                            width: Fit
                            height: Fit
                            text: ""
                            draw_text +: {
                                color: #x768292
                                text_style +: { font_size: 8.5 }
                            }
                        }
                    }
                    args_section := ToolSection {
                        section_label +: { text: "INPUT" }
                    }
                    result_section := ToolSection {
                        section_label +: { text: "OUTPUT" }
                        content_lbl +: { draw_text +: { color: #xaeb8c5 } }
                    }
                }
            }
        }
    }



    // -------------------------------------------------------------------
    // Sessions sidebar: project folders + session rows
    // -------------------------------------------------------------------
    let ProjectHeaderBase = RoundedView {
        width: Fill
        height: 34
        cursor: MouseCursor.Hand
        flow: Right
        spacing: 8
        align: Align{y: 0.5}
        margin: Inset{left: 3 right: 3 top: 7 bottom: 2}
        padding: Inset{left: 8 top: 4 right: 4 bottom: 4}
        draw_bg +: {
            color: #x00000000
            color_hover: #x222831
            border_radius: 8.0
        }
        animator +: {
            hover: {
                default: @off
                off: AnimatorState {
                    from: {all: Forward {duration: 0.12}}
                    apply: {draw_bg: {color: #x00000000}}
                }
                on: AnimatorState {
                    from: {all: Forward {duration: 0.08}}
                    apply: {draw_bg: {color: #x222831}}
                }
            }
        }
        folder_icon := Icon {
            width: 16
            height: 16
            icon_walk: Walk{width: 16 height: 16}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/folder.svg")
                color: #x8291a5
            }
        }
        name_lbl := Label {
            width: Fill
            height: 18
            text: ""
            draw_text +: {
                color: #xc2cad5
                text_style: theme.font_bold { font_size: 9.75 }
            }
        }
        detach_project_btn := Button {
            width: 22
            height: 22
            margin: 0
            padding: 0
            text: "×"
            draw_text +: {
                color: #x626d7d
                color_hover: #xd08a92
                color_down: #xf2a0aa
                text_style +: { font_size: 11.0 }
            }
            draw_bg +: {
                color: #x00000000
                color_hover: #x36272d
                color_focus: #x36272d
                color_down: #x482c34
                border_color: #x00000000
                border_color_hover: #x00000000
                border_color_focus: #x00000000
                border_color_down: #x00000000
                border_size: 0.0
                border_radius: 6.0
            }
        }
        new_project_session_btn := Button {
            width: 22
            height: 22
            margin: 0
            padding: 0
            text: ""
            align: Align{x: 0.5 y: 0.5}
            icon_walk: Walk{width: 11 height: 11}
            draw_icon +: {
                svg: crate_resource("self:resources/icons/plus.svg")
                color: #x758294
                color_hover: #xb8d5f5
                color_down: #xffffff
            }
            draw_bg +: {
                color: #x00000000
                color_hover: #x283544
                color_focus: #x283544
                color_down: #x30445b
                border_color: #x00000000
                border_color_hover: #x00000000
                border_color_focus: #x00000000
                border_color_down: #x00000000
                border_size: 0.0
                border_radius: 6.0
            }
        }
    }

    let SessionList = #(SessionList::register_widget(vm)) {
        width: Fill
        height: Fill

        list := PortalList {
            width: Fill
            height: Fill
            flow: Down
            drag_scrolling: true

            ProjectHeader := ProjectHeaderBase {}

            ProjectHeaderActive := ProjectHeaderBase {
                draw_bg +: {
                    color: #x222c38
                    color_hover: #x283543
                    border_color: #x34465a
                    border_size: 1.0
                }
                folder_icon +: { draw_icon +: { color: #x8fb9e8 } }
                name_lbl +: { draw_text +: { color: #xe0e7ef } }
            }

            SessionRow := SessionRowBase {}

            SessionRowActive := SessionRowBase {
                draw_bg +: {
                    color: #x263445
                    color_hover: #x2d3e52
                    border_color: #x344b65
                    border_size: 1.0
                    border_radius: 7.0
                }
                session_icon +: {
                    draw_icon +: { color: #x9fc3ef }
                }
                title_lbl +: {
                    draw_text +: {
                        color: #xe8edf4
                    }
                }
                time_lbl +: {
                    draw_text +: {
                        color: #xaeb6c2
                    }
                }
            }

            SessionRowContext := SessionRowBase {
                draw_bg +: {
                    color: #x273344
                    color_hover: #x30425a
                    border_color: #x4f82bd
                    border_size: 1.0
                    border_radius: 6.0
                }
                session_icon +: {
                    draw_icon +: { color: #x8fb9e8 }
                }
                title_lbl +: {
                    draw_text +: {
                        color: #xf0f4fa
                    }
                }
                time_lbl +: {
                    draw_text +: {
                        color: #xb7c5d8
                    }
                }
            }

            EmptyRow := EmptyRowBase {
                padding: Inset{left: 43 top: 4 right: 10 bottom: 8}
                lbl +: {
                    text: "No sessions yet"
                    draw_text +: { color: #x596474 text_style +: { font_size: 9.25 } }
                }
            }
        }
    }

    startup() do #(App::script_component(vm)){
        ui: Root {
            main_window := Window {
                window.inner_size: vec2(1280, 768)
                window.title: "mypi"
                pass.clear_color: #x181a1f
                body +: {
                    dock := DockFlat {
                        width: Fill
                        height: Fill
                        padding: 0

                        round_corner +: {
                            border_radius: 0.0
                        }

                        splitter: Splitter {
                            size: 6.0
                            draw_bg +: {
                                color: uniform(#x303641)
                                color_hover: uniform(#x4b5665)
                                color_drag: uniform(#x6a7b91)

                                pixel: fn() {
                                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                                    sdf.clear(#x181a1f)
                                    let line_color = mix(
                                        self.color
                                        mix(self.color_hover, self.color_drag, self.drag)
                                        self.hover
                                    )
                                    if self.is_vertical > 0.5 {
                                        sdf.rect(0.0, self.rect_size.y * 0.5 - 0.5, self.rect_size.x, 1.0)
                                    } else {
                                        sdf.rect(self.rect_size.x * 0.5 - 0.5, 0.0, 1.0, self.rect_size.y)
                                    }
                                    sdf.fill(line_color)
                                    return sdf.result
                                }
                            }
                        }

                        root := DockSplitter {
                            axis: SplitterAxis.Horizontal
                            align: SplitterAlign.FromA(250.0)
                            a: @sessions_tabs
                            b: @workspace_tabs
                        }

                        sessions_tabs := DockTabs {
                            tabs: [@sessions_tab]
                            selected: 0
                            closable: false
                            hide_tab_bar: true
                        }

                        workspace_tabs := DockTabs {
                            tabs: [@workspace_tab]
                            selected: 0
                            closable: false
                            hide_tab_bar: true
                        }

                        sessions_tab := DockTab {
                            name: "Sessions"
                            template: @PermanentTab
                            kind: @SessionsDock
                        }

                        workspace_tab := DockTab {
                            name: "Workspace"
                            template: @PermanentTab
                            kind: @WorkspaceDock
                        }

                        SessionsDock := View {
                            width: Fill
                            height: Fill
                            flow: Down
                            spacing: 0
                            padding: Inset{left: 8 top: 8 right: 8 bottom: 10}

                            projects_header := View {
                                width: Fill
                                height: 38
                                flow: Right
                                spacing: 8
                                align: Align{y: 0.5}
                                padding: Inset{left: 7 right: 4 bottom: 4}
                                projects_label := Label {
                                    width: Fill
                                    height: Fit
                                    text: "PROJECTS"
                                    draw_text +: {
                                        color: #x7f8b9b
                                        text_style: theme.font_bold { font_size: 8.5 }
                                    }
                                }
                                add_project_btn := Button {
                                    width: 62
                                    height: 24
                                    text: "Add"
                                    padding: Inset{left: 7 right: 8 top: 4 bottom: 4}
                                    icon_walk: Walk{width: 10 height: 10 margin: Inset{right: 3}}
                                    draw_icon +: {
                                        svg: crate_resource("self:resources/icons/plus.svg")
                                        color: #x9fc3ef
                                        color_hover: #xd3e8ff
                                    }
                                    draw_bg +: {
                                        color: #x232a33
                                        color_hover: #x2a3542
                                        color_focus: #x2a3542
                                        color_down: #x314257
                                        border_color: #x384452
                                        border_color_hover: #x526a84
                                        border_color_focus: #x526a84
                                        border_color_down: #x6fa8ff
                                        border_size: 1.0
                                        border_radius: 7.0
                                    }
                                    draw_text +: {
                                        color: #xb9c5d3
                                        color_hover: #xe4ebf3
                                        text_style +: { font_size: 9.0 }
                                    }
                                }
                            }
                            session_context_menu := SessionContextMenu {}
                            session_list := SessionList { height: Fill }
                        }

                        WorkspaceDock := View {
                            width: Fill
                            height: Fill
                            flow: Down
                            spacing: 8
                            padding: Inset{left: 10 top: 10 right: 12 bottom: 10}

                        header := PanelHeader {
                            spacing: 10
                            padding: Inset{left: 4 top: 1 right: 2 bottom: 3}

                            project_icon := Icon {
                                width: 18
                                height: 18
                                icon_walk: Walk{width: 16 height: 16}
                                draw_icon +: {
                                    svg: crate_resource("self:resources/icons/folder.svg")
                                    color: #x8fb9e8
                                }
                            }
                            project_identity := View {
                                width: Fit
                                height: Fit
                                flow: Down
                                spacing: 1

                                project_name_label := Label {
                                    width: Fit
                                    height: Fit
                                    text: ""
                                    draw_text +: {
                                        color: #xf0f4fa
                                        text_style: theme.font_bold { font_size: 14.0 }
                                    }
                                }
                                workspace_label := Label {
                                    width: Fit
                                    height: Fit
                                    text: ""
                                    draw_text +: {
                                        color: #x7f8b9a
                                        text_style +: { font_size: 9.0 }
                                    }
                                }
                            }
                            FlexSpacer {}
                            caps_btn := Button {
                                width: Fit
                                height: 30
                                padding: Inset{left: 10 right: 11 top: 5 bottom: 5}
                                text: "Capabilities  ›"
                                icon_walk: Walk{width: 13 height: 13 margin: Inset{right: 2}}
                                draw_icon +: {
                                    svg: crate_resource("self:resources/icons/skill.svg")
                                    color: #x8fb9e8
                                    color_hover: #xb9d7fa
                                    color_down: #xffffff
                                }
                                draw_bg +: {
                                    color: #x232830
                                    color_hover: #x2d3642
                                    color_focus: #x2d3642
                                    color_down: #x36475c
                                    border_color: #x3f4a59
                                    border_color_hover: #x6184ac
                                    border_color_focus: #x6184ac
                                    border_color_down: #x7eb4f2
                                    border_radius: 7.0
                                    border_size: 1.0
                                }
                                draw_text +: {
                                    color: #xcbd3dd
                                    color_hover: #xe8edf4
                                    color_focus: #xe8edf4
                                    color_down: #xffffff
                                    text_style: theme.font_bold { font_size: 9.5 }
                                }
                            }

                            status_pill := StatusPill {}
                        }

                        auth_row := AuthRow {}

                        content_row := View {
                            width: Fill
                            height: Fill
                            flow: Right
                            spacing: 10

                            chat_column := View {
                                width: Fill
                                height: Fill
                                flow: Down
                                spacing: 8

                            chat_panel := PanelSurface {
                                flow: Down
                                padding: Inset{left: 4 top: 6 right: 4 bottom: 6}
                                draw_bg +: {
                                    color: #x1c1f26
                                    border_radius: 10.0
                                }
                                chat_list := ChatList { height: Fill }
                                chat_working_indicator := View {
                                    width: Fill
                                    height: 18
                                    visible: false
                                    align: Align{x: 0.0 y: 0.5}
                                    chat_working_spinner := ActivityLoader {
                                        width: 22
                                        height: 11
                                        draw_bg +: {
                                            dot_radius: 1.25
                                            speed: 3.0
                                        }
                                    }
                                }
                            }

                            input_bar := mod.components.ComposerSurface {
                                width: Fill
                                height: Fit
                                flow: Down
                                spacing: 4
                                padding: Inset{left: 9 top: 7 right: 7 bottom: 7}
                                new_batch: true
                                draw_bg +: {
                                    color: #x1f232b
                                    color_hover: #x232830
                                    color_focus: #x252b35
                                    border_color: #x3a424e
                                    border_color_focus: #x4a6f9e
                                    border_color_down: #x6fa8ff
                                    border_color_error: #xb85c55
                                    border_size: 1.0
                                    border_radius: 11.0
                                }

                                attachment_row := View {
                                    width: Fill
                                    height: 24
                                    visible: false
                                    flow: Right
                                    spacing: 6
                                    clip_x: true

                                    attachment_chip_0 := mod.components.AttachmentChip { text: "" }
                                    attachment_chip_1 := mod.components.AttachmentChip { text: "" }
                                    attachment_chip_2 := mod.components.AttachmentChip { text: "" }
                                    attachment_chip_3 := mod.components.AttachmentChip { text: "" }
                                }

                                prompt_input := mod.components.MypiCommandTextInput {
                                    width: Fill
                                    height: Fit
                                    trigger: "/"
                                    inline_search: true
                                    color_focus: #x252b35
                                    color_hover: #x232830

                                    persistent +: {
                                        width: Fill
                                        height: Fit
                                        center +: {
                                            width: Fill
                                            height: Fit
                                            text_input +: {
                                                width: Fill
                                                height: Fit{min: FitBound.Abs(56), max: FitBound.Abs(180)}
                                                margin: 0
                                                padding: Inset{left: 3 top: 6 right: 3 bottom: 6}
                                                is_multiline: true
                                                submit_on_enter: true
                                                empty_text: "Ask mypi anything…"
                                                draw_bg +: {
                                                    color: #x00000000
                                                    color_empty: #x00000000
                                                    color_hover: #x00000000
                                                    color_focus: #x00000000
                                                    color_down: #x00000000
                                                    border_color: #x00000000
                                                    border_color_empty: #x00000000
                                                    border_color_hover: #x00000000
                                                    border_color_focus: #x00000000
                                                    border_color_down: #x00000000
                                                    border_size: 0.0
                                                }
                                                draw_text +: {
                                                    color: #xdde3ea
                                                    color_hover: #xffffff
                                                    color_focus: #xffffff
                                                    color_empty: #x7f8b9a
                                                    color_empty_hover: #x9aa5b3
                                                    color_empty_focus: #x9aa5b3
                                                    text_style +: {
                                                        font_size: 10.5
                                                        line_spacing: 1.35
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                composer_footer := View {
                                    width: Fill
                                    height: 30
                                    flow: Right
                                    spacing: 8
                                    align: Align{y: 0.5}



                                    composer_status := Label {
                                        width: Fit
                                        height: Fit
                                        visible: false
                                        text: ""
                                        draw_text +: {
                                            color: #x9aa5b3
                                            text_style +: { font_size: 8.5 }
                                        }
                                    }

                                    stop_btn := mod.components.ComposerAction {
                                        width: Fit
                                        height: 28
                                        visible: false
                                        text: "Stop"
                                        draw_bg +: {
                                            color: #xb85c55
                                            color_hover: #xd4775f
                                            color_down: #e39a5d
                                        }
                                    }

                                    attach_btn := mod.components.ComposerChip {
                                        width: 30
                                        height: 28
                                        padding: 0
                                        text: ""
                                        align: Align{x: 0.5 y: 0.5}
                                        icon_walk: Walk{width: 14 height: 14}
                                        draw_icon +: {
                                            svg: crate_resource("self:resources/icons/attach.svg")
                                            color: #x9aa5b3
                                            color_hover: #xdde3ea
                                            color_down: #xffffff
                                        }
                                    }

                                    composer_hint := Label {
                                        width: Fit
                                        height: Fit
                                        text: "Enter to send · Shift+Enter for newline"
                                        draw_text +: {
                                            color: #x7f8b9a
                                            text_style +: { font_size: 8.5 }
                                        }
                                    }

                                    FlexSpacer {}

                                    effort_picker := View {
                                        width: 130
                                        height: 28
                                        visible: false
                                        flow: Down
                                        clip_x: false
                                        clip_y: false

                                        effort_drop := ComposerDropDown {
                                            labels: [
                                                "Thinking: Off",
                                                "Thinking: Minimal",
                                                "Thinking: Low",
                                                "Thinking: High",
                                                "Thinking: XHigh",
                                                "Thinking: Max",
                                                "Thinking: Medium"
                                            ]
                                        }
                                    }

                                    model_picker := View {
                                        width: 158
                                        height: 28
                                        visible: false
                                        flow: Down
                                        clip_x: false
                                        clip_y: false

                                        model_drop := ComposerDropDown {
                                            labels: [
                                                "gpt-5.4",
                                                "gpt-5.4-mini",
                                                "gpt-5.5",
                                                "gpt-5.6-sol",
                                                "gpt-5.6-terra",
                                                "gpt-5.3-codex-spark",
                                                "gpt-4o",
                                                "gpt-4o-mini",
                                                "gpt-5.6-luna"
                                            ]
                                        }

                                    }

                                    send_btn := mod.components.ComposerAction {
                                        width: 34
                                        height: 30
                                        margin: 0
                                        padding: 0
                                        text: ""
                                        align: Align{x: 0.5 y: 0.5}
                                        icon_walk: Walk{width: 15 height: 15}
                                        draw_icon +: {
                                            svg: crate_resource("self:resources/icons/send.svg")
                                            color: #xffffff
                                        }
                                    }
                                }
                            }
                            }


                        }

                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
enum ImagePickerAction {
    Loaded {
        key: SessionKey,
        attachment: Result<Option<ImageAttachment>, String>,
    },
}

const MAX_IMAGE_ATTACHMENTS: usize = 4;
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

fn image_attachment_from_rgba(
    display_name: String,
    width: usize,
    height: usize,
    rgba: &[u8],
) -> Result<ImageAttachment, String> {
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "Clipboard image dimensions are too large".to_string())?;
    if rgba.len() != expected_len {
        return Err("Clipboard image has invalid pixel data".to_string());
    }

    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| format!("Could not encode clipboard image: {error}"))?;
        writer
            .write_image_data(rgba)
            .map_err(|error| format!("Could not encode clipboard image: {error}"))?;
    }
    if encoded.len() as u64 > MAX_IMAGE_BYTES {
        return Err("Clipboard image is larger than 10 MB after encoding".to_string());
    }

    Ok(ImageAttachment {
        display_name,
        data_url: format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(encoded)
        ),
    })
}

fn clipboard_image_attachment() -> Result<Option<ImageAttachment>, String> {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(_) => return Ok(None),
    };
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(_) => return Ok(None),
    };
    image_attachment_from_rgba(
        "clipboard.png".to_string(),
        image.width,
        image.height,
        image.bytes.as_ref(),
    )
    .map(Some)
}

fn load_image_attachment(path: &Path) -> Result<ImageAttachment, String> {
    let display_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("Could not read {display_name}: {error}"))?;
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(format!("{display_name} is larger than 10 MB"));
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("Could not read {display_name}: {error}"))?;
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return Err(format!(
            "{display_name} is not a supported PNG, JPEG, GIF, or WebP image"
        ));
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(ImageAttachment {
        display_name,
        data_url: format!("data:{mime};base64,{encoded}"),
    })
}

#[derive(Clone, Copy, PartialEq)]
enum UiStatus {
    Ready,
    Working,
    Error,
}

struct GenerationRun {
    id: u64,
    handle: tokio::task::JoinHandle<()>,
}

struct SessionRuntime {
    agent: Arc<tokio::sync::Mutex<CodingAgent>>,
    generation: Option<GenerationRun>,
    terminal_generation_id: Option<u64>,
    submitted_draft: Option<(u64, String)>,
    submitted_attachments: Option<(u64, Vec<ImageAttachment>)>,
    status: UiStatus,
    status_text: String,
    model: String,
    reasoning_effort: ReasoningEffort,
}

#[derive(Clone)]
struct ProjectCapabilities {
    summary: String,
    button_text: String,
    commands: Vec<CommandInfo>,
}

impl SessionRuntime {
    fn new(agent: CodingAgent, model: String, reasoning_effort: ReasoningEffort) -> Self {
        Self {
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            generation: None,
            terminal_generation_id: None,
            submitted_draft: None,
            submitted_attachments: None,
            status: UiStatus::Ready,
            status_text: String::new(),
            model,
            reasoning_effort,
        }
    }
}

#[derive(Script)]
pub struct App {
    #[live]
    pub ui: WidgetRef,
    #[rust]
    tx: Option<Sender<GuiAgentEvent>>,
    #[rust]
    rx: Option<Arc<Mutex<Receiver<GuiAgentEvent>>>>,
    #[rust]
    session_runtimes: HashMap<SessionKey, SessionRuntime>,
    #[rust]
    next_generation_id: u64,
    #[rust]
    busy: bool,
    #[rust]
    composer_state: ComposerState,
    #[rust]
    commands: Vec<CommandInfo>,
    #[rust]
    capabilities_summary: String,
    #[rust]
    available_models: Vec<String>,
    #[rust]
    workspace_state: AppState,
    #[rust]
    session_context_entry: Option<SessionEntry>,
    #[rust]
    auth_workspace: Option<SessionKey>,
    #[rust]
    project_registry: Option<ProjectRegistry>,
    #[rust]
    capability_cache: HashMap<PathBuf, ProjectCapabilities>,
}

impl ScriptHook for App {}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        let (tx, rx) = channel::<GuiAgentEvent>();
        self.tx = Some(tx);
        self.rx = Some(Arc::new(Mutex::new(rx)));
        self.set_model_dropup_options(
            cx,
            vec![
                "gpt-5.6-luna".into(),
                "gpt-5.4".into(),
                "gpt-5.4-mini".into(),
                "gpt-5.5".into(),
                "gpt-5.6-sol".into(),
                "gpt-5.6-terra".into(),
                "gpt-5.3-codex-spark".into(),
                "gpt-4o".into(),
                "gpt-4o-mini".into(),
            ],
            "gpt-5.6-luna",
        );
        self.set_reasoning_effort_picker(cx, ReasoningEffort::Medium);

        let launch_dir = std::env::current_dir()
            .ok()
            .and_then(|path| std::fs::canonicalize(path).ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let mut registry_error = None;
        match ProjectRegistry::load(&global_mypi_dir()) {
            Ok(mut registry) => {
                if let Err(error) = registry.attach(&launch_dir) {
                    registry_error = Some(error.to_string());
                }
                self.project_registry = Some(registry);
            }
            Err(error) => registry_error = Some(error.to_string()),
        }
        let project_dirs = self.registered_project_dirs_or(&launch_dir);
        refresh_sessions(&project_dirs);
        let selected_project = self
            .project_registry
            .as_ref()
            .and_then(|registry| {
                registry
                    .projects()
                    .iter()
                    .filter(|project| project.path.is_dir())
                    .max_by_key(|project| project.last_opened_at)
            })
            .cloned();
        let work_dir = selected_project
            .as_ref()
            .map(|project| project.path.clone())
            .unwrap_or_else(|| launch_dir.clone());
        let initial_entry = {
            let data = crate::panels::sessions::state::SESSIONS_DATA
                .read()
                .unwrap();
            data.projects
                .iter()
                .find(|project| project.work_dir == work_dir)
                .and_then(|project| {
                    selected_project
                        .as_ref()
                        .and_then(|selected| selected.last_session_id.as_deref())
                        .and_then(|session_id| {
                            project
                                .sessions
                                .iter()
                                .find(|session| session.id == session_id)
                        })
                        .or_else(|| project.sessions.first())
                })
                .cloned()
        };
        if let Some(entry) = initial_entry.as_ref() {
            set_active_session(&entry.work_dir, &entry.id);
            self.select_workspace(entry.work_dir.clone(), entry.id.clone());
        } else {
            set_active_project(&work_dir);
            self.workspace_state
                .select(SessionKey::project_draft(work_dir.clone()));
        }

        let mut key_opt = None;
        let mut account_id_opt = None;

        if let Some(creds) = auth::load_credentials() {
            self.ui
                .text_input(cx, ids!(api_key_input))
                .set_text(cx, &creds.access_token);
            self.push_chat(
                MsgRole::System,
                format!("Loaded saved credentials from {}", creds.source),
            );
            key_opt = Some(creds.access_token.clone());
            account_id_opt = creds.account_id.clone();
            self.ui.widget(cx, ids!(auth_row)).set_visible(cx, false);
            self.set_status(cx, UiStatus::Ready, "Ready");
        } else {
            self.ui.widget(cx, ids!(auth_row)).set_visible(cx, true);
            self.set_status(cx, UiStatus::Error, "Not signed in");
        }

        let home_dir = std::env::var_os("HOME").map(PathBuf::from);
        self.ui
            .label(cx, ids!(project_name_label))
            .set_text(cx, &project_name(&work_dir));
        self.ui
            .label(cx, ids!(workspace_label))
            .set_text(cx, &compact_workspace_path(&work_dir, home_dir.as_deref()));
        let context = ProjectContext::discover(&work_dir);
        if let Some(error) = registry_error {
            self.push_chat(
                MsgRole::System,
                format!("Could not load the attached-project registry: {error}"),
            );
        }

        if !context.context_files.is_empty() {
            self.push_chat(
                MsgRole::System,
                format!(
                    "Discovered {} context file(s): {:?}",
                    context.context_files.len(),
                    context.context_files
                ),
            );
        }

        let api_key = key_opt
            .clone()
            .unwrap_or_else(|| std::env::var("OPENAI_API_KEY").unwrap_or_default());
        let agent_opts = CodingAgentOptions {
            api_key: api_key.clone(),
            account_id: account_id_opt.clone(),
            model: "gpt-5.6-luna".to_string(),
            work_dir: work_dir.clone(),
            session_file: initial_entry
                .as_ref()
                .map(|entry| entry.session_file.clone()),
        };

        let coding_agent = CodingAgent::new(agent_opts);
        let discovered_skills: Vec<_> = coding_agent
            .skills
            .list_skills()
            .into_iter()
            .filter(|skill| skill.enabled && skill.is_valid)
            .collect();
        let discovered_agents = discover_agents(&work_dir, AgentScope::Both).agents;
        let subagent_enabled = coding_agent
            .wasi_extensions
            .get_tools()
            .iter()
            .any(|tool| tool["function"]["name"] == "subagent");
        self.capabilities_summary =
            format_capabilities_summary(&discovered_skills, &discovered_agents, subagent_enabled);
        self.ui.button(cx, ids!(caps_btn)).set_text(
            cx,
            &format!(
                "{} skills · {} agents  ›",
                discovered_skills.len(),
                discovered_agents.len()
            ),
        );

        self.commands = builtin_commands();
        self.commands
            .extend(discovered_skills.iter().map(|skill| CommandInfo {
                name: format!("skill {}", skill.id),
                description: format!(
                    "{} · {}",
                    skill.scope.display_name(),
                    truncate_chars(&normalize_catalog_text(&skill.description), 120)
                ),
            }));
        if coding_agent.wasi_extensions.extensions.is_empty() {
            self.push_chat(
                MsgRole::System,
                "No WASI extensions loaded (place packages in ./.mypi/extensions/<id>/)",
            );
        } else {
            for (ext_name, ext) in &coding_agent.wasi_extensions.extensions {
                let mut cmd_names = Vec::new();
                for cmd in &ext.manifest.commands {
                    cmd_names.push(format!("/{}", cmd.name));
                    self.commands.push(CommandInfo {
                        name: cmd.name.clone(),
                        description: cmd.description.clone(),
                    });
                }
                self.push_chat(
                    MsgRole::System,
                    format!(
                        "Loaded WASI extension `{}` ({}) — commands: {}",
                        ext_name,
                        ext.manifest.description,
                        cmd_names.join(", ")
                    ),
                );
            }
        }
        self.push_chat(
            MsgRole::System,
            "Type / in the input bar to browse slash commands.",
        );

        let initial_key = self
            .workspace_state
            .active_key()
            .cloned()
            .unwrap_or_else(|| SessionKey::project_draft(work_dir));
        if let Some(entry) = initial_entry {
            let messages = coding_agent.session_tree.get_active_branch_messages();
            self.workspace_state
                .workspace_mut(SessionKey::new(entry.work_dir, entry.id))
                .chat
                .replace_from_agent_messages(&messages);
        }
        self.session_runtimes.insert(
            initial_key,
            SessionRuntime::new(
                coding_agent,
                "gpt-5.6-luna".to_string(),
                ReasoningEffort::Medium,
            ),
        );

        self.spawn_model_fetch(api_key, account_id_opt);

        cx.redraw_all();
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            if let Some(ImagePickerAction::Loaded { key, attachment }) =
                action.downcast_ref::<ImagePickerAction>()
            {
                self.apply_image_picker_result(cx, key.clone(), attachment.clone());
            }
        }

        if self.ui.button(cx, ids!(login_btn)).clicked(actions) {
            self.auth_workspace = self.workspace_state.active_key().cloned();
            self.push_chat(MsgRole::System, "Initiating ChatGPT device code login...");
            self.apply_status_ui(cx, UiStatus::Working, "Connecting to ChatGPT...");
            cx.redraw_all();

            if let Some(tx) = self.tx.clone() {
                get_runtime().spawn(async move {
                    match auth::start_device_login().await {
                        Ok(resp) => {
                            let _ = tx.send(GuiAgentEvent::DeviceCodePrompt {
                                user_code: resp.user_code.clone(),
                                url: resp.verification_uri.clone(),
                            });
                            SignalToUI::set_ui_signal();

                            loop {
                                tokio::time::sleep(tokio::time::Duration::from_secs(
                                    resp.interval.max(3),
                                ))
                                .await;
                                match auth::poll_device_token(&resp.device_auth_id, &resp.user_code)
                                    .await
                                {
                                    Ok(_tokens) => {
                                        let _ = tx.send(GuiAgentEvent::DeviceLoginSuccess);
                                        SignalToUI::set_ui_signal();
                                        break;
                                    }
                                    Err(e)
                                        if e == "authorization_pending"
                                            || e.contains("pending") =>
                                    {
                                        continue
                                    }
                                    Err(e) => {
                                        let _ = tx.send(GuiAgentEvent::DeviceLoginError(e));
                                        SignalToUI::set_ui_signal();
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(GuiAgentEvent::DeviceLoginError(e));
                            SignalToUI::set_ui_signal();
                        }
                    }
                });
            }
        }

        if self.ui.button(cx, ids!(add_project_btn)).clicked(actions) {
            self.open_project_picker();
        }

        if self.ui.button(cx, ids!(caps_btn)).clicked(actions) {
            let summary = self.capabilities_summary.clone();
            self.push_chat(MsgRole::System, summary);
            cx.redraw_all();
        }

        if self.ui.button(cx, ids!(stop_btn)).clicked(actions) {
            let active_key = self.workspace_state.active_key().cloned();
            if let Some(key) = active_key {
                let current_draft = self
                    .ui
                    .mypi_command_text_input(cx, ids!(prompt_input))
                    .text_input_ref(cx)
                    .text();
                let (restored_draft, restored_attachments) = self
                    .session_runtimes
                    .get_mut(&key)
                    .and_then(|runtime| {
                        let generation = runtime.generation.take()?;
                        let generation_id = generation.id;
                        generation.handle.abort();
                        runtime.terminal_generation_id = None;
                        let draft = draft_for_cancellation(
                            Some(generation_id),
                            runtime.submitted_draft.as_ref(),
                            generation_id,
                        );
                        let attachments = runtime
                            .submitted_attachments
                            .as_ref()
                            .filter(|(id, _)| *id == generation_id)
                            .map(|(_, attachments)| attachments.clone())
                            .unwrap_or_default();
                        runtime.submitted_draft = None;
                        runtime.submitted_attachments = None;
                        Some((draft, attachments))
                    })
                    .unwrap_or_default();
                let draft = if current_draft.trim().is_empty() {
                    restored_draft.unwrap_or_default()
                } else {
                    current_draft
                };
                if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                    workspace.ui.draft = draft.clone();
                    workspace.ui.attachments = restored_attachments;
                }
                self.ui
                    .mypi_command_text_input(cx, ids!(prompt_input))
                    .text_input_ref(cx)
                    .set_text(cx, &draft);
                self.refresh_attachment_ui(cx);
                self.set_session_status(cx, &key, UiStatus::Ready, "Stopped");
                self.push_chat(MsgRole::System, "Generation stopped.");
                cx.redraw_all();
            }
        }

        let session_menu_uid = self.ui.widget(cx, ids!(session_context_menu)).widget_uid();
        if let Some(action) = actions.find_widget_action(session_menu_uid) {
            match action.cast::<SessionContextMenuAction>() {
                SessionContextMenuAction::Archive => {
                    self.apply_session_context_action(cx, archive_session, "Archived");
                }
                SessionContextMenuAction::Delete => {
                    self.apply_session_context_action(cx, delete_session, "Deleted");
                }
                SessionContextMenuAction::None => {}
            }
        }
        let session_list = self.ui.portal_list(cx, ids!(session_list.list));
        for (item_id, item) in session_list.items_with_actions(actions) {
            if item.button(cx, ids!(detach_project_btn)).clicked(actions) {
                if let Some(work_dir) = project_work_dir_at_row(item_id) {
                    self.detach_project(cx, work_dir);
                }
                continue;
            }
            if item
                .button(cx, ids!(new_project_session_btn))
                .clicked(actions)
            {
                if let Some(work_dir) = project_work_dir_at_row(item_id) {
                    self.create_and_activate_session(cx, work_dir);
                }
                continue;
            }
            if let Some(fe) = item.as_view().finger_up(actions) {
                if let Some(work_dir) = project_work_dir_at_row(item_id) {
                    if fe.is_over && fe.is_primary_hit() && fe.was_tap() {
                        self.select_project_draft(cx, work_dir);
                    }
                    continue;
                }
                let Some(entry) = session_entry_at_row(item_id) else {
                    continue;
                };
                if fe.is_over
                    && fe.was_tap()
                    && fe
                        .mouse_button()
                        .is_some_and(|button| button.is_secondary())
                {
                    self.session_context_entry = Some(entry.clone());
                    set_session_context_target(Some(&entry));
                    if let Some(mut menu) = self
                        .ui
                        .widget(cx, ids!(session_context_menu))
                        .borrow_mut::<SessionContextMenu>()
                    {
                        menu.open(cx, fe.abs);
                    }
                    cx.redraw_all();
                } else if fe.is_over && fe.is_primary_hit() && fe.was_tap() {
                    if let Some(mut menu) = self
                        .ui
                        .widget(cx, ids!(session_context_menu))
                        .borrow_mut::<SessionContextMenu>()
                    {
                        menu.close(cx);
                    }
                    self.session_context_entry = None;
                    self.activate_session(cx, entry);
                }
            }
        }

        if self.ui.button(cx, ids!(attach_btn)).clicked(actions) && !self.busy {
            self.open_image_picker(cx);
        }
        if self.ui.button(cx, ids!(attachment_chip_0)).clicked(actions) {
            self.remove_attachment(cx, 0);
        }
        if self.ui.button(cx, ids!(attachment_chip_1)).clicked(actions) {
            self.remove_attachment(cx, 1);
        }
        if self.ui.button(cx, ids!(attachment_chip_2)).clicked(actions) {
            self.remove_attachment(cx, 2);
        }
        if self.ui.button(cx, ids!(attachment_chip_3)).clicked(actions) {
            self.remove_attachment(cx, 3);
        }

        let cti = self.ui.mypi_command_text_input(cx, ids!(prompt_input));
        if cti.should_build_items(actions) {
            self.build_cmd_items(cx);
        }
        if let Some(name) = cti.item_selected(actions) {
            let text = format!("/{name} ");
            let text_input = cti.text_input_ref(cx);
            text_input.set_text(cx, &text);
            text_input.set_cursor(
                cx,
                Cursor {
                    index: text.chars().count(),
                    prefer_next_row: false,
                },
                false,
            );
        }

        if self
            .ui
            .drop_down(cx, ids!(effort_drop))
            .selected(actions)
            .is_some()
        {
            let selected_label = self.ui.drop_down(cx, ids!(effort_drop)).selected_label();
            if !self.busy {
                if let Some(effort) = ReasoningEffort::from_label(&selected_label) {
                    if let Some(key) = self.workspace_state.active_key() {
                        if let Some(runtime) = self.session_runtimes.get_mut(key) {
                            runtime.reasoning_effort = effort;
                        }
                    }
                    self.set_reasoning_effort_picker(cx, effort);
                }
            }
        }

        if self
            .ui
            .drop_down(cx, ids!(model_drop))
            .selected(actions)
            .is_some()
        {
            let model_name = self.ui.drop_down(cx, ids!(model_drop)).selected_label();
            if !model_name.is_empty() && !self.busy {
                self.set_model_dropup_options(cx, self.available_models.clone(), &model_name);
                self.dispatch_input(cx, format!("/model {model_name}"), false);
            }
        }

        let submit_prompt = self.ui.button(cx, ids!(send_btn)).clicked(actions)
            || cti.text_input_ref(cx).returned(actions).is_some();
        if submit_prompt && !self.busy {
            let input_text = cti.text_input_ref(cx).text();
            let has_attachments = self
                .workspace_state
                .active_workspace()
                .is_some_and(|workspace| !workspace.ui.attachments.is_empty());
            if !input_text.trim().is_empty() || has_attachments {
                self.dispatch_input(cx, input_text, true);
            }
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        crate::components::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if self.handle_clipboard_image_paste(cx, event) {
            return;
        }
        self.match_event(cx, event);
        self.poll_agent_events(cx);
        let mut scope = Scope::with_data(&mut self.workspace_state);
        self.ui.handle_event(cx, event, &mut scope);
    }
}

const MAX_CAPABILITY_SUMMARY_ITEMS: usize = 32;

fn normalize_catalog_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_capabilities_summary(
    skills: &[SkillMetadata],
    agents: &[AgentConfig],
    subagent_enabled: bool,
) -> String {
    let mut summary = format!(
        "Capabilities\n\nSkills ({}) — use /skill <id> or let the model load one automatically.",
        skills.len()
    );
    if skills.is_empty() {
        summary.push_str("\n  No skills discovered.");
    } else {
        for skill in skills.iter().take(MAX_CAPABILITY_SUMMARY_ITEMS) {
            summary.push_str(&format!(
                "\n  • {} [{}] — {}",
                truncate_chars(&normalize_catalog_text(&skill.id), 128),
                skill.scope.display_name(),
                truncate_chars(&normalize_catalog_text(&skill.description), 160)
            ));
        }
        if skills.len() > MAX_CAPABILITY_SUMMARY_ITEMS {
            summary.push_str(&format!(
                "\n  … and {} more",
                skills.len() - MAX_CAPABILITY_SUMMARY_ITEMS
            ));
        }
    }

    summary.push_str(&format!(
        "\n\nSubagents ({}) — {}",
        agents.len(),
        if subagent_enabled {
            "use /subagent <task>, or let the model delegate automatically."
        } else {
            "the subagent extension is not loaded."
        }
    ));
    if agents.is_empty() {
        summary.push_str("\n  No agent presets discovered.");
    } else {
        for agent in agents.iter().take(MAX_CAPABILITY_SUMMARY_ITEMS) {
            let model = agent
                .model
                .as_deref()
                .map(|model| format!(" · {}", truncate_chars(&normalize_catalog_text(model), 96)))
                .unwrap_or_default();
            summary.push_str(&format!(
                "\n  • {} [{}{}] — {}",
                truncate_chars(&normalize_catalog_text(&agent.name), 128),
                agent.source.as_str(),
                model,
                truncate_chars(&normalize_catalog_text(&agent.description), 160)
            ));
        }
        if agents.len() > MAX_CAPABILITY_SUMMARY_ITEMS {
            summary.push_str(&format!(
                "\n  … and {} more",
                agents.len() - MAX_CAPABILITY_SUMMARY_ITEMS
            ));
        }
    }
    summary
}

impl App {
    fn set_model_dropup_options(&mut self, cx: &mut Cx, models: Vec<String>, selected_model: &str) {
        let mut ordered = Vec::new();
        for model in models {
            if !model.is_empty() && !ordered.contains(&model) {
                ordered.push(model);
            }
        }
        if ordered.is_empty() {
            return;
        }

        let selected_model = if ordered.iter().any(|model| model == selected_model) {
            selected_model.to_string()
        } else {
            ordered[0].clone()
        };
        ordered.retain(|model| model != &selected_model);
        ordered.push(selected_model);

        let selected_item = ordered.len() - 1;
        self.available_models = ordered.clone();
        let model_drop = self.ui.drop_down(cx, ids!(model_drop));
        model_drop.set_labels(cx, ordered);
        model_drop.set_selected_item(cx, selected_item);
    }

    fn set_reasoning_effort_picker(&mut self, cx: &mut Cx, effort: ReasoningEffort) {
        let efforts = [
            ReasoningEffort::Off,
            ReasoningEffort::Minimal,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::XHigh,
            ReasoningEffort::Max,
        ];
        let mut ordered: Vec<_> = efforts
            .into_iter()
            .filter(|candidate| *candidate != effort)
            .collect();
        ordered.push(effort);

        let labels = ordered
            .iter()
            .map(|effort| format!("Thinking: {}", effort.label()))
            .collect();
        let effort_drop = self.ui.drop_down(cx, ids!(effort_drop));
        effort_drop.set_labels(cx, labels);
        effort_drop.set_selected_item(cx, ordered.len() - 1);
    }

    fn refresh_attachment_ui(&self, cx: &mut Cx) {
        let names = self
            .workspace_state
            .active_workspace()
            .map(|workspace| {
                workspace
                    .ui
                    .attachments
                    .iter()
                    .map(|attachment| attachment.display_name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.ui
            .widget(cx, ids!(attachment_row))
            .set_visible(cx, !names.is_empty());
        for (index, path) in [
            ids!(attachment_chip_0),
            ids!(attachment_chip_1),
            ids!(attachment_chip_2),
            ids!(attachment_chip_3),
        ]
        .into_iter()
        .enumerate()
        {
            let chip = self.ui.button(cx, path);
            if let Some(name) = names.get(index) {
                chip.set_text(cx, &format!("{name}  ×"));
                chip.set_visible(cx, true);
            } else {
                chip.set_visible(cx, false);
            }
        }
    }

    fn remove_attachment(&mut self, cx: &mut Cx, index: usize) {
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            if index < workspace.ui.attachments.len() {
                workspace.ui.attachments.remove(index);
            }
        }
        self.refresh_attachment_ui(cx);
    }

    fn handle_clipboard_image_paste(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if self.busy
            || !cx.has_key_focus(
                self.ui
                    .mypi_command_text_input(cx, ids!(prompt_input))
                    .text_input_ref(cx)
                    .area(),
            )
        {
            return false;
        }
        let is_paste = match event {
            Event::TextInput(input) => input.was_paste,
            Event::KeyDown(key) => {
                key.key_code == KeyCode::KeyV && (key.modifiers.logo || key.modifiers.control)
            }
            _ => false,
        };
        if !is_paste {
            return false;
        }

        match clipboard_image_attachment() {
            Ok(Some(attachment)) => {
                if let Some(key) = self.workspace_state.active_key().cloned() {
                    self.apply_image_picker_result(cx, key, Ok(Some(attachment)));
                }
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.push_chat(MsgRole::System, error);
                cx.redraw_all();
                true
            }
        }
    }

    fn apply_image_picker_result(
        &mut self,
        cx: &mut Cx,
        key: SessionKey,
        result: Result<Option<ImageAttachment>, String>,
    ) {
        if self.workspace_state.workspace(&key).is_none() {
            return;
        }
        match result {
            Ok(Some(attachment)) => {
                let workspace = self.workspace_state.workspace_mut(key.clone());
                if workspace.ui.attachments.len() >= MAX_IMAGE_ATTACHMENTS {
                    self.push_chat_to(
                        key.clone(),
                        MsgRole::System,
                        format!("You can attach up to {MAX_IMAGE_ATTACHMENTS} images per prompt"),
                    );
                } else if !workspace
                    .ui
                    .attachments
                    .iter()
                    .any(|existing| existing.data_url == attachment.data_url)
                {
                    workspace.ui.attachments.push(attachment);
                }
            }
            Ok(None) => {}
            Err(error) => self.push_chat_to(key.clone(), MsgRole::System, error),
        }
        if self.workspace_state.is_active(&key) {
            self.refresh_attachment_ui(cx);
        }
    }

    fn open_image_picker(&mut self, cx: &mut Cx) {
        let Some(key) = self.workspace_state.active_key().cloned() else {
            return;
        };
        if self
            .workspace_state
            .workspace(&key)
            .is_some_and(|workspace| workspace.ui.attachments.len() >= MAX_IMAGE_ATTACHMENTS)
        {
            self.push_chat(
                MsgRole::System,
                format!("You can attach up to {MAX_IMAGE_ATTACHMENTS} images per prompt"),
            );
            return;
        }

        let callback_key = key.clone();
        let result = FileDialog::new()
            .set_title("Attach an image")
            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
            .pick_image(move |result| {
                let attachment = match result {
                    Ok(Some(file)) => match file.into_local_file() {
                        Ok(local_file) => load_image_attachment(local_file.path()).map(Some),
                        Err(error) => Err(format!("Could not access selected image: {error}")),
                    },
                    Ok(None) => Ok(None),
                    Err(error) => Err(format!("Image picker failed: {error}")),
                };
                Cx::post_action(ImagePickerAction::Loaded {
                    key: callback_key,
                    attachment,
                });
            });
        if let Err(error) = result {
            self.push_chat_to(
                key,
                MsgRole::System,
                format!("Image picker failed: {error}"),
            );
            cx.redraw_all();
        }
    }

    fn registered_project_dirs_or(&self, fallback: &Path) -> Vec<PathBuf> {
        let dirs = self
            .project_registry
            .as_ref()
            .map(|registry| {
                registry
                    .projects()
                    .iter()
                    .map(|project| project.path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if dirs.is_empty() {
            vec![fallback.to_path_buf()]
        } else {
            dirs
        }
    }

    fn registered_project_dirs(&self) -> Vec<PathBuf> {
        self.project_registry
            .as_ref()
            .map(|registry| {
                registry
                    .projects()
                    .iter()
                    .map(|project| project.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn refresh_registered_sessions(&self) {
        refresh_sessions(&self.registered_project_dirs());
    }

    fn active_work_dir(&self) -> Option<&Path> {
        self.workspace_state
            .active_key()
            .map(|key| key.work_dir.as_path())
    }

    fn is_attached_project(&self, work_dir: &Path) -> bool {
        if let Some(registry) = self.project_registry.as_ref() {
            return registry
                .projects()
                .iter()
                .any(|project| project.path == work_dir && project.path.is_dir());
        }
        crate::panels::sessions::state::SESSIONS_DATA
            .read()
            .unwrap()
            .projects
            .iter()
            .any(|project| project.work_dir == work_dir && project.available)
    }

    fn open_project_picker(&self) {
        // rfd's macOS backend must be invoked from the application main thread.
        // Makepad action handlers run there, so do not move this call to a worker.
        let picked = rfd::FileDialog::new()
            .set_title("Attach a project folder")
            .pick_folder();
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(GuiAgentEvent::ProjectFolderPicked(Ok(picked)));
            SignalToUI::set_ui_signal();
        }
    }

    fn apply_project_folder_result(
        &mut self,
        cx: &mut Cx,
        result: Result<Option<PathBuf>, String>,
    ) {
        let raw_path = match result {
            Ok(Some(path)) => path,
            Ok(None) => return,
            Err(error) => {
                self.push_chat(MsgRole::System, format!("Project picker failed: {error}"));
                return;
            }
        };
        let Some(registry) = self.project_registry.as_mut() else {
            self.push_chat(MsgRole::System, "The project registry is unavailable.");
            return;
        };
        match registry.attach(&raw_path) {
            Ok(project) => {
                self.refresh_registered_sessions();
                self.select_project_draft(cx, project.path);
                self.ui
                    .mypi_command_text_input(cx, ids!(prompt_input))
                    .text_input_ref(cx)
                    .set_key_focus(cx);
            }
            Err(error) => self.push_chat(
                MsgRole::System,
                format!("Could not attach project: {error}"),
            ),
        }
    }

    fn detach_project(&mut self, cx: &mut Cx, work_dir: PathBuf) {
        if is_project_working(&work_dir) {
            self.push_chat(
                MsgRole::System,
                format!(
                    "Stop all running sessions in `{}` before detaching it.",
                    project_name(&work_dir)
                ),
            );
            return;
        }
        let Some(registry) = self.project_registry.as_mut() else {
            return;
        };
        match registry.detach(&work_dir) {
            Ok(true) => {
                let was_active = self.active_work_dir() == Some(work_dir.as_path());
                self.session_runtimes
                    .retain(|key, _| key.work_dir != work_dir);
                let keys = self
                    .workspace_state
                    .keys_for_project(&work_dir)
                    .cloned()
                    .collect::<Vec<_>>();
                for key in keys {
                    self.workspace_state.remove(&key);
                }
                self.refresh_registered_sessions();
                if was_active {
                    if let Some(fallback) = self.registered_project_dirs().into_iter().next() {
                        self.select_project_draft(cx, fallback);
                    }
                }
            }
            Ok(false) => {}
            Err(error) => self.push_chat(
                MsgRole::System,
                format!("Could not detach project: {error}"),
            ),
        }
    }

    fn current_credentials(&self, cx: &Cx) -> (String, Option<String>) {
        let mut api_key = self.ui.text_input(cx, ids!(api_key_input)).text();
        let mut account_id = None;

        if let Some(creds) = auth::load_credentials() {
            if api_key.trim().is_empty() || api_key.trim() == creds.access_token {
                api_key = creds.access_token;
                account_id = creds.account_id;
            }
        }
        if api_key.trim().is_empty() {
            api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        }

        (api_key, account_id)
    }

    fn select_workspace(&mut self, work_dir: PathBuf, session_id: impl Into<String>) {
        self.workspace_state
            .select(SessionKey::new(work_dir, session_id));
    }

    fn select_project_draft(&mut self, cx: &mut Cx, work_dir: PathBuf) {
        if !work_dir.is_dir() {
            self.push_chat(
                MsgRole::System,
                format!("Project folder `{}` is missing.", work_dir.display()),
            );
            return;
        }
        set_active_project(&work_dir);
        if let Some(registry) = self.project_registry.as_mut() {
            if let Err(error) = registry.remember_selection(&work_dir, None) {
                self.push_chat(
                    MsgRole::System,
                    format!("Could not update recent-project state: {error}"),
                );
            }
        }
        self.select_workspace_ui(cx, work_dir.clone(), "draft".to_string());
        let key = SessionKey::project_draft(work_dir.clone());
        if !self.session_runtimes.contains_key(&key) {
            let (api_key, account_id) = self.current_credentials(cx);
            let model = self.ui.drop_down(cx, ids!(model_drop)).selected_label();
            let model = if model.is_empty() {
                "gpt-5.6-luna".to_string()
            } else {
                model
            };
            let effort = ReasoningEffort::from_label(
                &self.ui.drop_down(cx, ids!(effort_drop)).selected_label(),
            )
            .unwrap_or_default();
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key,
                account_id,
                model: model.clone(),
                work_dir: work_dir.clone(),
                session_file: None,
            });
            self.session_runtimes
                .insert(key.clone(), SessionRuntime::new(agent, model, effort));
        }
        if let Some((model, effort)) = self
            .session_runtimes
            .get(&key)
            .map(|runtime| (runtime.model.clone(), runtime.reasoning_effort))
        {
            self.set_model_dropup_options(cx, self.available_models.clone(), &model);
            self.set_reasoning_effort_picker(cx, effort);
        }
        self.refresh_project_capabilities(cx, &work_dir);
        self.restore_active_status(cx);
        cx.redraw_all();
    }

    fn refresh_project_capabilities(&mut self, cx: &mut Cx, work_dir: &Path) {
        let canonical = std::fs::canonicalize(work_dir).unwrap_or_else(|_| work_dir.to_path_buf());
        let capabilities =
            if let Some(cached) = self.capability_cache.get(&canonical) {
                cached.clone()
            } else {
                let (api_key, account_id) = self.current_credentials(cx);
                let agent = CodingAgent::new(CodingAgentOptions {
                    api_key,
                    account_id,
                    model: "gpt-5.6-luna".to_string(),
                    work_dir: canonical.clone(),
                    session_file: None,
                });
                let skills = agent
                    .skills
                    .list_skills()
                    .into_iter()
                    .filter(|skill| skill.enabled && skill.is_valid)
                    .collect::<Vec<_>>();
                let agents = discover_agents(&canonical, AgentScope::Both).agents;
                let subagent_enabled = agent
                    .wasi_extensions
                    .get_tools()
                    .iter()
                    .any(|tool| tool["function"]["name"] == "subagent");
                let mut commands = builtin_commands();
                commands.extend(skills.iter().map(|skill| CommandInfo {
                    name: format!("skill {}", skill.id),
                    description: format!(
                        "{} · {}",
                        skill.scope.display_name(),
                        truncate_chars(&normalize_catalog_text(&skill.description), 120)
                    ),
                }));
                for extension in agent.wasi_extensions.extensions.values() {
                    commands.extend(extension.manifest.commands.iter().map(|command| {
                        CommandInfo {
                            name: command.name.clone(),
                            description: command.description.clone(),
                        }
                    }));
                }
                let capabilities = ProjectCapabilities {
                    summary: format_capabilities_summary(&skills, &agents, subagent_enabled),
                    button_text: format!("{} skills · {} agents  ›", skills.len(), agents.len()),
                    commands,
                };
                self.capability_cache
                    .insert(canonical.clone(), capabilities.clone());
                capabilities
            };
        self.capabilities_summary = capabilities.summary;
        self.commands = capabilities.commands;
        self.ui
            .button(cx, ids!(caps_btn))
            .set_text(cx, &capabilities.button_text);
    }

    fn save_active_draft(&mut self, cx: &Cx) {
        let draft = self
            .ui
            .mypi_command_text_input(cx, ids!(prompt_input))
            .text_input_ref(cx)
            .text();
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            workspace.ui.draft = draft;
        }
    }

    fn select_workspace_ui(&mut self, cx: &mut Cx, work_dir: PathBuf, session_id: String) {
        self.save_active_draft(cx);
        self.select_workspace(work_dir, session_id);
        if let Some(key) = self.workspace_state.active_key() {
            let home_dir = std::env::var_os("HOME").map(PathBuf::from);
            self.ui
                .label(cx, ids!(project_name_label))
                .set_text(cx, &project_name(&key.work_dir));
            self.ui.label(cx, ids!(workspace_label)).set_text(
                cx,
                &compact_workspace_path(&key.work_dir, home_dir.as_deref()),
            );
        }
        let draft = self
            .workspace_state
            .active_workspace()
            .map(|workspace| workspace.ui.draft.clone())
            .unwrap_or_default();
        self.ui
            .mypi_command_text_input(cx, ids!(prompt_input))
            .text_input_ref(cx)
            .set_text(cx, &draft);
        self.refresh_attachment_ui(cx);
    }

    fn push_chat(&mut self, role: MsgRole, text: impl Into<String>) {
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            workspace.chat.push_chat(role, text);
        }
    }

    fn push_chat_to(&mut self, key: SessionKey, role: MsgRole, text: impl Into<String>) {
        self.workspace_state
            .workspace_mut(key)
            .chat
            .push_chat(role, text);
    }

    fn set_status(&mut self, cx: &mut Cx, status: UiStatus, text: &str) {
        if let Some(key) = self.workspace_state.active_key().cloned() {
            self.set_session_status(cx, &key, status, text);
        } else {
            self.apply_status_ui(cx, status, text);
        }
    }

    fn set_session_status(&mut self, cx: &mut Cx, key: &SessionKey, status: UiStatus, text: &str) {
        if let Some(runtime) = self.session_runtimes.get_mut(key) {
            runtime.status = status;
            runtime.status_text = text.to_string();
        }
        set_session_working(&key.work_dir, &key.session_id, status == UiStatus::Working);
        if self.workspace_state.is_active(key) {
            self.apply_status_ui(cx, status, text);
        }
    }

    fn restore_active_status(&mut self, cx: &mut Cx) {
        let Some(key) = self.workspace_state.active_key().cloned() else {
            self.apply_status_ui(cx, UiStatus::Ready, "Ready");
            return;
        };
        let (status, text) = self
            .session_runtimes
            .get(&key)
            .map(|runtime| (runtime.status, runtime.status_text.clone()))
            .unwrap_or((UiStatus::Ready, String::new()));
        self.apply_status_ui(cx, status, &text);
    }

    fn apply_status_ui(&mut self, cx: &mut Cx, status: UiStatus, text: &str) {
        let composer_status = match status {
            UiStatus::Ready => ComposerStatus::Ready,
            UiStatus::Working => ComposerStatus::Working,
            UiStatus::Error => ComposerStatus::Error,
        };
        self.composer_state.set_status(composer_status, text);
        self.busy = status == UiStatus::Working;
        let working = status == UiStatus::Working;
        let has_generation = self
            .workspace_state
            .active_key()
            .and_then(|key| self.session_runtimes.get(key))
            .is_some_and(|runtime| runtime.generation.is_some());
        self.ui
            .widget(cx, ids!(chat_working_indicator))
            .set_visible(cx, working);
        self.ui
            .widget(cx, ids!(stop_btn))
            .set_visible(cx, working && has_generation);
        self.ui.widget(cx, ids!(send_btn)).set_visible(cx, !working);
        self.ui.label(cx, ids!(composer_status)).set_text(cx, text);
        self.ui
            .label(cx, ids!(composer_status))
            .set_visible(cx, working || status == UiStatus::Error);
        self.ui.widget(cx, ids!(chat_working_indicator)).redraw(cx);
        self.apply_composer_presentation(cx);
    }

    fn apply_composer_presentation(&mut self, cx: &mut Cx) {
        let presentation = self.composer_state.presentation();
        self.ui
            .widget(cx, ids!(composer_status))
            .set_visible(cx, presentation.working || presentation.show_error);
        self.ui
            .label(cx, ids!(composer_status))
            .set_text(cx, &presentation.status_text);
        self.ui
            .widget(cx, ids!(effort_picker))
            .set_visible(cx, presentation.show_model);
        self.ui
            .widget(cx, ids!(model_picker))
            .set_visible(cx, presentation.show_model);

        self.ui
            .button(cx, ids!(attach_btn))
            .set_visible(cx, !presentation.working);
        self.ui
            .button(cx, ids!(send_btn))
            .set_visible(cx, !presentation.working);
        let has_generation = self
            .workspace_state
            .active_key()
            .and_then(|key| self.session_runtimes.get(key))
            .is_some_and(|runtime| runtime.generation.is_some());
        self.ui
            .button(cx, ids!(stop_btn))
            .set_visible(cx, presentation.working && has_generation);
    }

    fn apply_session_context_action(
        &mut self,
        cx: &mut Cx,
        action: fn(&SessionEntry) -> bool,
        action_name: &str,
    ) {
        let Some(entry) = self.session_context_entry.take() else {
            return;
        };
        if let Some(mut menu) = self
            .ui
            .widget(cx, ids!(session_context_menu))
            .borrow_mut::<SessionContextMenu>()
        {
            menu.close(cx);
        }

        if is_session_working(&entry.work_dir, &entry.id) {
            self.push_chat(
                MsgRole::System,
                format!("Stop session `{}` before modifying it.", entry.title),
            );
            cx.redraw_all();
            return;
        }

        if !action(&entry) {
            self.push_chat(
                MsgRole::System,
                format!(
                    "Could not {} session `{}`.",
                    action_name.to_lowercase(),
                    entry.title
                ),
            );
            cx.redraw_all();
            return;
        }
        let was_active = active_session_entry()
            .is_some_and(|active| active.id == entry.id && active.work_dir == entry.work_dir);
        let removed_key = SessionKey::new(entry.work_dir.clone(), entry.id.clone());
        self.workspace_state.remove(&removed_key);
        self.session_runtimes.remove(&removed_key);
        self.refresh_registered_sessions();

        if was_active {
            let fallback = {
                let data = crate::panels::sessions::state::SESSIONS_DATA
                    .read()
                    .unwrap();
                data.projects
                    .iter()
                    .find(|project| project.work_dir == entry.work_dir)
                    .and_then(|project| project.sessions.first())
                    .cloned()
            };
            if let Some(fallback) = fallback {
                self.activate_session(cx, fallback);
                return;
            }
            self.select_project_draft(cx, entry.work_dir.clone());
        }
        self.push_chat(
            MsgRole::System,
            format!("{} session `{}`.", action_name, entry.title),
        );
        cx.redraw_all();
    }

    fn create_and_activate_session(&mut self, cx: &mut Cx, work_dir: PathBuf) {
        let Some(entry) = create_new_session(&work_dir) else {
            self.push_chat(MsgRole::System, "Could not create a new session file.");
            cx.redraw_all();
            return;
        };
        self.refresh_registered_sessions();
        self.activate_session(cx, entry);
    }

    fn activate_session(&mut self, cx: &mut Cx, mut entry: SessionEntry) {
        entry.work_dir =
            std::fs::canonicalize(&entry.work_dir).unwrap_or_else(|_| entry.work_dir.clone());
        if !self.is_attached_project(&entry.work_dir) {
            self.push_chat(
                MsgRole::System,
                format!(
                    "Attach `{}` before opening its sessions.",
                    entry.work_dir.display()
                ),
            );
            cx.redraw_all();
            return;
        }

        let key = SessionKey::new(entry.work_dir.clone(), entry.id.clone());
        self.select_workspace_ui(cx, entry.work_dir.clone(), entry.id.clone());
        set_active_session(&entry.work_dir, &entry.id);
        if let Some(registry) = self.project_registry.as_mut() {
            if let Err(error) = registry.remember_selection(&entry.work_dir, Some(&entry.id)) {
                self.push_chat_to(
                    key.clone(),
                    MsgRole::System,
                    format!("Could not update recent-project state: {error}"),
                );
            }
        }

        if !self.session_runtimes.contains_key(&key) {
            let (api_key, account_id) = self.current_credentials(cx);
            let selected_model = self.ui.drop_down(cx, ids!(model_drop)).selected_label();
            let model = if selected_model.is_empty() {
                "gpt-5.6-luna".to_string()
            } else {
                selected_model
            };
            let reasoning_effort = ReasoningEffort::from_label(
                &self.ui.drop_down(cx, ids!(effort_drop)).selected_label(),
            )
            .unwrap_or_default();
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key,
                account_id,
                model: model.clone(),
                work_dir: entry.work_dir.clone(),
                session_file: Some(entry.session_file.clone()),
            });
            let messages = agent.session_tree.get_active_branch_messages();
            self.session_runtimes.insert(
                key.clone(),
                SessionRuntime::new(agent, model, reasoning_effort),
            );
            self.workspace_state
                .workspace_mut(key.clone())
                .chat
                .replace_from_agent_messages(&messages);
        }

        if let Some((model, reasoning_effort)) = self
            .session_runtimes
            .get(&key)
            .map(|runtime| (runtime.model.clone(), runtime.reasoning_effort))
        {
            self.set_model_dropup_options(cx, self.available_models.clone(), &model);
            self.set_reasoning_effort_picker(cx, reasoning_effort);
        }

        self.refresh_project_capabilities(cx, &entry.work_dir);
        self.restore_active_status(cx);
        cx.redraw_all();
    }

    fn build_cmd_items(&mut self, cx: &mut Cx) {
        let cti = self.ui.mypi_command_text_input(cx, ids!(prompt_input));
        let search = cti.search_text(cx).to_lowercase();
        let commands = self
            .commands
            .iter()
            .filter(|cmd| search.is_empty() || cmd.name.to_lowercase().starts_with(&search))
            .cloned()
            .collect();
        cti.set_items(cx, commands);
    }

    fn dispatch_input(&mut self, cx: &mut Cx, input_text: String, show_in_chat: bool) {
        let attachments = if show_in_chat {
            self.workspace_state
                .active_workspace()
                .map(|workspace| workspace.ui.attachments.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let (api_key, account_id) = self.current_credentials(cx);
        if api_key.is_empty() {
            self.push_chat(
                MsgRole::System,
                "Please provide an OpenAI API key or click 'Login ChatGPT' to authenticate.",
            );
            cx.redraw_all();
            return;
        }

        let selected_model = self.ui.drop_down(cx, ids!(model_drop)).selected_label();
        let model_name = if selected_model.is_empty() {
            "gpt-5.6-luna".to_string()
        } else {
            selected_model
        };
        let reasoning_effort =
            ReasoningEffort::from_label(&self.ui.drop_down(cx, ids!(effort_drop)).selected_label())
                .unwrap_or_default();
        let Some(active_key) = self.workspace_state.active_key().cloned() else {
            return;
        };
        let work_dir = active_key.work_dir.clone();

        if show_in_chat
            && active_session_entry().is_none()
            && input_text.trim_start().starts_with('/')
        {
            self.push_chat(
                MsgRole::System,
                "Select an existing session before running a session command.",
            );
            cx.redraw_all();
            return;
        }

        if show_in_chat && active_key.is_draft() {
            self.save_active_draft(cx);
            let Some(entry) = create_new_session(&work_dir) else {
                self.push_chat(MsgRole::System, "Could not create a new session file.");
                cx.redraw_all();
                return;
            };
            self.refresh_registered_sessions();
            set_active_session(&entry.work_dir, &entry.id);
            let key = SessionKey::new(entry.work_dir.clone(), entry.id);
            self.workspace_state
                .move_workspace(&active_key, key.clone());
            self.select_workspace_ui(cx, entry.work_dir.clone(), key.session_id.clone());
            self.session_runtimes.remove(&active_key);
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key: api_key.clone(),
                account_id: account_id.clone(),
                model: model_name.clone(),
                work_dir: entry.work_dir,
                session_file: Some(entry.session_file),
            });
            self.session_runtimes.insert(
                key,
                SessionRuntime::new(agent, model_name.clone(), reasoning_effort),
            );
        }

        let Some(key) = self.workspace_state.active_key().cloned() else {
            return;
        };
        if !self.session_runtimes.contains_key(&key) {
            let session_file = active_session_entry().map(|entry| entry.session_file);
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key,
                account_id,
                model: model_name.clone(),
                work_dir: key.work_dir.clone(),
                session_file,
            });
            self.session_runtimes.insert(
                key.clone(),
                SessionRuntime::new(agent, model_name.clone(), reasoning_effort),
            );
        }

        if let Some(runtime) = self.session_runtimes.get_mut(&key) {
            runtime.reasoning_effort = reasoning_effort;
        }

        if let Some(model) = input_text.trim().strip_prefix("/model ") {
            if !model.trim().is_empty() {
                if let Some(runtime) = self.session_runtimes.get_mut(&key) {
                    runtime.model = model.trim().to_string();
                }
            }
        }

        let Some(tx) = self.tx.clone() else { return };
        let agent_arc = self.session_runtimes[&key].agent.clone();
        let (submitted_draft, input_str) = match submitted_draft(&input_text) {
            Some(draft) => draft,
            None if !attachments.is_empty() => (input_text.clone(), String::new()),
            None => return,
        };
        self.next_generation_id = self.next_generation_id.wrapping_add(1);
        let generation_id = self.next_generation_id;

        if show_in_chat {
            let attachment_names = attachments
                .iter()
                .map(|attachment| attachment.display_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let visible_input = if attachment_names.is_empty() {
                input_str.clone()
            } else if input_str.is_empty() {
                format!("Attached: {attachment_names}")
            } else {
                format!("{input_str}\n\nAttached: {attachment_names}")
            };
            self.push_chat(MsgRole::User, visible_input);
        }
        let chat_list = self.ui.widget(cx, ids!(chat_list));
        chat_list.portal_list(cx, ids!(list)).set_tail_range(true);
        cx.redraw_all();

        let event_work_dir = key.work_dir.clone();
        let event_session_id = key.session_id.clone();
        let generation_attachments = attachments.clone();
        let generation_handle = get_runtime().spawn(async move {
            let mut agent_lock = agent_arc.lock().await;
            agent_lock.set_reasoning_effort(reasoning_effort).await;

            // Poll input and its event stream in one task. This keeps event
            // forwarding scoped to the generation and preserves terminal order.
            let mut event_rx = agent_lock.subscribe();
            let input_future =
                agent_lock.handle_input_with_images(&input_str, generation_attachments);
            tokio::pin!(input_future);
            let output = loop {
                tokio::select! {
                    output = &mut input_future => break output,
                    result = event_rx.recv() => match result {
                        Ok(event) => {
                            let _ = tx.send(GuiAgentEvent::GenerationAgent {
                                generation_id,
                                work_dir: event_work_dir.clone(),
                                session_id: event_session_id.clone(),
                                event,
                            });
                            SignalToUI::set_ui_signal();
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break None,
                    }
                }
            };
            while let Ok(event) = event_rx.try_recv() {
                let _ = tx.send(GuiAgentEvent::GenerationAgent {
                    generation_id,
                    work_dir: event_work_dir.clone(),
                    session_id: event_session_id.clone(),
                    event,
                });
                SignalToUI::set_ui_signal();
            }

            if let Some(out) = output {
                let _ = tx.send(GuiAgentEvent::CommandOutput {
                    generation_id,
                    work_dir: event_work_dir.clone(),
                    session_id: event_session_id.clone(),
                    output: out,
                });
                SignalToUI::set_ui_signal();
            }
            let _ = tx.send(GuiAgentEvent::GenerationFinished {
                generation_id,
                work_dir: event_work_dir,
                session_id: event_session_id,
            });
            SignalToUI::set_ui_signal();
        });
        if let Some(runtime) = self.session_runtimes.get_mut(&key) {
            runtime.generation = Some(GenerationRun {
                id: generation_id,
                handle: generation_handle,
            });
            runtime.terminal_generation_id = None;
            runtime.submitted_draft = Some((generation_id, submitted_draft));
            runtime.submitted_attachments = Some((generation_id, attachments));
        }
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            workspace.ui.draft.clear();
            workspace.ui.attachments.clear();
        }
        self.refresh_attachment_ui(cx);
        self.ui
            .mypi_command_text_input(cx, ids!(prompt_input))
            .reset(cx);
        self.set_session_status(cx, &key, UiStatus::Working, "Working...");
    }

    fn spawn_model_fetch(&self, api_key: String, account_id: Option<String>) {
        if api_key.is_empty() {
            return;
        }
        let Some(tx) = self.tx.clone() else { return };
        get_runtime().spawn(async move {
            let models = fetch_available_models(&api_key, account_id.as_deref()).await;
            let _ = tx.send(GuiAgentEvent::AvailableModelsLoaded(models));
            SignalToUI::set_ui_signal();
        });
    }

    fn handle_agent_event(
        &mut self,
        cx: &mut Cx,
        event: AgentEvent,
        generation: Option<(SessionKey, u64)>,
    ) {
        let target_key = generation
            .as_ref()
            .map(|(key, _)| key.clone())
            .or_else(|| self.workspace_state.active_key().cloned());

        match event {
            AgentEvent::AgentStart => {
                if let Some(key) = target_key {
                    self.set_session_status(cx, &key, UiStatus::Working, "Working...");
                }
            }
            AgentEvent::MessageUpdate {
                text_delta,
                reasoning_delta,
                ..
            } => {
                let Some(key) = target_key else { return };
                let workspace = self.workspace_state.workspace_mut(key);
                if let Some(delta) = reasoning_delta {
                    workspace
                        .chat
                        .push_stream_delta(crate::state::StreamingKind::Thinking, &delta);
                }
                if let Some(delta) = text_delta {
                    workspace
                        .chat
                        .push_stream_delta(crate::state::StreamingKind::Assistant, &delta);
                }
            }
            AgentEvent::MessageEnd { message } => {
                let Some(key) = target_key else { return };
                let workspace = self.workspace_state.workspace_mut(key);
                if matches!(
                    message,
                    mypi_agent::AgentMessage::Assistant {
                        tool_calls: Some(_),
                        ..
                    }
                ) {
                    workspace.chat.flush_tool_call_preamble();
                } else {
                    workspace.chat.flush_streaming();
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                name,
                arguments,
            } => {
                let Some(key) = target_key else { return };
                self.workspace_state.workspace_mut(key).chat.push_tool(
                    tool_call_id,
                    name,
                    arguments,
                );
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
            } => {
                let Some(key) = target_key else { return };
                self.workspace_state.workspace_mut(key).chat.update_tool(
                    &tool_call_id,
                    partial_result,
                    None,
                );
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                ..
            } => {
                let Some(key) = target_key else { return };
                self.workspace_state.workspace_mut(key).chat.update_tool(
                    &tool_call_id,
                    result.content,
                    Some(if result.is_error {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Done
                    }),
                );
            }
            AgentEvent::TurnEnd { .. } => {
                if let Some(key) = target_key {
                    self.set_session_status(cx, &key, UiStatus::Working, "Turn completed");
                }
            }
            // AgentEnd closes one agent loop, but CodingAgent may still run
            // hooks or scheduled work. GenerationFinished is the terminal event.
            AgentEvent::AgentEnd { .. } if generation.is_none() => return,
            AgentEvent::AgentEnd { .. } => {
                let Some((key, id)) = generation else { return };
                let accepted = self.session_runtimes.get(&key).is_some_and(|runtime| {
                    accepts_generation_event(
                        runtime.generation.as_ref().map(|generation| generation.id),
                        runtime.terminal_generation_id,
                        id,
                        GenerationEvent::AgentEnd,
                    )
                });
                if !accepted {
                    return;
                }
                self.workspace_state
                    .workspace_mut(key.clone())
                    .chat
                    .flush_streaming();
                self.set_session_status(cx, &key, UiStatus::Working, "Finishing...");
            }
            AgentEvent::AgentError { error } => {
                let Some((key, id)) = generation else {
                    self.push_chat(MsgRole::System, format!("Agent error: {error}"));
                    let status = concise_status(&error);
                    self.set_status(cx, UiStatus::Error, &status);
                    return;
                };
                let accepted = self.session_runtimes.get(&key).is_some_and(|runtime| {
                    accepts_generation_event(
                        runtime.generation.as_ref().map(|generation| generation.id),
                        runtime.terminal_generation_id,
                        id,
                        GenerationEvent::AgentError,
                    )
                });
                if !accepted {
                    return;
                }

                let (restored_draft, restored_attachments) = self
                    .session_runtimes
                    .get_mut(&key)
                    .map(|runtime| {
                        runtime.generation = None;
                        runtime.terminal_generation_id = None;
                        let draft = runtime
                            .submitted_draft
                            .take()
                            .filter(|(draft_id, _)| *draft_id == id)
                            .map(|(_, draft)| draft);
                        let attachments = runtime
                            .submitted_attachments
                            .take()
                            .filter(|(attachment_id, _)| *attachment_id == id)
                            .map(|(_, attachments)| attachments)
                            .unwrap_or_default();
                        (draft, attachments)
                    })
                    .unwrap_or_default();
                let is_active = self.workspace_state.is_active(&key);
                let current_draft = if is_active {
                    self.ui
                        .mypi_command_text_input(cx, ids!(prompt_input))
                        .text_input_ref(cx)
                        .text()
                } else {
                    self.workspace_state
                        .workspace(&key)
                        .map(|workspace| workspace.ui.draft.clone())
                        .unwrap_or_default()
                };
                let draft = if current_draft.trim().is_empty() {
                    restored_draft.unwrap_or_default()
                } else {
                    current_draft
                };
                let workspace = self.workspace_state.workspace_mut(key.clone());
                workspace.chat.flush_streaming();
                workspace.ui.draft = draft.clone();
                workspace.ui.attachments = restored_attachments;
                workspace
                    .chat
                    .push_chat(MsgRole::System, format!("Agent error: {error}"));
                if is_active {
                    self.ui
                        .mypi_command_text_input(cx, ids!(prompt_input))
                        .text_input_ref(cx)
                        .set_text(cx, &draft);
                    self.refresh_attachment_ui(cx);
                }
                let status = concise_status(&error);
                self.set_session_status(cx, &key, UiStatus::Error, &status);
            }
            AgentEvent::TurnStart { .. } | AgentEvent::MessageStart { .. } => {}
        }
    }

    pub fn poll_agent_events(&mut self, cx: &mut Cx) {
        let mut events = Vec::new();
        if let Some(rx_arc) = &self.rx {
            if let Ok(rx) = rx_arc.lock() {
                while let Ok(evt) = rx.try_recv() {
                    events.push(evt);
                }
            }
        }
        if events.is_empty() {
            return;
        }

        for evt in events {
            match evt {
                GuiAgentEvent::TaskEvent(task_event) => {
                    self.handle_agent_event(cx, task_event.event, None);
                }
                GuiAgentEvent::CommandOutput {
                    generation_id,
                    work_dir,
                    session_id,
                    output,
                } => {
                    let key = SessionKey::new(work_dir, session_id);
                    let is_current_generation =
                        self.session_runtimes.get(&key).is_some_and(|runtime| {
                            accepts_generation_event(
                                runtime.generation.as_ref().map(|generation| generation.id),
                                runtime.terminal_generation_id,
                                generation_id,
                                GenerationEvent::CommandOutput,
                            )
                        });
                    if !is_current_generation {
                        continue;
                    }
                    self.workspace_state
                        .workspace_mut(key)
                        .chat
                        .push_chat(MsgRole::System, output);
                }
                GuiAgentEvent::GenerationFinished {
                    generation_id,
                    work_dir,
                    session_id,
                } => {
                    let key = SessionKey::new(work_dir, session_id);
                    let is_current = self
                        .session_runtimes
                        .get(&key)
                        .and_then(|runtime| runtime.generation.as_ref())
                        .is_some_and(|generation| generation.id == generation_id);
                    if !is_current {
                        continue;
                    }
                    if let Some(runtime) = self.session_runtimes.get_mut(&key) {
                        runtime.generation = None;
                        runtime.terminal_generation_id = None;
                        runtime.submitted_draft = None;
                        runtime.submitted_attachments = None;
                    }
                    self.workspace_state
                        .workspace_mut(key.clone())
                        .chat
                        .flush_streaming();
                    self.set_session_status(cx, &key, UiStatus::Ready, "Ready");

                    self.refresh_registered_sessions();
                }

                GuiAgentEvent::AvailableModelsLoaded(models) => {
                    let selected_model = self.ui.drop_down(cx, ids!(model_drop)).selected_label();
                    self.set_model_dropup_options(cx, models, &selected_model);
                }
                GuiAgentEvent::ProjectFolderPicked(result) => {
                    self.apply_project_folder_result(cx, result);
                }
                GuiAgentEvent::DeviceCodePrompt { user_code, url } => {
                    if let Some(key) = self.auth_workspace.clone() {
                        self.push_chat_to(
                            key.clone(),
                            MsgRole::System,
                            format!(
                                "Sign in: open {url} in your browser and enter code {user_code} \
                                 (waiting for authorization...)"
                            ),
                        );
                        if self.workspace_state.is_active(&key) {
                            self.apply_status_ui(
                                cx,
                                UiStatus::Working,
                                &format!("Enter code {user_code}"),
                            );
                        }
                    }
                }
                GuiAgentEvent::DeviceLoginSuccess => {
                    let mut key_opt = None;
                    let mut acc_opt = None;
                    if let Some(creds) = auth::load_credentials() {
                        self.ui
                            .text_input(cx, ids!(api_key_input))
                            .set_text(cx, &creds.access_token);
                        key_opt = Some(creds.access_token.clone());
                        acc_opt = creds.account_id;
                    }
                    if let Some(key) = self.auth_workspace.take() {
                        self.push_chat_to(
                            key.clone(),
                            MsgRole::System,
                            "Successfully authenticated with ChatGPT.",
                        );
                        if self.workspace_state.is_active(&key) {
                            self.restore_active_status(cx);
                        }
                    }
                    self.ui.widget(cx, ids!(auth_row)).set_visible(cx, false);

                    if let Some(key) = key_opt {
                        self.spawn_model_fetch(key, acc_opt);
                    }
                }
                GuiAgentEvent::DeviceLoginError(error) => {
                    if let Some(key) = self.auth_workspace.take() {
                        self.push_chat_to(
                            key.clone(),
                            MsgRole::System,
                            format!("Authentication error: {error}"),
                        );
                        if self.workspace_state.is_active(&key) {
                            self.apply_status_ui(cx, UiStatus::Error, &concise_status(&error));
                        }
                    }
                }
                GuiAgentEvent::GenerationAgent {
                    generation_id,
                    work_dir,
                    session_id,
                    event: agent_event,
                } => {
                    let key = SessionKey::new(work_dir, session_id);
                    let is_current = self
                        .session_runtimes
                        .get(&key)
                        .and_then(|runtime| runtime.generation.as_ref())
                        .is_some_and(|generation| generation.id == generation_id);
                    if !is_current {
                        continue;
                    }
                    self.handle_agent_event(cx, agent_event, Some((key, generation_id)))
                }
                GuiAgentEvent::Agent(agent_event) => self.handle_agent_event(cx, agent_event, None),
            }
        }

        cx.redraw_all();
    }
}

#[cfg(test)]
mod workspace_header_tests {
    use super::{compact_workspace_path, project_name};
    use std::path::Path;

    #[test]
    fn workspace_header_uses_final_directory_as_project_name() {
        assert_eq!(project_name(Path::new("/Users/alex/code/mypi")), "mypi");
    }

    #[test]
    fn workspace_header_uses_display_path_when_project_has_no_final_directory() {
        assert_eq!(project_name(Path::new("/")), "/");
    }

    #[test]
    fn workspace_header_shortens_paths_below_home() {
        assert_eq!(
            compact_workspace_path(
                Path::new("/Users/alex/Documents/mypi"),
                Some(Path::new("/Users/alex")),
            ),
            "~/Documents/mypi"
        );
    }

    #[test]
    fn workspace_header_preserves_home_path_when_nothing_would_be_omitted() {
        assert_eq!(
            compact_workspace_path(
                Path::new("/Users/alex/Documents/exploration/mypi"),
                Some(Path::new("/Users/alex")),
            ),
            "~/Documents/exploration/mypi"
        );
    }

    #[test]
    fn workspace_header_compacts_the_middle_of_long_paths() {
        assert_eq!(
            compact_workspace_path(
                Path::new("/Users/alex/Documents/code/client/exploration/mypi"),
                Some(Path::new("/Users/alex")),
            ),
            "~/Documents/…/exploration/mypi"
        );
    }

    #[test]
    fn workspace_header_preserves_short_absolute_paths() {
        assert_eq!(
            compact_workspace_path(Path::new("/work/mypi"), None),
            "/work/mypi"
        );
    }

    #[test]
    fn workspace_header_does_not_expand_relative_paths_to_home() {
        assert_eq!(
            compact_workspace_path(Path::new("home/project"), Some(Path::new("home"))),
            "home/project"
        );
    }

    #[test]
    fn workspace_header_preserves_root() {
        assert_eq!(compact_workspace_path(Path::new("/"), None), "/");
    }
}
