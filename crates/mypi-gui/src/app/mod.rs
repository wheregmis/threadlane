//! App shell: script_mod! DSL, startup/auth wiring, agent event pump.
//!
//! Chat, sessions, plan, and command palette panels are modularized under `crate::panels`.

use crate::panels::chat::{ChatList, ToolFoldHeader};
use crate::panels::command_palette::*;
use crate::panels::plan::PlanList;
use crate::panels::sessions::{SessionContextMenu, SessionContextMenuAction, SessionList};
use crate::state::{
    active_session_entry, archive_session, builtin_commands, create_new_session, delete_session,
    project_work_dir_at_row, refresh_plan_data, refresh_sessions, session_entry_at_row,
    set_active_session, set_session_context_target, set_sessions_working, truncate_chars,
    CommandInfo, GuiAgentEvent, MsgRole, SessionEntry, ToolStatus,
};
use crate::workspace::{AppState, SessionKey};
use makepad_widgets::text::selection::Cursor;
use makepad_widgets::*;
use mypi_agent::{get_runtime, AgentEvent};
use mypi_coding_agent::{
    discover_agents, AgentConfig, AgentScope, CodingAgent, CodingAgentOptions, ProjectContext,
    SkillMetadata,
};
use mypi_provider::auth;
use mypi_provider::openai::fetch_available_models;

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

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
                        icon_lbl +: {
                            text: "⋯"
                            draw_text +: { color: #x8d8aa3 }
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
    // Plan card rows
    // -------------------------------------------------------------------
    let PlanList = #(PlanList::register_widget(vm)) {
        width: Fill
        height: Fill

        list := PortalList {
            width: Fill
            height: Fill
            flow: Down

            PlanRow := View {
                width: Fill
                height: Fit
                flow: Right
                spacing: 8
                padding: Inset{left: 10 top: 4 right: 10 bottom: 4}
                status_lbl := Label {
                    width: 16
                    height: Fit
                    text: "○"
                    draw_text +: {
                        color: #x8b93a0
                        text_style +: { font_size: 10.5 }
                    }
                }
                desc_lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #xc7cdd6
                        text_style +: { font_size: 10.5 }
                    }
                }
            }

            EmptyRow := EmptyRowBase {
                padding: Inset{left: 10 top: 4 right: 10 bottom: 4}
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
                View {
                    width: 14
                    height: 12
                    show_bg: true
                    draw_bg +: {
                        color: uniform(#x8090a3)
                        pixel: fn() {
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.box(2.0, 1.0, 5.5, 4.0, 1.0)
                            sdf.fill_keep(self.color)
                            sdf.box(1.0, 3.0, 12.0, 8.0, 1.5)
                            sdf.stroke(self.color, 1.0)
                            return sdf.result
                        }
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
                new_project_session_btn := glass.GlassButton {
                    width: 24
                    height: 22
                    padding: 0
                    text: "+"
                    draw_text +: {
                        color: #xeef2f7
                        text_style: theme.font_bold { font_size: 10.5 }
                    }
                    draw_glass +: {
                        corner_radius: 5.0
                    }
                }
            }

            SessionRow := SessionRowBase {}

            SessionRowActive := SessionRowBase {
                draw_bg +: {
                    color: #x3a424e
                    color_hover: #x46505d
                }
                title_lbl +: {
                    draw_text +: {
                        color: #xe8edf4
                    }
                }
                session_row_spinner := ActivityLoader {
                    width: 18
                    height: 10
                    visible: false
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
                title_lbl +: {
                    draw_text +: {
                        color: #xf0f4fa
                    }
                }
                session_row_spinner := ActivityLoader {
                    width: 18
                    height: 10
                    visible: false
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

                            input_bar := mod.components.WarmComposerSurface {
                                width: Fill
                                height: Fit
                                flow: Down
                                spacing: 4
                                padding: Inset{left: 9 top: 7 right: 7 bottom: 7}
                                new_batch: true
                                draw_bg +: {
                                    color: #x29231f
                                    color_hover: #x2f2823
                                    color_focus: #x342a24
                                    border_color: #x51443d
                                    border_color_focus: #xb96543
                                    border_color_down: #xd49a52
                                    border_color_error: #xb85c55
                                    border_size: 1.0
                                    border_radius: 11.0
                                }

                                prompt_input := mod.components.MypiCommandTextInput {
                                    width: Fill
                                    height: Fit
                                    trigger: "/"
                                    inline_search: true
                                    color_focus: #x342a24
                                    color_hover: #x302722

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
                                                    color: #xffeee2
                                                    color_hover: #xfffff5
                                                    color_focus: #xfffff5
                                                    color_empty: #x9f8879
                                                    color_empty_hover: #xb39a88
                                                    color_empty_focus: #xb39a88
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

                                    plan_toggle_btn := Button {
                                        width: Fit
                                        height: 26
                                        visible: false
                                        text: "Plan · 0 steps"
                                        draw_bg +: {
                                            color: #x332a25
                                            color_hover: #x46332a
                                            color_down: #x59402c
                                            border_color: #x604a3f
                                            border_color_hover: #xa86a4c
                                            border_size: 1.0
                                            border_radius: 6.0
                                        }
                                        draw_text +: {
                                            color: #xd8c0ad
                                            text_style +: { font_size: 9.0 }
                                        }
                                    }

                                    composer_status := Label {
                                        width: Fit
                                        height: Fit
                                        visible: false
                                        text: ""
                                        draw_text +: {
                                            color: #xb39a88
                                            text_style +: { font_size: 8.5 }
                                        }
                                    }

                                    stop_btn := mod.components.WarmComposerAction {
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
                                            color: #xb39a88
                                            text_style +: { font_size: 8.5 }
                                        }
                                    }

                                    FlexSpacer {}

                                    model_picker := View {
                                        width: 158
                                        height: 28
                                        visible: false
                                        flow: Overlay
                                        clip_x: false
                                        clip_y: false
                                        align: Align{x: 0.0 y: 1.0}

                                        model_drop := DropDown {
                                            width: 158
                                            height: 1
                                            margin: Inset{bottom: 58}
                                            padding: 0
                                            labels: [
                                                "gpt-5.6-luna",
                                                "gpt-5.4",
                                                "gpt-5.4-mini",
                                                "gpt-5.5",
                                                "gpt-5.6-sol",
                                                "gpt-5.6-terra",
                                                "gpt-5.3-codex-spark",
                                                "gpt-4o",
                                                "gpt-4o-mini"
                                            ]
                                            draw_bg +: {
                                                pixel: fn() {
                                                    return vec4(0.0, 0.0, 0.0, 0.0)
                                                }
                                            }
                                            draw_text +: {
                                                color: #x00000000
                                                color_hover: #x00000000
                                                color_focus: #x00000000
                                                color_down: #x00000000
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

                                        model_picker_btn := Button {
                                            width: Fill
                                            height: Fill
                                            text: "gpt-5.6-luna"
                                            align: Align{x: 0.0 y: 0.5}
                                            padding: Inset{left: 10 right: 24}
                                            draw_bg +: {
                                                color: #x332a25
                                                color_hover: #x46332a
                                                color_down: #x59402c
                                                border_color: #x604a3f
                                                border_color_hover: #xa86a4c
                                                border_color_down: #xb96543
                                                border_size: 1.0
                                                border_radius: 6.0
                                            }
                                            draw_text +: {
                                                color: #xd8c0ad
                                                color_hover: #xf0d7c1
                                                color_down: #xffe4ca
                                                text_style +: { font_size: 9.5 }
                                            }
                                        }

                                        View {
                                            width: Fill
                                            height: Fill
                                            align: Align{x: 1.0 y: 0.5}
                                            padding: Inset{right: 9}
                                            Label {
                                                width: Fit
                                                height: Fit
                                                text: "⌃"
                                                draw_text +: {
                                                    color: #x7f8b9a
                                                    text_style +: { font_size: 8.5 }
                                                }
                                            }
                                        }
                                    }

                                    send_btn := mod.components.WarmComposerAction {
                                        width: 34
                                        height: 30
                                        padding: 0
                                        text: "↑"
                                        draw_text +: {
                                            color: #xfff5ea
                                            text_style: theme.font_bold { font_size: 12.0 }
                                        }
                                    }
                                }
                            }
                            }

                            plan_drawer := PanelSurface {
                                width: 300
                                visible: false
                                flow: Down
                                padding: Inset{left: 2 top: 10 right: 2 bottom: 8}
                                PanelHeader {
                                    width: Fill
                                    height: Fit
                                    padding: Inset{left: 12 right: 8 bottom: 8}
                                    Label {
                                        text: "Plan"
                                        draw_text +: {
                                            color: #xdde3ea
                                            text_style: theme.font_bold { font_size: 11.0 }
                                        }
                                    }
                                    FlexSpacer {}
                                    plan_status_label := Label {
                                        text: "active"
                                        draw_text +: {
                                            color: #x6fa8ff
                                            text_style +: { font_size: 9.5 }
                                        }
                                    }
                                    plan_close_btn := Button {
                                        width: 28
                                        height: 24
                                        text: "×"
                                    }
                                }
                                plan_empty_label := Label {
                                    width: Fill
                                    height: Fit
                                    visible: false
                                    text: "Waiting for the planning response…"
                                    draw_text +: {
                                        color: #x8b93a0
                                        text_style +: { font_size: 10.0 }
                                    }
                                }
                                plan_list := PlanList {}
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

#[derive(Script)]
pub struct App {
    #[live]
    pub ui: WidgetRef,
    #[rust]
    tx: Option<Sender<GuiAgentEvent>>,
    #[rust]
    rx: Option<Arc<Mutex<Receiver<GuiAgentEvent>>>>,
    #[rust]
    agent: Option<Arc<tokio::sync::Mutex<CodingAgent>>>,
    #[rust]
    active_generation: Option<GenerationRun>,
    #[rust]
    terminal_generation_id: Option<u64>,
    #[rust]
    next_generation_id: u64,
    #[rust]
    busy: bool,
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

        self.agent = Some(Arc::new(tokio::sync::Mutex::new(coding_agent)));

        self.spawn_model_fetch(api_key, account_id_opt);

        cx.redraw_all();
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.ui.button(cx, ids!(login_btn)).clicked(actions) {
            if let Some(generation) = self.active_generation.take() {
                generation.handle.abort();
            }
            self.push_chat(MsgRole::System, "Initiating ChatGPT device code login...");
            self.set_status(cx, UiStatus::Working, "Connecting to ChatGPT...");
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
                                        let _ =
                                            tx.send(GuiAgentEvent::Agent(AgentEvent::AgentError {
                                                error: e,
                                            }));
                                        SignalToUI::set_ui_signal();
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            let _ =
                                tx.send(GuiAgentEvent::Agent(AgentEvent::AgentError { error: e }));
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

        if self.ui.button(cx, ids!(plan_toggle_btn)).clicked(actions) {
            let plan_drawer_open = self.toggle_plan_drawer();
            self.ui
                .widget(cx, ids!(plan_drawer))
                .set_visible(cx, plan_drawer_open);
            cx.redraw_all();
        }
        if self.ui.button(cx, ids!(plan_close_btn)).clicked(actions) {
            self.set_plan_drawer_open(false);
            self.ui.widget(cx, ids!(plan_drawer)).set_visible(cx, false);
            cx.redraw_all();
        }

        if self.ui.button(cx, ids!(stop_btn)).clicked(actions) {
            if let Some(generation) = self.active_generation.take() {
                generation.handle.abort();
                self.set_status(cx, UiStatus::Ready, "Stopped");
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
                .glass_button(cx, ids!(new_project_session_btn))
                .clicked(actions)
                && !self.busy
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
                } else if fe.is_over && fe.is_primary_hit() && fe.was_tap() && !self.busy {
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

        if self.ui.button(cx, ids!(model_picker_btn)).clicked(actions) {
            if let Some(mut model_drop) = self
                .ui
                .widget(cx, ids!(model_drop))
                .borrow_mut::<DropDown>()
            {
                model_drop.set_key_focus(cx);
                model_drop.set_active(cx);
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
                cti.reset(cx);
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
        self.ui
            .button(cx, ids!(model_picker_btn))
            .set_text(cx, &self.available_models[selected_item]);
    }

    fn select_workspace(&mut self, work_dir: PathBuf, session_id: impl Into<String>) {
        self.workspace_state
            .select(SessionKey::new(work_dir, session_id));
    }

    fn save_active_draft(&mut self, cx: &Cx) {
        let draft = self.ui.text_input(cx, ids!(prompt_input)).text();
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
            .text_input(cx, ids!(prompt_input))
            .set_text(cx, &draft);
    }

    fn push_chat(&mut self, role: MsgRole, text: impl Into<String>) {
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            workspace.chat.push_chat(role, text);
        }
    }

    fn toggle_plan_drawer(&mut self) -> bool {
        let Some(workspace) = self.workspace_state.active_workspace_mut() else {
            return false;
        };
        workspace.ui.plan_drawer_open = !workspace.ui.plan_drawer_open;
        workspace.ui.plan_drawer_open
    }

    fn set_plan_drawer_open(&mut self, open: bool) {
        if let Some(workspace) = self.workspace_state.active_workspace_mut() {
            workspace.ui.plan_drawer_open = open;
        }
    }

    fn set_status(&mut self, cx: &mut Cx, status: UiStatus, _text: &str) {
        self.busy = status == UiStatus::Working;
        let working = status == UiStatus::Working;
        set_sessions_working(working);
        self.ui
            .widget(cx, ids!(chat_working_indicator))
            .set_visible(cx, working);
        self.ui
            .widget(cx, ids!(stop_btn))
            .set_visible(cx, working && self.active_generation.is_some());
        self.ui.widget(cx, ids!(send_btn)).set_visible(cx, !working);
        self.ui.label(cx, ids!(composer_status)).set_text(cx, _text);
        self.ui
            .label(cx, ids!(composer_status))
            .set_visible(cx, working || status == UiStatus::Error);
        self.ui.widget(cx, ids!(chat_working_indicator)).redraw(cx);
    }

    fn refresh_plan_ui(&mut self, cx: &mut Cx, work_dir: &std::path::Path, session_id: &str) {
        let key = SessionKey::new(work_dir.to_path_buf(), session_id);
        let workspace = self.workspace_state.workspace_mut(key);
        let enabled = refresh_plan_data(&mut workspace.plan, work_dir, session_id);
        let item_count = workspace.plan.items.len();
        let plan_drawer_open = &mut workspace.ui.plan_drawer_open;

        if !enabled {
            *plan_drawer_open = false;
        }

        self.ui
            .button(cx, ids!(plan_toggle_btn))
            .set_visible(cx, enabled);
        self.ui.button(cx, ids!(plan_toggle_btn)).set_text(
            cx,
            &format!(
                "Plan · {item_count} step{}",
                if item_count == 1 { "" } else { "s" }
            ),
        );
        self.ui
            .widget(cx, ids!(plan_drawer))
            .set_visible(cx, enabled && *plan_drawer_open);
        self.ui
            .widget(cx, ids!(plan_list))
            .set_visible(cx, enabled && item_count > 0);
        self.ui
            .label(cx, ids!(plan_empty_label))
            .set_visible(cx, enabled && item_count == 0);
        self.ui.label(cx, ids!(plan_status_label)).set_text(
            cx,
            if item_count == 0 {
                "planning"
            } else {
                "active"
            },
        );
    }

    fn apply_session_context_action(
        &mut self,
        cx: &mut Cx,
        action: fn(&SessionEntry) -> bool,
        action_name: &str,
    ) {
        if self.busy {
            return;
        }
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
        self.workspace_state
            .remove(&SessionKey::new(entry.work_dir.clone(), entry.id.clone()));
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
        if self.busy {
            return;
        }

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

        self.select_workspace_ui(cx, entry.work_dir.clone(), entry.id.clone());

        if let Some(agent_arc) = &self.agent {
            if let Ok(agent) = agent_arc.try_lock() {
                if agent.session_file_path() == Some(&entry.session_file) {
                    set_active_session(&entry.work_dir, &entry.id);
                    cx.redraw_all();
                    return;
                }
            }
        }

        set_active_session(&entry.work_dir, &entry.id);
        let Some(tx) = self.tx.clone() else { return };
        let Some(agent_arc) = self.agent.clone() else {
            if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                workspace.chat.replace_from_agent_messages(&[]);
            }
            self.push_chat(
                MsgRole::System,
                format!(
                    "Selected session `{}` (agent not started yet).",
                    entry.title
                ),
            );
            self.refresh_plan_ui(cx, &entry.work_dir, &entry.id);
            cx.redraw_all();
            return;
        };

        if let Some(generation) = self.active_generation.take() {
            generation.handle.abort();
        }
        self.terminal_generation_id = None;
        self.set_status(cx, UiStatus::Working, "Switching session...");
        let session_file = entry.session_file.clone();
        let session_id = entry.id.clone();
        let title = entry.title.clone();
        let work_dir = entry.work_dir.clone();

        get_runtime().spawn(async move {
            let mut agent = agent_arc.lock().await;
            agent.switch_session_file(session_file).await;
            let messages = agent.session_tree.get_active_branch_messages();
            let _ = tx.send(GuiAgentEvent::SessionSwitched {
                session_id,
                title,
                work_dir,
                messages,
            });
            SignalToUI::set_ui_signal();
        });
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
        let api_key_widget = self.ui.text_input(cx, ids!(api_key_input));
        let mut api_key = api_key_widget.text();
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

        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let new_session_file = if show_in_chat && active_session_entry().is_none() {
            match create_new_session(&work_dir) {
                Some(entry) => {
                    set_active_session(&entry.work_dir, &entry.id);
                    self.select_workspace_ui(cx, entry.work_dir, entry.id);
                    Some(entry.session_file)
                }
                None => {
                    self.push_chat(MsgRole::System, "Could not create a new session file.");
                    cx.redraw_all();
                    return;
                }
            }
        } else {
            None
        };

        if self.agent.is_none() {
            let agent_opts = CodingAgentOptions {
                api_key,
                account_id,
                model: model_name,
                work_dir: work_dir.clone(),
                session_file: active_session_entry().map(|e| e.session_file),
            };
            self.agent = Some(Arc::new(tokio::sync::Mutex::new(CodingAgent::new(
                agent_opts,
            ))));
        }

        let Some(tx) = self.tx.clone() else { return };
        let agent_arc = self.agent.as_ref().unwrap().clone();
        let input_str = input_text.trim().to_string();
        self.next_generation_id = self.next_generation_id.wrapping_add(1);
        let generation_id = self.next_generation_id;
        self.terminal_generation_id = None;

        if show_in_chat {
            self.push_chat(MsgRole::User, input_str.clone());
        }
        let chat_list = self.ui.widget(cx, ids!(chat_list));
        chat_list.portal_list(cx, ids!(list)).set_tail_range(true);
        cx.redraw_all();

        let generation_handle = get_runtime().spawn(async move {
            let mut agent_lock = agent_arc.lock().await;
            if let Some(session_file) = new_session_file {
                agent_lock.switch_session_file(session_file).await;
            }

            // Keep the detached forwarder correlated with this generation and
            // stop it when the outer generation task is aborted. Without this,
            // a forwarder can deliver late events from an aborted generation.
            let mut event_rx = agent_lock.subscribe();
            let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
            let tx_event = tx.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = &mut cancel_rx => break,
                        result = event_rx.recv() => match result {
                            Ok(evt) => {
                                let _ = tx_event.send(GuiAgentEvent::GenerationAgent {
                                    generation_id,
                                    event: evt,
                                });
                                SignalToUI::set_ui_signal();
                            }
                            Err(_) => break,
                        }
                    }
                }
            });

            if let Some(out) = agent_lock.handle_input(&input_str).await {
                let _ = tx.send(GuiAgentEvent::CommandOutput {
                    generation_id,
                    output: out,
                });
                SignalToUI::set_ui_signal();
            }
            let _ = cancel_tx.send(());
        });
        self.active_generation = Some(GenerationRun {
            id: generation_id,
            handle: generation_handle,
        });
        self.set_status(cx, UiStatus::Working, "Working...");
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

    fn handle_agent_event(&mut self, cx: &mut Cx, event: AgentEvent, generation_id: Option<u64>) {
        match event {
            AgentEvent::AgentStart => self.set_status(cx, UiStatus::Working, "Working..."),
            AgentEvent::MessageUpdate {
                text_delta,
                reasoning_delta,
                ..
            } => {
                if let Some(delta) = reasoning_delta {
                    if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                        workspace
                            .chat
                            .push_stream_delta(crate::state::StreamingKind::Thinking, &delta);
                    }
                }
                if let Some(delta) = text_delta {
                    if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                        workspace
                            .chat
                            .push_stream_delta(crate::state::StreamingKind::Assistant, &delta);
                    }
                }
            }
            AgentEvent::MessageEnd { message } => {
                if matches!(
                    message,
                    mypi_agent::AgentMessage::Assistant {
                        tool_calls: Some(_),
                        ..
                    }
                ) {
                    if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                        workspace.chat.flush_tool_call_preamble();
                    }
                } else {
                    if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                        workspace.chat.flush_streaming();
                    }
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                name,
                arguments,
            } => {
                if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                    workspace.chat.push_tool(tool_call_id, name, arguments);
                }
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
            } => {
                if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                    workspace
                        .chat
                        .update_tool(&tool_call_id, partial_result, None);
                }
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                ..
            } => {
                if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                    workspace.chat.update_tool(
                        &tool_call_id,
                        result.content,
                        Some(if result.is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Done
                        }),
                    );
                }
            }
            AgentEvent::TurnEnd { .. } => self.set_status(cx, UiStatus::Working, "Turn completed"),
            AgentEvent::AgentEnd { .. } => {
                if let Some(id) = generation_id {
                    if self
                        .active_generation
                        .as_ref()
                        .is_some_and(|generation| generation.id == id)
                    {
                        self.active_generation = None;
                        self.terminal_generation_id = Some(id);
                    }
                }
                if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                    workspace.chat.flush_streaming();
                }
                self.set_status(cx, UiStatus::Ready, "Ready");
                let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                if let Some(session) = active_session_entry() {
                    self.refresh_plan_ui(cx, &work_dir, &session.id);
                }
                refresh_sessions(&work_dir);
            }
            AgentEvent::AgentError { error } => {
                self.active_generation = None;
                if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                    workspace.chat.flush_streaming();
                }
                self.push_chat(MsgRole::System, format!("Agent error: {error}"));
                self.set_status(cx, UiStatus::Error, "Error");
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
                    output,
                } => {
                    let is_current_generation = self
                        .active_generation
                        .as_ref()
                        .is_some_and(|generation| generation.id == generation_id)
                        || self.terminal_generation_id == Some(generation_id);
                    if !is_current_generation {
                        continue;
                    }
                    self.active_generation = None;
                    self.terminal_generation_id = None;
                    self.push_chat(MsgRole::System, output);
                    self.set_status(cx, UiStatus::Ready, "Ready");
                    let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    if let Some(session) = active_session_entry() {
                        self.refresh_plan_ui(cx, &work_dir, &session.id);
                    }
                    refresh_sessions(&work_dir);
                }
                GuiAgentEvent::SessionSwitched {
                    session_id,
                    title,
                    work_dir,
                    messages,
                } => {
                    set_active_session(&work_dir, &session_id);
                    self.select_workspace_ui(cx, work_dir.clone(), session_id.clone());
                    if let Some(workspace) = self.workspace_state.active_workspace_mut() {
                        workspace.chat.replace_from_agent_messages(&messages);
                    }
                    self.push_chat(MsgRole::System, format!("Switched to session `{title}`."));
                    self.refresh_plan_ui(cx, &work_dir, &session_id);
                    refresh_sessions(&work_dir);
                    set_active_session(&work_dir, &session_id);
                    self.set_status(cx, UiStatus::Ready, "Ready");
                }
                GuiAgentEvent::AvailableModelsLoaded(models) => {
                    let selected_model = self.ui.drop_down(cx, ids!(model_drop)).selected_label();
                    self.set_model_dropup_options(cx, models, &selected_model);
                }
                GuiAgentEvent::DeviceCodePrompt { user_code, url } => {
                    self.push_chat(
                        MsgRole::System,
                        format!(
                            "Sign in: open {url} in your browser and enter code {user_code} \
                             (waiting for authorization...)"
                        ),
                    );
                    self.set_status(cx, UiStatus::Working, &format!("Enter code {user_code}"));
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
                    self.push_chat(MsgRole::System, "Successfully authenticated with ChatGPT.");
                    self.ui.widget(cx, ids!(auth_row)).set_visible(cx, false);
                    self.set_status(cx, UiStatus::Ready, "Signed in");

                    if let Some(key) = key_opt {
                        self.spawn_model_fetch(key, acc_opt);
                    }
                }
                GuiAgentEvent::GenerationAgent {
                    generation_id,
                    event: agent_event,
                } => {
                    if self
                        .active_generation
                        .as_ref()
                        .is_none_or(|generation| generation.id != generation_id)
                    {
                        continue;
                    }
                    self.handle_agent_event(cx, agent_event, Some(generation_id))
                }
                GuiAgentEvent::Agent(agent_event) => self.handle_agent_event(cx, agent_event, None),
            }
        }

        cx.redraw_all();
    }
}
