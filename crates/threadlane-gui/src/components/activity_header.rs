//! ActivityHeader and ActivitySvgIcon components for tool and thinking activity items.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*

    mod.components.ActivitySvgIcon = View {
        width: 14
        height: 14
        visible: false
        icon := Icon {
            width: Fill
            height: Fill
            icon_walk: Walk{width: 14 height: 14}
            draw_icon +: { color: #x7f8b9a }
        }
    }

    mod.components.ActivityHeader = RoundedView {
        width: Fit
        height: 28
        cursor: MouseCursor.Hand
        padding: Inset{left: 3 top: 2 right: 3 bottom: 2}
        flow: Right
        spacing: 7
        align: Align{y: 0.5}
        draw_bg +: {
            color: #x00000000
            border_radius: 5.0
            border_size: 0.0
        }
        icon_tile := View {
            width: 20
            height: 20
            align: Align{x: 0.5 y: 0.5}
            icon_stack := View {
                width: 14
                height: 14
                flow: Overlay
                icon_generic := mod.components.ActivitySvgIcon {
                    visible: true
                    icon +: { draw_icon +: { svg: crate_resource("self:resources/icons/tool.svg") } }
                }
                icon_thinking := mod.components.ActivitySvgIcon {
                    icon +: { draw_icon +: { svg: crate_resource("self:resources/icons/thinking.svg") } }
                }
                icon_read_file := mod.components.ActivitySvgIcon {
                    icon +: { draw_icon +: { svg: crate_resource("self:resources/icons/read-file.svg") } }
                }
                icon_write_file := mod.components.ActivitySvgIcon {
                    icon +: { draw_icon +: { svg: crate_resource("self:resources/icons/write-file.svg") } }
                }
                icon_edit_file := mod.components.ActivitySvgIcon {
                    icon +: { draw_icon +: { svg: crate_resource("self:resources/icons/edit-file.svg") } }
                }
                icon_list_directory := mod.components.ActivitySvgIcon {
                    icon +: { draw_icon +: { svg: crate_resource("self:resources/icons/list-directory.svg") } }
                }
                icon_terminal := mod.components.ActivitySvgIcon {
                    icon +: { draw_icon +: { svg: crate_resource("self:resources/icons/terminal.svg") } }
                }
                icon_skill := mod.components.ActivitySvgIcon {
                    icon +: { draw_icon +: { svg: crate_resource("self:resources/icons/skill.svg") } }
                }
                icon_subagent := mod.components.ActivitySvgIcon {
                    icon +: { draw_icon +: { svg: crate_resource("self:resources/icons/subagent.svg") } }
                }
            }
        }
        title_lbl := Label {
            width: 108
            height: Fit
            text: "Tool"
            draw_text +: {
                color: #xb8c0cc
                text_style: theme.font_bold { font_size: 9.5 }
            }
        }
        summary := View {
            width: Fit
            height: Fit
            flow: Right
            spacing: 8
            align: Align{y: 0.5}
        }
        fold_slot := View {
            width: 18
            height: 20
            align: Align{x: 0.5 y: 0.5}
            fold_button := FoldButton {
                width: 15
                draw_bg +: { active: 0.0 fade: 0.0 }
                animator +: { active: { default: @off } }
            }
        }
    }
}
