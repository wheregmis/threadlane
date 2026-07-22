//! App shell: script_mod! DSL, startup/auth wiring, agent event pump.
//!
//! Chat/plan/activity rows are drawn by the custom widgets in `chat.rs`
//! from the shared state in `state.rs`.

use crate::chat::{ActivityList, ChatList, PlanList, SessionList};
use crate::command_text_input::*;
use crate::state::{
    active_session_entry, builtin_commands, clear_activity, create_new_session, flush_streaming,
    push_activity, push_chat, push_stream_delta, refresh_plan, refresh_sessions,
    replace_chat_from_agent_messages, session_entry_at_row, set_active_session, set_chat_text,
    truncate_chars, update_activity, ActivityStatus, CommandInfo, GuiAgentEvent, MsgRole,
    SessionEntry,
};
use makepad_widgets::text::selection::Cursor;
use makepad_widgets::*;
use mypi_agent::{get_runtime, AgentEvent, CodingAgent, CodingAgentOptions};
use mypi_provider::auth;
use mypi_provider::openai::fetch_available_models;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

script_mod! {
    use mod.prelude.widgets.*

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

            UserMsg := RoundedView {
                width: Fill
                height: Fit
                margin: Inset{top: 6 bottom: 6 left: 50 right: 8}
                padding: Inset{left: 14 top: 10 right: 14 bottom: 10}
                draw_bg +: {
                    color: #x2a3547
                    border_radius: 10.0
                }
                md := Markdown {
                    width: Fill
                    height: Fit
                    selectable: true
                    use_code_block_widget: false
                    body: ""
                }
            }

            AssistantMsg := RoundedView {
                width: Fill
                height: Fit
                margin: Inset{top: 6 bottom: 6 left: 8 right: 40}
                padding: Inset{left: 14 top: 10 right: 14 bottom: 10}
                draw_bg +: {
                    color: #x20252d
                    border_radius: 10.0
                    border_size: 1.0
                    border_color: #x2d3540
                }
                md := Markdown {
                    width: Fill
                    height: Fit
                    selectable: true
                    use_code_block_widget: false
                    body: ""
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

            ToolMsg := View {
                width: Fill
                height: Fit
                margin: Inset{top: 2 bottom: 2 left: 12 right: 12}
                lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #x6f7a88
                        text_style: theme.font_code { font_size: 9.5 }
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

            EmptyRow := View {
                width: Fill
                height: Fit
                padding: Inset{left: 10 top: 4 right: 10 bottom: 4}
                lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #x6f7a88
                        text_style +: { font_size: 10.0 }
                    }
                }
            }
        }
    }

    // -------------------------------------------------------------------
    // Activity card rows
    // -------------------------------------------------------------------
    let ActivityList = #(ActivityList::register_widget(vm)) {
        width: Fill
        height: Fill

        list := PortalList {
            width: Fill
            height: Fill
            flow: Down
            auto_tail: true

            ActivityRow := View {
                width: Fill
                height: Fit
                flow: Right
                spacing: 8
                align: Align{y: 0.5}
                padding: Inset{left: 8 top: 3 right: 8 bottom: 3}
                head_lbl := Label {
                    width: Fit
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #xc7cdd6
                        text_style: theme.font_code { font_size: 10.0 }
                    }
                }
                detail_lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #x6f7a88
                        text_style +: { font_size: 9.5 }
                    }
                }
            }

            EmptyRow := View {
                width: Fill
                height: Fit
                padding: Inset{left: 8 top: 4 right: 8 bottom: 4}
                lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #x6f7a88
                        text_style +: { font_size: 10.0 }
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
                height: Fit
                flow: Right
                spacing: 8
                align: Align{y: 0.5}
                padding: Inset{left: 10 top: 10 right: 10 bottom: 4}
                folder_lbl := Label {
                    width: Fit
                    height: Fit
                    text: "📁"
                    draw_text +: {
                        color: #xc7cdd6
                        text_style +: { font_size: 11.0 }
                    }
                }
                name_lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #xb8c0cc
                        text_style +: { font_size: 11.0 }
                    }
                }
            }

            SessionRow := RoundedView {
                width: Fill
                height: Fit
                cursor: MouseCursor.Hand
                flow: Right
                spacing: 8
                align: Align{y: 0.5}
                margin: Inset{left: 6 right: 6 top: 1 bottom: 1}
                padding: Inset{left: 12 top: 7 right: 10 bottom: 7}
                draw_bg +: {
                    color: #x00000000
                    border_radius: 8.0
                }
                title_lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #x9aa3b0
                        text_style +: { font_size: 11.0 }
                    }
                }
                time_lbl := Label {
                    width: Fit
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #x6f7a88
                        text_style +: { font_size: 10.0 }
                    }
                }
            }

            SessionRowActive := RoundedView {
                width: Fill
                height: Fit
                cursor: MouseCursor.Hand
                flow: Right
                spacing: 8
                align: Align{y: 0.5}
                margin: Inset{left: 6 right: 6 top: 1 bottom: 1}
                padding: Inset{left: 12 top: 7 right: 10 bottom: 7}
                draw_bg +: {
                    color: #x3a424e
                    border_radius: 8.0
                }
                title_lbl := Label {
                    width: Fill
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #xe8edf4
                        text_style +: { font_size: 11.0 }
                    }
                }
                time_lbl := Label {
                    width: Fit
                    height: Fit
                    text: ""
                    draw_text +: {
                        color: #xaeb6c2
                        text_style +: { font_size: 10.0 }
                    }
                }
            }

            EmptyRow := View {
                width: Fill
                height: Fit
                padding: Inset{left: 22 top: 4 right: 10 bottom: 8}
                lbl := Label {
                    width: Fill
                    height: Fit
                    text: "No agents yet"
                    draw_text +: {
                        color: #x555d6a
                        text_style +: { font_size: 11.0 }
                    }
                }
            }
        }
    }

    startup() do #(App::script_component(vm)){
        // Template minted at runtime for slash-command autocomplete rows
        // (collected in App::on_after_apply, instantiated per command).
        CmdItem := View {
            width: Fill
            height: Fit
            flow: Down
            spacing: 1
            padding: Inset{left: 12 top: 6 right: 12 bottom: 6}
            show_bg: true
            cmd_name := Label {
                width: Fill
                height: Fit
                text: ""
                draw_text +: {
                    color: #xdde3ea
                    text_style: theme.font_code { font_size: 10.5 }
                }
            }
            cmd_desc := Label {
                width: Fill
                height: Fit
                text: ""
                draw_text +: {
                    color: #x8b93a0
                    text_style +: { font_size: 9.0 }
                }
            }
        }

        ui: Root {
            main_window := Window {
                window.inner_size: vec2(1280, 768)
                window.title: "mypi"
                pass.clear_color: #x181a1f
                body +: {
                    View {
                        width: Fill
                        height: Fill
                        flow: Down
                        spacing: 8
                        padding: Inset{left: 12 top: 10 right: 12 bottom: 10}

                        // ------------------------------------------------
                        // Header: title | workspace | model | status pill
                        // ------------------------------------------------
                        header := View {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 10
                            align: Align{y: 0.5}
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
                            View { width: Fill height: 1 }
                            model_drop := DropDown {
                                width: 180
                                height: 28
                                labels: [
                                    "gpt-5.4",
                                    "gpt-5.4-mini",
                                    "gpt-5.5",
                                    "gpt-5.6-luna",
                                    "gpt-5.6-sol",
                                    "gpt-5.6-terra",
                                    "gpt-5.3-codex-spark",
                                    "gpt-4o",
                                    "gpt-4o-mini"
                                ]
                                draw_bg +: {
                                    color: #x232830
                                    color_hover: #x2a313c
                                    color_focus: #x2a313c
                                    color_down: #x2a313c
                                    border_color: #x3a424e
                                    border_color_hover: #x4a5564
                                    border_color_focus: #x4a5564
                                    border_color_down: #x4a5564
                                    arrow_color: #xc7cdd6
                                    arrow_color_hover: #xdde3ea
                                    arrow_color_focus: #xdde3ea
                                    arrow_color_down: #xdde3ea
                                }
                                draw_text +: {
                                    color: #xdde3ea
                                    color_hover: #xffffff
                                    color_focus: #xffffff
                                    color_down: #xffffff
                                    text_style +: { font_size: 10.5 }
                                }
                                popup_menu: PopupMenuFlat {
                                    width: 210
                                    draw_bg +: {
                                        color: #x232830
                                        border_color: #x4a5564
                                        border_size: 1.0
                                        border_radius: 8.0
                                    }
                                    menu_item: PopupMenuItem {
                                        draw_text +: {
                                            color: #xdde3ea
                                            color_hover: #xffffff
                                            color_active: #xffffff
                                            text_style +: { font_size: 10.5 }
                                        }
                                        draw_bg +: {
                                            color: #x00000000
                                            color_hover: #x2f3a4d
                                            color_active: #x3a4a5f
                                            mark_color: #x00000000
                                            mark_color_active: #x4ac26b
                                        }
                                    }
                                }
                            }
                            status_pill := RoundedView {
                                width: Fit
                                height: Fit
                                flow: Right
                                spacing: 6
                                align: Align{y: 0.5}
                                padding: Inset{left: 10 top: 5 right: 10 bottom: 5}
                                draw_bg +: {
                                    color: #x1f232b
                                    border_radius: 12.0
                                }
                                // Pre-styled dots toggled by visibility (no
                                // runtime shader applies needed).
                                dot_ready := RoundedView {
                                    width: 8
                                    height: 8
                                    draw_bg +: {
                                        color: #x4ac26b
                                        border_radius: 4.0
                                    }
                                }
                                dot_working := RoundedView {
                                    width: 8
                                    height: 8
                                    visible: false
                                    draw_bg +: {
                                        color: #xd9a94a
                                        border_radius: 4.0
                                    }
                                }
                                dot_error := RoundedView {
                                    width: 8
                                    height: 8
                                    visible: false
                                    draw_bg +: {
                                        color: #xe5534b
                                        border_radius: 4.0
                                    }
                                }
                                status_label := Label {
                                    text: "Ready"
                                    draw_text +: {
                                        color: #xc7cdd6
                                        text_style +: { font_size: 10.0 }
                                    }
                                }
                                spinner := LoadingSpinner {
                                    width: 14
                                    height: 14
                                    visible: false
                                    draw_bg +: {
                                        stroke_width: 2.0
                                    }
                                }
                            }
                        }

                        // ------------------------------------------------
                        // First-run auth banner (hidden once authenticated)
                        // ------------------------------------------------
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

                        // ------------------------------------------------
                        // Sessions | Chat | Plan
                        // ------------------------------------------------
                        content_row := View {
                            width: Fill
                            height: Fill
                            flow: Right
                            spacing: 10

                            sessions_panel := View {
                                width: 240
                                height: Fill
                                flow: Down
                                spacing: 6
                                padding: Inset{left: 2 top: 2 right: 2 bottom: 2}

                                View {
                                    width: Fill
                                    height: Fit
                                    flow: Right
                                    align: Align{y: 0.5}
                                    padding: Inset{left: 8 right: 6 top: 2 bottom: 2}
                                    Label {
                                        text: "Sessions"
                                        draw_text +: {
                                            color: #xaeb6c2
                                            text_style: theme.font_bold { font_size: 10.5 }
                                        }
                                    }
                                    View { width: Fill height: 1 }
                                    new_session_btn := Button {
                                        width: Fit
                                        height: 26
                                        text: "New"
                                        draw_bg +: {
                                            color: #x2a313c
                                            color_hover: #x343d4a
                                            color_down: #x3a424e
                                            border_radius: 6.0
                                            border_size: 0.0
                                        }
                                        draw_text +: {
                                            color: #xc7cdd6
                                            text_style +: { font_size: 10.0 }
                                        }
                                    }
                                }

                                session_list := SessionList {}
                            }

                            chat_panel := RoundedView {
                                width: Fill
                                height: Fill
                                padding: Inset{left: 4 top: 6 right: 4 bottom: 6}
                                draw_bg +: {
                                    color: #x1c1f26
                                    border_radius: 10.0
                                }
                                chat_list := ChatList {}
                            }

                            plan_card := RoundedView {
                                width: 280
                                height: Fill
                                flow: Down
                                padding: Inset{left: 2 top: 10 right: 2 bottom: 8}
                                draw_bg +: {
                                    color: #x1f232b
                                    border_radius: 10.0
                                }
                                View {
                                    width: Fill
                                    height: Fit
                                    flow: Right
                                    align: Align{y: 0.5}
                                    padding: Inset{left: 12 right: 12 bottom: 8}
                                    Label {
                                        text: "Plan"
                                        draw_text +: {
                                            color: #xdde3ea
                                            text_style: theme.font_bold { font_size: 11.0 }
                                        }
                                    }
                                    View { width: Fill height: 1 }
                                    plan_status_label := Label {
                                        text: "inactive"
                                        draw_text +: {
                                            color: #x6f7a88
                                            text_style +: { font_size: 9.5 }
                                        }
                                    }
                                }
                                plan_list := PlanList {}
                            }
                        }

                        // ------------------------------------------------
                        // Compact foldable activity (collapsed by default)
                        // ------------------------------------------------
                        activity_fold := FoldHeader {
                            width: Fill
                            height: Fit
                            opened: 0.0

                            header: RoundedView {
                                width: Fill
                                height: Fit
                                flow: Right
                                spacing: 8
                                align: Align{y: 0.5}
                                padding: Inset{left: 10 top: 6 right: 10 bottom: 6}
                                draw_bg +: {
                                    color: #x1f232b
                                    border_radius: 8.0
                                }
                                fold_button := FoldButton {}
                                Label {
                                    text: "Activity"
                                    draw_text +: {
                                        color: #xaeb6c2
                                        text_style: theme.font_bold { font_size: 10.5 }
                                    }
                                }
                                activity_summary := Label {
                                    width: Fill
                                    height: Fit
                                    text: "No recent tools"
                                    draw_text +: {
                                        color: #x6f7a88
                                        text_style +: { font_size: 10.0 }
                                    }
                                }
                            }

                            body: View {
                                width: Fill
                                height: Fit{max: FitBound.Abs(132)}
                                padding: Inset{left: 2 top: 2 right: 2 bottom: 2}
                                activity_list := ActivityList {}
                            }
                        }

                        // ------------------------------------------------
                        // Growing prompt input + Send
                        // ------------------------------------------------
                        input_bar := RoundedView {
                            width: Fill
                            height: Fit
                            flow: Right
                            spacing: 8
                            align: Align{y: 1.0}
                            padding: Inset{left: 8 top: 8 right: 8 bottom: 8}
                            draw_bg +: {
                                color: #x1f232b
                                border_radius: 10.0
                            }

                            prompt_input := mod.widgets.CommandTextInput {
                                width: Fill
                                height: Fit
                                trigger: "/"
                                inline_search: true
                                color_focus: #x2f3a4d
                                color_hover: #x262c37

                                // Use `+:` so we merge into the stock RoundedView/TextInput
                                // children. `:= { ... }` replaces them and drops the TextInput
                                // widget type, leaving an invisible empty view.
                                persistent +: {
                                    width: Fill
                                    height: Fit
                                    center +: {
                                        width: Fill
                                        height: Fit
                                        text_input +: {
                                            width: Fill
                                            height: Fit{min: FitBound.Abs(40), max: FitBound.Abs(160)}
                                            margin: 0
                                            is_multiline: true
                                            submit_on_enter: true
                                            empty_text: "Message mypi…  (/ commands · Enter to send · Shift+Enter for newline)"
                                            draw_bg +: {
                                                color: #x181a1f
                                                color_empty: #x181a1f
                                                color_hover: #x1c2028
                                                color_focus: #x1c2028
                                            }
                                        }
                                    }
                                }
                            }

                            send_btn := Button {
                                width: 84
                                height: 40
                                text: "Send"
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
    busy: bool,
    /// Built-in + extension slash commands for autocomplete.
    #[rust]
    commands: Vec<CommandInfo>,
    /// The `CmdItem` DSL template, collected in `on_after_apply`.
    #[rust]
    cmd_item_template: Option<ScriptObjectRef>,
    /// Popup item widget -> command name for the currently shown items.
    #[rust]
    cmd_items: Vec<(WidgetRef, String)>,
    /// tool_call_id -> chat message index for in-flight tool lines.
    #[rust]
    tool_msg_index: HashMap<String, usize>,
}

impl ScriptHook for App {
    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        apply: &Apply,
        _scope: &mut Scope,
        value: ScriptValue,
    ) {
        // Collect the CmdItem template from the app object's vec entries
        // (same mechanism PortalList uses for its item templates).
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if kv.key.as_id() == Some(live_id!(CmdItem)) {
                            if let Some(template_obj) = kv.value.as_object() {
                                self.cmd_item_template =
                                    Some(vm.bx.heap.new_object_ref(template_obj));
                            }
                        }
                    }
                });
            }
        }
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        // Single event channel for the whole app lifetime; the Sender is
        // cloned into every background task so no in-flight event gets lost.
        let (tx, rx) = channel::<GuiAgentEvent>();
        self.tx = Some(tx);
        self.rx = Some(Arc::new(Mutex::new(rx)));

        let mut key_opt = None;
        let mut account_id_opt = None;

        if let Some(creds) = auth::load_credentials() {
            self.ui
                .text_input(cx, ids!(api_key_input))
                .set_text(cx, &creds.access_token);
            push_chat(
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

        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        self.ui
            .label(cx, ids!(workspace_label))
            .set_text(cx, &work_dir.display().to_string());
        self.refresh_plan_ui(cx, &work_dir);
        refresh_sessions(&work_dir);
        let context = mypi_agent::ProjectContext::discover(&work_dir);

        if !context.context_files.is_empty() {
            push_chat(
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
            model: "gpt-5.4".to_string(),
            work_dir: work_dir.clone(),
            session_file: active_session_entry().map(|e| e.session_file),
            enable_plan_mode: false,
        };

        let coding_agent = CodingAgent::new(agent_opts);
        if let Some(path) = coding_agent.session_file_path() {
            let id = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "default".into());
            set_active_session(&work_dir, &id);
        }

        // Slash-command list: built-ins + WASI extension commands.
        self.commands = builtin_commands();
        if coding_agent.wasi_extensions.extensions.is_empty() {
            push_chat(
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
                push_chat(
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
        push_chat(
            MsgRole::System,
            "Type / in the input bar to browse slash commands.",
        );

        self.agent = Some(Arc::new(tokio::sync::Mutex::new(coding_agent)));

        // Fetch models in background.
        self.spawn_model_fetch(api_key, account_id_opt);
        self.ui
            .fold_header(cx, ids!(activity_fold))
            .set_is_open(cx, false, Animate::No);
        self.refresh_activity_header(cx);
        cx.redraw_all();
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        // --- ChatGPT device-code login ---
        if self.ui.button(cx, ids!(login_btn)).clicked(actions) {
            push_chat(MsgRole::System, "Initiating ChatGPT device code login...");
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

        // --- Sessions: new + select ---
        if self.ui.button(cx, ids!(new_session_btn)).clicked(actions) && !self.busy {
            self.create_and_activate_session(cx);
        }
        let session_list = self.ui.portal_list(cx, ids!(session_list.list));
        for (item_id, item) in session_list.items_with_actions(actions) {
            if let Some(fe) = item.as_view().finger_up(actions) {
                if fe.is_over && fe.is_primary_hit() && fe.was_tap() && !self.busy {
                    if let Some(entry) = session_entry_at_row(item_id) {
                        self.activate_session(cx, entry);
                    }
                }
            }
        }

        // --- Slash-command autocomplete popup ---
        let cti = self.ui.command_text_input(cx, ids!(prompt_input));
        if cti.should_build_items(actions) {
            self.build_cmd_items(cx);
        }
        if let Some(selected) = cti.item_selected(actions) {
            let uid = selected.widget_uid();
            let name = self
                .cmd_items
                .iter()
                .find(|(item, _)| item.widget_uid() == uid)
                .map(|(_, name)| name.clone());
            if let Some(name) = name {
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
        }

        // --- Model selection dispatches /model through the agent ---
        if self
            .ui
            .drop_down(cx, ids!(model_drop))
            .selected(actions)
            .is_some()
        {
            let model_name = self.ui.drop_down(cx, ids!(model_drop)).selected_label();
            if !model_name.is_empty() && !self.busy {
                self.dispatch_input(cx, format!("/model {model_name}"), false);
            }
        }

        // --- Prompt submission ---
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
        crate::command_text_input::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.poll_agent_events(cx);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}

impl App {
    // -----------------------------------------------------------------
    // Status pill
    // -----------------------------------------------------------------
    fn set_status(&mut self, cx: &mut Cx, status: UiStatus, text: &str) {
        self.busy = status == UiStatus::Working;
        self.ui.label(cx, ids!(status_label)).set_text(cx, text);
        self.ui.widget(cx, ids!(spinner)).set_visible(cx, self.busy);
        self.ui
            .button(cx, ids!(send_btn))
            .set_enabled(cx, !self.busy);
        self.ui
            .widget(cx, ids!(dot_ready))
            .set_visible(cx, status == UiStatus::Ready);
        self.ui
            .widget(cx, ids!(dot_working))
            .set_visible(cx, status == UiStatus::Working);
        self.ui
            .widget(cx, ids!(dot_error))
            .set_visible(cx, status == UiStatus::Error);
        self.ui.widget(cx, ids!(status_pill)).redraw(cx);
    }

    // -----------------------------------------------------------------
    // Plan panel
    // -----------------------------------------------------------------
    fn refresh_plan_ui(&mut self, cx: &mut Cx, work_dir: &std::path::Path) {
        let enabled = refresh_plan(work_dir);
        self.ui
            .label(cx, ids!(plan_status_label))
            .set_text(cx, if enabled { "active" } else { "inactive" });
    }

    // -----------------------------------------------------------------
    // Sessions sidebar
    // -----------------------------------------------------------------
    fn create_and_activate_session(&mut self, cx: &mut Cx) {
        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let Some(entry) = create_new_session(&work_dir) else {
            push_chat(MsgRole::System, "Could not create a new session file.");
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
            push_chat(
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

        // Skip reload if the agent is already bound to this session file.
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
            // No agent yet — still update sidebar selection and clear chat.
            replace_chat_from_agent_messages(&[]);
            clear_activity();
            push_chat(
                MsgRole::System,
                format!("Selected session `{}` (agent not started yet).", entry.title),
            );
            self.refresh_plan_ui(cx, &entry.work_dir);
            self.refresh_activity_header(cx);
            cx.redraw_all();
            return;
        };

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

    // -----------------------------------------------------------------
    // Activity fold header
    // -----------------------------------------------------------------
    fn refresh_activity_header(&mut self, cx: &mut Cx) {
        let data = crate::state::ACTIVITY_DATA.read().unwrap();
        let summary = if data.is_empty() {
            "No recent tools".to_string()
        } else if let Some(last) = data.last() {
            let count = data.len();
            let noun = if count == 1 { "tool" } else { "tools" };
            format!(
                "{} {} · {count} {noun}",
                last.status.glyph(),
                truncate_chars(&last.name, 36)
            )
        } else {
            "No recent tools".to_string()
        };
        self.ui
            .label(cx, ids!(activity_summary))
            .set_text(cx, &summary);
    }

    // -----------------------------------------------------------------
    // Slash-command autocomplete
    // -----------------------------------------------------------------
    fn build_cmd_items(&mut self, cx: &mut Cx) {
        let mut cti = self.ui.command_text_input(cx, ids!(prompt_input));
        let search = cti.search_text(cx).to_lowercase();
        cti.clear_items(cx);
        self.cmd_items.clear();

        let commands = self.commands.clone();
        for cmd in commands
            .iter()
            .filter(|cmd| search.is_empty() || cmd.name.to_lowercase().starts_with(&search))
        {
            if let Some(widget) = self.make_cmd_item(cx, cmd) {
                self.cmd_items.push((widget.clone(), cmd.name.clone()));
                cti.add_item(cx, widget);
            }
        }
        self.ui.widget(cx, ids!(prompt_input)).redraw(cx);
    }

    fn make_cmd_item(&mut self, cx: &mut Cx, cmd: &CommandInfo) -> Option<WidgetRef> {
        let template = self.cmd_item_template.as_ref()?;
        let template_value: ScriptValue = template.as_object().into();
        let widget = cx.with_vm(|vm| WidgetRef::script_from_value(vm, template_value));
        widget
            .label(cx, ids!(cmd_name))
            .set_text(cx, &format!("/{}", cmd.name));
        widget
            .label(cx, ids!(cmd_desc))
            .set_text(cx, &cmd.description);
        Some(widget)
    }

    // -----------------------------------------------------------------
    // Prompt / command dispatch
    // -----------------------------------------------------------------
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
            push_chat(
                MsgRole::System,
                "Please provide an OpenAI API key or click 'Login ChatGPT' to authenticate.",
            );
            cx.redraw_all();
            return;
        }

        let selected_model = self.ui.drop_down(cx, ids!(model_drop)).selected_label();
        let model_name = if selected_model.is_empty() {
            "gpt-5.4".to_string()
        } else {
            selected_model
        };

        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        if self.agent.is_none() {
            let agent_opts = CodingAgentOptions {
                api_key,
                account_id,
                model: model_name,
                work_dir: work_dir.clone(),
                session_file: active_session_entry().map(|e| e.session_file),
                enable_plan_mode: false,
            };
            self.agent = Some(Arc::new(tokio::sync::Mutex::new(CodingAgent::new(
                agent_opts,
            ))));
        }

        let Some(tx) = self.tx.clone() else { return };
        let agent_arc = self.agent.as_ref().unwrap().clone();
        let input_str = input_text.trim().to_string();

        if show_in_chat {
            push_chat(MsgRole::User, input_str.clone());
        }
        self.set_status(cx, UiStatus::Working, "Working...");

        // Keep the chat pinned to the tail for the response.
        let chat_list = self.ui.widget(cx, ids!(chat_list));
        chat_list.portal_list(cx, ids!(list)).set_tail_range(true);
        cx.redraw_all();

        get_runtime().spawn(async move {
            let mut agent_lock = agent_arc.lock().await;
            let mut event_rx = agent_lock.subscribe();

            let tx_event = tx.clone();
            tokio::spawn(async move {
                while let Ok(evt) = event_rx.recv().await {
                    let _ = tx_event.send(GuiAgentEvent::Agent(evt));
                    SignalToUI::set_ui_signal();
                }
            });

            if let Some(out) = agent_lock.handle_input(&input_str).await {
                let _ = tx.send(GuiAgentEvent::CommandOutput(out));
                SignalToUI::set_ui_signal();
            }
        });
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

    // -----------------------------------------------------------------
    // Background event pump
    // -----------------------------------------------------------------
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
                GuiAgentEvent::CommandOutput(output) => {
                    push_chat(MsgRole::System, output);
                    self.set_status(cx, UiStatus::Ready, "Ready");
                    let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    self.refresh_plan_ui(cx, &work_dir);
                    refresh_sessions(&work_dir);
                }
                GuiAgentEvent::SessionSwitched {
                    session_id,
                    title,
                    work_dir,
                    messages,
                } => {
                    set_active_session(&work_dir, &session_id);
                    replace_chat_from_agent_messages(&messages);
                    clear_activity();
                    push_chat(
                        MsgRole::System,
                        format!("Switched to session `{title}`."),
                    );
                    self.refresh_plan_ui(cx, &work_dir);
                    self.refresh_activity_header(cx);
                    refresh_sessions(&work_dir);
                    set_active_session(&work_dir, &session_id);
                    self.set_status(cx, UiStatus::Ready, "Ready");
                    self.tool_msg_index.clear();
                }
                GuiAgentEvent::AvailableModelsLoaded(models) => {
                    self.ui
                        .drop_down(cx, ids!(model_drop))
                        .set_labels(cx, models);
                }
                GuiAgentEvent::DeviceCodePrompt { user_code, url } => {
                    push_chat(
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
                    push_chat(MsgRole::System, "Successfully authenticated with ChatGPT.");
                    self.ui.widget(cx, ids!(auth_row)).set_visible(cx, false);
                    self.set_status(cx, UiStatus::Ready, "Signed in");

                    if let Some(key) = key_opt {
                        self.spawn_model_fetch(key, acc_opt);
                    }
                }
                GuiAgentEvent::Agent(agent_event) => match agent_event {
                    AgentEvent::AgentStart => {
                        self.set_status(cx, UiStatus::Working, "Working...");
                    }
                    AgentEvent::TurnStart { turn_number } => {
                        push_activity(
                            None,
                            format!("Turn {turn_number}"),
                            ActivityStatus::Info,
                            "",
                        );
                    }
                    AgentEvent::MessageUpdate {
                        text_delta,
                        tool_call_name,
                        ..
                    } => {
                        if let Some(delta) = text_delta {
                            push_stream_delta(&delta);
                        }
                        if let Some(tool_name) = tool_call_name {
                            push_activity(None, tool_name, ActivityStatus::Requested, "");
                        }
                    }
                    AgentEvent::MessageEnd { .. } => {
                        flush_streaming();
                    }
                    AgentEvent::ToolExecutionStart {
                        tool_call_id,
                        name,
                        arguments,
                    } => {
                        // Flush any streamed text so the tool line lands after it.
                        flush_streaming();
                        let index = push_chat(MsgRole::Tool, format!("… {name}"));
                        self.tool_msg_index.insert(tool_call_id.clone(), index);
                        push_activity(
                            Some(tool_call_id),
                            name,
                            ActivityStatus::Running,
                            truncate_chars(&arguments, 200),
                        );
                    }
                    AgentEvent::ToolExecutionUpdate {
                        tool_call_id,
                        partial_result,
                    } => {
                        update_activity(
                            &tool_call_id,
                            None,
                            Some(truncate_chars(&partial_result, 200)),
                        );
                    }
                    AgentEvent::ToolExecutionEnd {
                        tool_call_id,
                        name,
                        result,
                    } => {
                        let status = if result.is_error {
                            ActivityStatus::Error
                        } else {
                            ActivityStatus::Done
                        };
                        update_activity(
                            &tool_call_id,
                            Some(status),
                            Some(truncate_chars(&result.content, 200)),
                        );
                        if let Some(index) = self.tool_msg_index.remove(&tool_call_id) {
                            set_chat_text(index, format!("{} {name}", status.glyph()));
                        }
                    }
                    AgentEvent::TurnEnd { .. } => {
                        self.set_status(cx, UiStatus::Working, "Turn completed");
                    }
                    AgentEvent::AgentEnd { .. } => {
                        flush_streaming();
                        self.set_status(cx, UiStatus::Ready, "Ready");
                        let work_dir =
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                        self.refresh_plan_ui(cx, &work_dir);
                        refresh_sessions(&work_dir);
                    }
                    AgentEvent::AgentError { error } => {
                        flush_streaming();
                        push_chat(MsgRole::System, format!("Agent error: {error}"));
                        self.set_status(cx, UiStatus::Error, "Error");
                    }
                    _ => {}
                },
            }
        }

        // Something changed — repaint everything (chat/plan/activity read
        // their rows from the shared state during draw).
        self.refresh_activity_header(cx);
        cx.redraw_all();
    }
}
