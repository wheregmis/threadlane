//! App shell: script_mod! DSL, startup/auth wiring, agent event pump.
//!
//! Chat, sessions, and command palette panels are modularized under `crate::panels`.

use crate::panels::chat::{
    accepts_generation_event, concise_status, draft_for_cancellation, submitted_draft, ChatList,
    ComposerState, ComposerStatus, GenerationEvent, ToolFoldHeader,
};
use crate::panels::command_palette::*;

use crate::panels::sessions::{SessionContextMenu, SessionContextMenuAction, SessionList};
use crate::state::{
    active_session_entry, archive_session, builtin_commands, create_new_session, delete_session,
    is_session_working, project_work_dir_at_row, refresh_sessions, session_entry_at_row,
    set_active_session, set_session_context_target, set_session_working, truncate_chars,
    CommandInfo, GuiAgentEvent, MsgRole, SessionEntry, ToolStatus,
};
use crate::workspace::{AppState, SessionKey};
use makepad_widgets::text::selection::Cursor;
use makepad_widgets::*;
use mypi_agent::{get_runtime, AgentEvent, ReasoningEffort};
use mypi_coding_agent::{
    discover_agents, AgentConfig, AgentScope, CodingAgent, CodingAgentOptions, ProjectContext,
    SkillMetadata,
};
use mypi_provider::auth;
use mypi_provider::openai::fetch_available_models;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

script_mod! {
    use mod.prelude.widgets.*
    use mod.components.*

    let ComposerDropDown = DropDown {
        width: Fill
        height: Fill
        margin: 0
        padding: Inset{left: 10 right: 24}
        draw_bg +: {
            color: #x232830
            color_hover: #x2a313c
            color_focus: #x2f3a4d
            color_down: #x354153
            border_color: #x3a424e
            border_color_hover: #x4a5564
            border_color_focus: #x6fa8ff
            border_color_down: #x6fa8ff
            border_size: 1.0
            border_radius: 6.0
            arrow_color: #x7f8b9a
            arrow_color_hover: #xc7cdd6
            arrow_color_focus: #xc7cdd6
            arrow_color_down: #xffffff
        }
        draw_text +: {
            color: #xc7cdd6
            color_hover: #xdde3ea
            color_focus: #xdde3ea
            color_down: #xffffff
            text_style +: { font_size: 9.5 }
        }
        popup_menu: PopupMenuFlat {
            width: 220
            draw_bg +: {
                color: #x242932
                border_color: #x454e5b
                border_size: 1.0
                border_radius: 7.0
            }
            menu_item: PopupMenuItem {
                draw_text +: {
                    color: #xc9d0da
                    color_hover: #xffffff
                    color_active: #xffffff
                    text_style +: { font_size: 10.0 }
                }
                draw_bg +: {
                    color: #x00000000
                    color_hover: #x303844
                    color_active: #x354153
                    mark_color: #x00000000
                    mark_color_active: #x6fa8ff
                }
            }
        }
    }

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
    let SessionList = #(SessionList::register_widget(vm)) {
        width: Fill
        height: Fill

        list := PortalList {
            width: Fill
            height: Fill
            flow: Down
            drag_scrolling: true

            ProjectHeader := View {
                width: Fill
                height: 28
                flow: Right
                spacing: 7
                align: Align{y: 0.5}
                padding: Inset{left: 10 top: 3 right: 5 bottom: 3}
                Icon {
                    width: 15
                    height: 15
                    icon_walk: Walk{width: 15 height: 15}
                    draw_icon +: {
                        svg: crate_resource("self:resources/icons/folder.svg")
                        color: #x8090a3
                    }
                }
                name_lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #xb2bbc7
                        text_style: theme.font_bold { font_size: 9.75 }
                    }
                }
                new_project_session_btn := Button {
                    width: 26
                    height: 24
                    margin: 0
                    padding: 0
                    text: ""
                    align: Align{x: 0.5 y: 0.5}
                    icon_walk: Walk{width: 12 height: 12}
                    draw_icon +: {
                        svg: crate_resource("self:resources/icons/plus.svg")
                        color: #xaeb8c5
                    }
                    draw_bg +: {
                        color: #x00000000
                        color_hover: #x2a313c
                        color_focus: #x2a313c
                        color_down: #x354153
                        border_color: #x00000000
                        border_color_hover: #x4a5564
                        border_color_focus: #x4a5564
                        border_color_down: #x6fa8ff
                        border_size: 1.0
                        border_radius: 6.0
                    }
                }
            }

            SessionRow := SessionRowBase {}

            SessionRowActive := SessionRowBase {
                draw_bg +: {
                    color: #x3a424e
                    color_hover: #x46505d
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
                padding: Inset{left: 22 top: 4 right: 10 bottom: 8}
                lbl +: {
                    text: "No sessions yet"
                    draw_text +: { color: #x555d6a text_style +: { font_size: 11.0 } }
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
                            spacing: 2
                            padding: Inset{left: 8 top: 10 right: 8 bottom: 10}

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
                            padding: Inset{left: 2 top: 0 right: 2 bottom: 2}

                            Label {
                                text: "mypi"
                                draw_text +: {
                                    color: #xdde3ea
                                    text_style: theme.font_bold { font_size: 14.0 }
                                }
                            }
                            workspace_label := Label {
                                text: ""
                                draw_text +: {
                                    color: #x6f7a88
                                    text_style +: { font_size: 10.0 }
                                }
                            }
                            FlexSpacer {}
                            caps_btn := Button {
                                width: Fit
                                height: 28
                                text: "Capabilities"
                                draw_bg +: {
                                    color: #x232830
                                    color_hover: #x2a313c
                                    color_down: #x3a424e
                                    border_color: #x3a424e
                                    border_radius: 6.0
                                    border_size: 1.0
                                }
                                draw_text +: {
                                    color: #xc7cdd6
                                    text_style +: { font_size: 10.0 }
                                }
                            }

                            status_pill := View {
                                width: 16
                                height: 20
                                visible: false
                                flow: Down
                                spacing: 2
                                align: Align{x: 0.5 y: 0.5}
                                progress_dot_1 := ProgressDot {
                                    visible: false
                                }
                                progress_dot_2 := ProgressDot {
                                    visible: false
                                    draw_bg +: { color: #xaeb6c2 }
                                }
                                progress_dot_3 := ProgressDot {
                                    visible: false
                                    draw_bg +: { color: #x6f7a88 }
                                }
                                error_dot := RoundedView {
                                    width: 5
                                    height: 5
                                    visible: false
                                    draw_bg +: { color: #xe5534b border_radius: 2.5 }
                                }
                            }
                        }

                        auth_row := RoundedView {
                            width: Fill
                            height: Fit
                            visible: false
                            flow: Right
                            spacing: 8
                            align: Align{y: 0.5}
                            padding: 10
                            draw_bg +: {
                                color: #x262133
                                border_radius: 8.0
                                border_size: 1.0
                                border_color: #x4a3c55
                            }
                            Label {
                                text: "Not signed in"
                                draw_text +: {
                                    color: #xc7cdd6
                                    text_style +: { font_size: 10.5 }
                                }
                            }
                            api_key_input := TextInput {
                                width: Fill
                                height: 32
                                empty_text: "OpenAI API key (or sign in with ChatGPT)"
                            }
                            login_btn := Button {
                                width: 130
                                height: 32
                                text: "Login ChatGPT"
                            }
                        }

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
    status: UiStatus,
    status_text: String,
    model: String,
    reasoning_effort: ReasoningEffort,
}

impl SessionRuntime {
    fn new(agent: CodingAgent, model: String, reasoning_effort: ReasoningEffort) -> Self {
        Self {
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            generation: None,
            terminal_generation_id: None,
            submitted_draft: None,
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

        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        self.select_workspace(work_dir.clone(), "draft");

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

        self.ui
            .label(cx, ids!(workspace_label))
            .set_text(cx, &work_dir.display().to_string());
        refresh_sessions(&work_dir);
        let context = ProjectContext::discover(&work_dir);

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
            session_file: active_session_entry().map(|e| e.session_file),
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
                "{} skills · {} agents",
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

        let draft_key = SessionKey::new(work_dir, "draft");
        self.session_runtimes.insert(
            draft_key,
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
                let restored_draft = self.session_runtimes.get_mut(&key).and_then(|runtime| {
                    let generation = runtime.generation.take()?;
                    let generation_id = generation.id;
                    generation.handle.abort();
                    runtime.terminal_generation_id = None;
                    let draft = draft_for_cancellation(
                        Some(generation_id),
                        runtime.submitted_draft.as_ref(),
                        generation_id,
                    );
                    runtime.submitted_draft = None;
                    draft
                });
                let draft = if current_draft.trim().is_empty() {
                    restored_draft.unwrap_or_default()
                } else {
                    current_draft
                };
                if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                    workspace.ui.draft = draft.clone();
                }
                self.ui
                    .mypi_command_text_input(cx, ids!(prompt_input))
                    .text_input_ref(cx)
                    .set_text(cx, &draft);
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
            if !input_text.trim().is_empty() {
                self.dispatch_input(cx, input_text, true);
            }
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        crate::components::script_mod(vm);
        crate::panels::command_palette::view::script_mod(vm);
        crate::panels::chat::components::script_mod(vm);
        crate::panels::sessions::components::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
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
        let draft = self
            .workspace_state
            .active_workspace()
            .map(|workspace| workspace.ui.draft.clone())
            .unwrap_or_default();
        self.ui
            .mypi_command_text_input(cx, ids!(prompt_input))
            .text_input_ref(cx)
            .set_text(cx, &draft);
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
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let was_active = active_session_entry()
            .is_some_and(|active| active.id == entry.id && active.work_dir == entry.work_dir);
        let removed_key = SessionKey::new(entry.work_dir.clone(), entry.id.clone());
        self.workspace_state.remove(&removed_key);
        self.session_runtimes.remove(&removed_key);
        refresh_sessions(&cwd);

        if was_active {
            if let Some(fallback) = active_session_entry() {
                self.activate_session(cx, fallback);
                return;
            }
            self.select_workspace_ui(cx, cwd, "draft".to_string());
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
        self.activate_session(cx, entry);
    }

    fn activate_session(&mut self, cx: &mut Cx, entry: SessionEntry) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        if entry.work_dir != cwd {
            self.push_chat(
                MsgRole::System,
                format!(
                    "Session `{}` belongs to another project ({}). \
                     Cross-project agent switch is not wired yet — open that folder to use it.",
                    entry.title,
                    entry.work_dir.display()
                ),
            );
            cx.redraw_all();
            return;
        }

        let key = SessionKey::new(entry.work_dir.clone(), entry.id.clone());
        self.select_workspace_ui(cx, entry.work_dir.clone(), entry.id.clone());
        set_active_session(&entry.work_dir, &entry.id);

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
        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        if show_in_chat && active_session_entry().is_none() {
            let Some(entry) = create_new_session(&work_dir) else {
                self.push_chat(MsgRole::System, "Could not create a new session file.");
                cx.redraw_all();
                return;
            };
            set_active_session(&entry.work_dir, &entry.id);
            self.select_workspace_ui(cx, entry.work_dir.clone(), entry.id.clone());
            let key = SessionKey::new(entry.work_dir.clone(), entry.id);
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
        let Some((submitted_draft, input_str)) = submitted_draft(&input_text) else {
            return;
        };
        self.next_generation_id = self.next_generation_id.wrapping_add(1);
        let generation_id = self.next_generation_id;

        if show_in_chat {
            self.push_chat(MsgRole::User, input_str.clone());
        }
        let chat_list = self.ui.widget(cx, ids!(chat_list));
        chat_list.portal_list(cx, ids!(list)).set_tail_range(true);
        cx.redraw_all();

        let event_work_dir = key.work_dir.clone();
        let event_session_id = key.session_id.clone();
        let generation_handle = get_runtime().spawn(async move {
            let mut agent_lock = agent_arc.lock().await;
            agent_lock.set_reasoning_effort(reasoning_effort).await;

            // Poll input and its event stream in one task. This keeps event
            // forwarding scoped to the generation and preserves terminal order.
            let mut event_rx = agent_lock.subscribe();
            let input_future = agent_lock.handle_input(&input_str);
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
        }
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            workspace.ui.draft.clear();
        }
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

                let restored_draft = self.session_runtimes.get_mut(&key).and_then(|runtime| {
                    runtime.generation = None;
                    runtime.terminal_generation_id = None;
                    runtime
                        .submitted_draft
                        .take()
                        .filter(|(draft_id, _)| *draft_id == id)
                        .map(|(_, draft)| draft)
                });
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
                workspace
                    .chat
                    .push_chat(MsgRole::System, format!("Agent error: {error}"));
                if is_active {
                    self.ui
                        .mypi_command_text_input(cx, ids!(prompt_input))
                        .text_input_ref(cx)
                        .set_text(cx, &draft);
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
                    }
                    self.workspace_state
                        .workspace_mut(key.clone())
                        .chat
                        .flush_streaming();
                    self.set_session_status(cx, &key, UiStatus::Ready, "Ready");

                    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    refresh_sessions(&cwd);
                }
                GuiAgentEvent::AvailableModelsLoaded(models) => {
                    let selected_model = self.ui.drop_down(cx, ids!(model_drop)).selected_label();
                    self.set_model_dropup_options(cx, models, &selected_model);
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
