//! ProviderSettingsModal component for managing LLM providers, extensions, skills, and MCP servers.

use makepad_widgets::*;
use threadlane_coding_agent::ExtensionScope;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
    #[default]
    GoogleAntigravity,
    OpenAi,
    OpenCodeGo,
    Capabilities,
    Skills,
    McpServers,
    AcpAgents,
    About,
}

#[derive(Clone, Debug, Default)]
pub enum ProviderSettingsModalAction {
    ShowExtensions,
    ShowSkills,
    ShowMcpServers,
    Add(ExtensionScope),
    Refresh,
    SetEnabled {
        row: usize,
        enabled: bool,
    },
    Remove(usize),
    SetSkillEnabled {
        row: usize,
        enabled: bool,
    },
    RefreshSkills,
    SetMcpEnabled {
        row: usize,
        enabled: bool,
    },
    RemoveMcpServer(usize),
    RefreshMcpServers,
    AddMcpServer {
        scope: threadlane_coding_agent::McpScope,
        name: String,
        command: String,
    },
    ShowAcpAgents,
    SetAcpEnabled {
        row: usize,
        enabled: bool,
    },
    RemoveAcpAgent(usize),
    RefreshAcpAgents,
    AddAcpAgent {
        scope: threadlane_coding_agent::AcpScope,
        name: String,
        command: String,
    },
    #[default]
    None,
}

pub fn script_mod(_vm: &mut ScriptVm) {}

#[derive(Script, Widget)]
pub struct ProviderSettingsModal {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    draw_list: Option<DrawList2d>,
    #[rust]
    opened: bool,
    #[rust]
    pub page: SettingsPage,
    #[rust]
    extension_rows: Vec<crate::state::CapabilityExtensionRow>,
    #[rust]
    skill_rows: Vec<crate::state::CapabilitySkillRow>,
    #[rust]
    mcp_rows: Vec<crate::state::CapabilityMcpRow>,
    #[rust]
    acp_rows: Vec<crate::state::CapabilityAcpRow>,
    #[rust]
    install_scope_global: bool,
}

impl ScriptHook for ProviderSettingsModal {
    fn on_after_new(&mut self, vm: &mut ScriptVm) {
        self.draw_list = Some(DrawList2d::script_new(vm));
    }

    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        _apply: &Apply,
        _scope: &mut Scope,
        _value: ScriptValue,
    ) {
        vm.with_cx_mut(|cx| {
            if let Some(draw_list) = &self.draw_list {
                draw_list.redraw(cx);
            }
            let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
            self.view
                .label(cx, ids!(about_version_lbl))
                .set_text(cx, &version);
            self.sync_page_visibility(cx);
        });
    }
}

impl Widget for ProviderSettingsModal {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.opened {
            return;
        }

        self.view.handle_event(cx, event, scope);
        if let Event::MouseUp(mouse) = event {
            if mouse.button.is_primary() {
                for input_id in [
                    ids!(mcp_name_input),
                    ids!(mcp_command_input),
                    ids!(acp_name_input),
                    ids!(acp_command_input),
                ] {
                    let input = self.view.text_input(cx, input_id);
                    if input.area().rect(cx).contains(mouse.abs) {
                        input.set_key_focus(cx);
                        break;
                    }
                }
            }
        }
        if let Event::Actions(actions) = event {
            let uid = self.widget_uid();
            if self
                .view
                .button(cx, ids!(settings_nav_google_btn))
                .clicked(actions)
            {
                self.set_page(cx, SettingsPage::GoogleAntigravity);
            }
            if self
                .view
                .button(cx, ids!(settings_nav_openai_btn))
                .clicked(actions)
            {
                self.set_page(cx, SettingsPage::OpenAi);
            }
            if self
                .view
                .button(cx, ids!(settings_nav_opencode_btn))
                .clicked(actions)
            {
                self.set_page(cx, SettingsPage::OpenCodeGo);
            }
            if self
                .view
                .button(cx, ids!(settings_nav_capabilities_btn))
                .clicked(actions)
            {
                self.set_page(cx, SettingsPage::Capabilities);
                cx.widget_action(uid, ProviderSettingsModalAction::ShowExtensions);
            }
            if self
                .view
                .button(cx, ids!(settings_nav_skills_btn))
                .clicked(actions)
            {
                self.set_page(cx, SettingsPage::Skills);
                cx.widget_action(uid, ProviderSettingsModalAction::ShowSkills);
            }
            if self
                .view
                .button(cx, ids!(settings_nav_mcp_btn))
                .clicked(actions)
            {
                self.set_page(cx, SettingsPage::McpServers);
                cx.widget_action(uid, ProviderSettingsModalAction::ShowMcpServers);
            }
            if self
                .view
                .button(cx, ids!(settings_nav_acp_btn))
                .clicked(actions)
            {
                self.set_page(cx, SettingsPage::AcpAgents);
                cx.widget_action(uid, ProviderSettingsModalAction::ShowAcpAgents);
            }
            if self
                .view
                .button(cx, ids!(settings_nav_about_btn))
                .clicked(actions)
            {
                self.set_page(cx, SettingsPage::About);
            }
            if self
                .view
                .button(cx, ids!(capability_scope_global_btn))
                .clicked(actions)
            {
                self.install_scope_global = true;
                self.sync_install_scope(cx);
            }
            if self
                .view
                .button(cx, ids!(capability_scope_project_btn))
                .clicked(actions)
                || self
                    .view
                    .button(cx, ids!(mcp_scope_project_btn))
                    .clicked(actions)
                || self
                    .view
                    .button(cx, ids!(acp_scope_project_btn))
                    .clicked(actions)
            {
                self.install_scope_global = false;
                self.sync_install_scope(cx);
            }
            if self
                .view
                .button(cx, ids!(mcp_scope_global_btn))
                .clicked(actions)
                || self
                    .view
                    .button(cx, ids!(acp_scope_global_btn))
                    .clicked(actions)
            {
                self.install_scope_global = true;
                self.sync_install_scope(cx);
            }
            if self
                .view
                .button(cx, ids!(mcp_submit_add_btn))
                .clicked(actions)
            {
                let name = self.view.text_input(cx, ids!(mcp_name_input)).text();
                let command = self.view.text_input(cx, ids!(mcp_command_input)).text();
                let scope = if self.install_scope_global {
                    threadlane_coding_agent::McpScope::Global
                } else {
                    threadlane_coding_agent::McpScope::Project
                };
                cx.widget_action(
                    uid,
                    ProviderSettingsModalAction::AddMcpServer {
                        scope,
                        name,
                        command,
                    },
                );
            }
            if self
                .view
                .button(cx, ids!(acp_submit_add_btn))
                .clicked(actions)
            {
                let name = self.view.text_input(cx, ids!(acp_name_input)).text();
                let command = self.view.text_input(cx, ids!(acp_command_input)).text();
                let scope = if self.install_scope_global {
                    threadlane_coding_agent::AcpScope::Global
                } else {
                    threadlane_coding_agent::AcpScope::Project
                };
                cx.widget_action(
                    uid,
                    ProviderSettingsModalAction::AddAcpAgent {
                        scope,
                        name,
                        command,
                    },
                );
            }
            if self
                .view
                .button(cx, ids!(capability_add_btn))
                .clicked(actions)
            {
                cx.widget_action(uid, ProviderSettingsModalAction::Add(self.install_scope()));
            }
            if self
                .view
                .button(cx, ids!(capability_refresh_btn))
                .clicked(actions)
            {
                cx.widget_action(uid, ProviderSettingsModalAction::Refresh);
            }
            if self
                .view
                .button(cx, ids!(skill_refresh_btn))
                .clicked(actions)
            {
                cx.widget_action(uid, ProviderSettingsModalAction::RefreshSkills);
            }
            if self.view.button(cx, ids!(mcp_refresh_btn)).clicked(actions) {
                cx.widget_action(uid, ProviderSettingsModalAction::RefreshMcpServers);
            }
            if self.view.button(cx, ids!(acp_refresh_btn)).clicked(actions) {
                cx.widget_action(uid, ProviderSettingsModalAction::RefreshAcpAgents);
            }
            let capability_list = self.view.portal_list(cx, ids!(capability_list));
            for (row, item) in capability_list.items_with_actions(actions) {
                if let Some(enabled) = item.check_box(cx, ids!(enabled_toggle)).changed(actions) {
                    cx.widget_action(
                        uid,
                        ProviderSettingsModalAction::SetEnabled { row, enabled },
                    );
                }
                if item.button(cx, ids!(remove_btn)).clicked(actions) {
                    cx.widget_action(uid, ProviderSettingsModalAction::Remove(row));
                }
            }
            let skill_list = self.view.portal_list(cx, ids!(skill_list));
            for (row, item) in skill_list.items_with_actions(actions) {
                if let Some(enabled) = item.check_box(cx, ids!(enabled_toggle)).changed(actions) {
                    cx.widget_action(
                        uid,
                        ProviderSettingsModalAction::SetSkillEnabled { row, enabled },
                    );
                }
            }
            let mcp_list = self.view.portal_list(cx, ids!(mcp_list));
            for (row, item) in mcp_list.items_with_actions(actions) {
                if let Some(enabled) = item.check_box(cx, ids!(enabled_toggle)).changed(actions) {
                    cx.widget_action(
                        uid,
                        ProviderSettingsModalAction::SetMcpEnabled { row, enabled },
                    );
                }
                if item.button(cx, ids!(remove_btn)).clicked(actions) {
                    cx.widget_action(uid, ProviderSettingsModalAction::RemoveMcpServer(row));
                }
            }
            let acp_list = self.view.portal_list(cx, ids!(acp_list));
            for (row, item) in acp_list.items_with_actions(actions) {
                if let Some(enabled) = item.check_box(cx, ids!(enabled_toggle)).changed(actions) {
                    cx.widget_action(
                        uid,
                        ProviderSettingsModalAction::SetAcpEnabled { row, enabled },
                    );
                }
                if item.button(cx, ids!(remove_btn)).clicked(actions) {
                    cx.widget_action(uid, ProviderSettingsModalAction::RemoveAcpAgent(row));
                }
            }
        }
        let modal_rect = self.view.widget(cx, ids!(modal_card)).area().rect(cx);
        let should_close = matches!(
            event,
            Event::MouseUp(event)
                if event.button.is_primary() && !modal_rect.contains(event.abs)
        ) || matches!(event, Event::KeyDown(event) if event.key_code == KeyCode::Escape)
            || matches!(event, Event::BackPressed { .. });
        if should_close {
            self.close(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, _walk: Walk) -> DrawStep {
        let draw_list = self.draw_list.as_mut().unwrap();
        draw_list.begin_overlay_reuse(cx);

        let pass_size = cx.current_pass_size();
        cx.begin_root_turtle(pass_size, Layout::flow_overlay());
        if self.opened {
            let walk = Walk {
                width: Size::Fixed(pass_size.x),
                height: Size::Fixed(pass_size.y),
                ..Default::default()
            };
            while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
                if let Some(mut list) = item.as_portal_list().borrow_mut() {
                    if self.page == SettingsPage::Skills {
                        list.set_item_range(cx, 0, self.skill_rows.len().max(1));
                        while let Some(row_index) = list.next_visible_item(cx) {
                            if self.skill_rows.is_empty() {
                                if row_index == 0 {
                                    list.item(cx, row_index, id!(SkillEmptyRow))
                                        .draw_all_unscoped(cx);
                                }
                                continue;
                            }
                            let Some(row) = self.skill_rows.get(row_index) else {
                                continue;
                            };
                            let item = list.item(cx, row_index, id!(SkillRow));
                            item.label(cx, ids!(name_lbl)).set_text(cx, &row.id);
                            item.label(cx, ids!(scope_lbl))
                                .set_text(cx, &row.scope_status());
                            item.label(cx, ids!(path_lbl))
                                .set_text(cx, &row.file_path.display().to_string());
                            item.check_box(cx, ids!(enabled_toggle)).set_active(
                                cx,
                                row.enabled && row.is_valid,
                                Animate::No,
                            );
                            item.draw_all_unscoped(cx);
                        }
                    } else if self.page == SettingsPage::McpServers {
                        list.set_item_range(cx, 0, self.mcp_rows.len().max(1));
                        while let Some(row_index) = list.next_visible_item(cx) {
                            if self.mcp_rows.is_empty() {
                                if row_index == 0 {
                                    list.item(cx, row_index, id!(McpEmptyRow))
                                        .draw_all_unscoped(cx);
                                }
                                continue;
                            }
                            let Some(row) = self.mcp_rows.get(row_index) else {
                                continue;
                            };
                            let item = list.item(cx, row_index, id!(McpRow));
                            item.label(cx, ids!(name_lbl)).set_text(cx, &row.name);
                            item.label(cx, ids!(scope_lbl))
                                .set_text(cx, &row.scope_status());
                            item.label(cx, ids!(path_lbl))
                                .set_text(cx, &row.transport_detail);
                            item.check_box(cx, ids!(enabled_toggle)).set_active(
                                cx,
                                row.enabled,
                                Animate::No,
                            );
                            item.draw_all_unscoped(cx);
                        }
                    } else if self.page == SettingsPage::AcpAgents {
                        list.set_item_range(cx, 0, self.acp_rows.len().max(1));
                        while let Some(row_index) = list.next_visible_item(cx) {
                            if self.acp_rows.is_empty() {
                                if row_index == 0 {
                                    list.item(cx, row_index, id!(AcpEmptyRow))
                                        .draw_all_unscoped(cx);
                                }
                                continue;
                            }
                            let Some(row) = self.acp_rows.get(row_index) else {
                                continue;
                            };
                            let item = list.item(cx, row_index, id!(AcpRow));
                            item.label(cx, ids!(name_lbl)).set_text(cx, &row.name);
                            item.label(cx, ids!(scope_lbl))
                                .set_text(cx, &row.scope_status());
                            item.label(cx, ids!(path_lbl))
                                .set_text(cx, &row.command_detail);
                            item.check_box(cx, ids!(enabled_toggle)).set_active(
                                cx,
                                row.enabled,
                                Animate::No,
                            );
                            item.draw_all_unscoped(cx);
                        }
                    } else if self.page == SettingsPage::Capabilities {
                        list.set_item_range(cx, 0, self.extension_rows.len().max(1));
                        while let Some(row_index) = list.next_visible_item(cx) {
                            if self.extension_rows.is_empty() {
                                if row_index == 0 {
                                    list.item(cx, row_index, id!(EmptyRow))
                                        .draw_all_unscoped(cx);
                                }
                                continue;
                            }
                            let Some(row) = self.extension_rows.get(row_index) else {
                                continue;
                            };
                            let item = list.item(cx, row_index, id!(ExtensionRow));
                            item.label(cx, ids!(name_lbl))
                                .set_text(cx, &format!("{} · v{}", row.name, row.version));
                            item.label(cx, ids!(scope_lbl))
                                .set_text(cx, &row.scope_status());
                            item.label(cx, ids!(path_lbl))
                                .set_text(cx, &row.module_path.display().to_string());
                            item.check_box(cx, ids!(enabled_toggle)).set_active(
                                cx,
                                row.enabled,
                                Animate::No,
                            );
                            item.draw_all_unscoped(cx);
                        }
                    }
                }
            }
        }
        cx.end_pass_sized_turtle();
        draw_list.end(cx);
        DrawStep::done()
    }
}

impl ProviderSettingsModal {
    pub fn install_scope(&self) -> ExtensionScope {
        if self.install_scope_global {
            ExtensionScope::Global
        } else {
            ExtensionScope::Project
        }
    }

    pub fn redraw_capability_overlay(&mut self, cx: &mut Cx) {
        if let Some(draw_list) = &self.draw_list {
            draw_list.redraw(cx);
        }
        self.view.redraw(cx);
    }

    pub fn set_extension_rows(
        &mut self,
        cx: &mut Cx,
        rows: Vec<crate::state::CapabilityExtensionRow>,
    ) {
        self.extension_rows = rows;
        self.redraw_capability_overlay(cx);
    }

    pub fn set_skill_rows(&mut self, cx: &mut Cx, rows: Vec<crate::state::CapabilitySkillRow>) {
        self.skill_rows = rows;
        self.redraw_capability_overlay(cx);
    }

    pub fn set_mcp_rows(&mut self, cx: &mut Cx, rows: Vec<crate::state::CapabilityMcpRow>) {
        self.mcp_rows = rows;
        self.redraw_capability_overlay(cx);
    }

    pub fn set_acp_rows(&mut self, cx: &mut Cx, rows: Vec<crate::state::CapabilityAcpRow>) {
        self.acp_rows = rows;
        self.redraw_capability_overlay(cx);
    }

    pub fn set_acp_status(&mut self, cx: &mut Cx, status: &str) {
        self.view
            .label(cx, ids!(acp_status_lbl))
            .set_text(cx, status);
    }

    pub fn set_mcp_status(&mut self, cx: &mut Cx, status: &str) {
        self.view
            .label(cx, ids!(mcp_status_lbl))
            .set_text(cx, status);
    }

    pub fn set_skill_status(&mut self, cx: &mut Cx, status: &str) {
        self.view
            .label(cx, ids!(skill_status_lbl))
            .set_text(cx, status);
    }

    pub fn set_extension_status(&mut self, cx: &mut Cx, status: &str) {
        self.view
            .label(cx, ids!(capability_status_lbl))
            .set_text(cx, status);
    }

    pub fn sync_install_scope(&mut self, cx: &mut Cx) {
        let normal_color = Vec4f {
            x: 0x2b as f32 / 255.0,
            y: 0x31 as f32 / 255.0,
            z: 0x3d as f32 / 255.0,
            w: 1.0,
        };
        let selected_color = Vec4f {
            x: 0x2d as f32 / 255.0,
            y: 0x40 as f32 / 255.0,
            z: 0x5a as f32 / 255.0,
            w: 1.0,
        };
        let normal_border = Vec4f {
            x: 0x3a as f32 / 255.0,
            y: 0x43 as f32 / 255.0,
            z: 0x54 as f32 / 255.0,
            w: 1.0,
        };
        let selected_border = Vec4f {
            x: 0x4b as f32 / 255.0,
            y: 0x71 as f32 / 255.0,
            z: 0x9f as f32 / 255.0,
            w: 1.0,
        };
        for (button_id, selected) in [
            (ids!(capability_scope_global_btn), self.install_scope_global),
            (
                ids!(capability_scope_project_btn),
                !self.install_scope_global,
            ),
            (ids!(mcp_scope_global_btn), self.install_scope_global),
            (ids!(mcp_scope_project_btn), !self.install_scope_global),
            (ids!(acp_scope_global_btn), self.install_scope_global),
            (ids!(acp_scope_project_btn), !self.install_scope_global),
        ] {
            let mut button = self.view.button(cx, button_id);
            let color = if selected {
                selected_color
            } else {
                normal_color
            };
            let border_color = if selected {
                selected_border
            } else {
                normal_border
            };
            script_apply_eval!(cx, button, {
                draw_bg +: {
                    color: #(color)
                    border_color: #(border_color)
                }
            });
            button.redraw(cx);
        }
    }

    pub fn set_page(&mut self, cx: &mut Cx, page: SettingsPage) {
        self.page = page;
        self.sync_page_visibility(cx);
        self.view.redraw(cx);
    }

    pub fn sync_page_visibility(&mut self, cx: &mut Cx) {
        let google_selected = self.page == SettingsPage::GoogleAntigravity;
        let openai_selected = self.page == SettingsPage::OpenAi;
        let opencode_selected = self.page == SettingsPage::OpenCodeGo;
        let capabilities_selected = self.page == SettingsPage::Capabilities;
        let skills_selected = self.page == SettingsPage::Skills;
        let mcp_selected = self.page == SettingsPage::McpServers;
        let acp_selected = self.page == SettingsPage::AcpAgents;
        let about_selected = self.page == SettingsPage::About;

        for (button_id, selected) in [
            (ids!(settings_nav_google_btn), google_selected),
            (ids!(settings_nav_openai_btn), openai_selected),
            (ids!(settings_nav_opencode_btn), opencode_selected),
            (ids!(settings_nav_capabilities_btn), capabilities_selected),
            (ids!(settings_nav_skills_btn), skills_selected),
            (ids!(settings_nav_mcp_btn), mcp_selected),
            (ids!(settings_nav_acp_btn), acp_selected),
            (ids!(settings_nav_about_btn), about_selected),
        ] {
            let button = self.view.button(cx, button_id);
            crate::components::nav_button::set_selected(cx, &button, selected);
            button.redraw(cx);
        }

        self.view
            .button(cx, ids!(settings_nav_google_btn))
            .set_visible(cx, true);
        self.view
            .button(cx, ids!(settings_nav_openai_btn))
            .set_visible(cx, true);
        self.view
            .button(cx, ids!(settings_nav_opencode_btn))
            .set_visible(cx, true);
        self.view
            .button(cx, ids!(settings_nav_capabilities_btn))
            .set_visible(cx, true);
        self.view
            .button(cx, ids!(settings_nav_skills_btn))
            .set_visible(cx, true);
        self.view
            .button(cx, ids!(settings_nav_mcp_btn))
            .set_visible(cx, true);
        self.view
            .button(cx, ids!(settings_nav_acp_btn))
            .set_visible(cx, true);
        self.view
            .button(cx, ids!(settings_nav_about_btn))
            .set_visible(cx, true);
        self.view
            .widget(cx, ids!(google_antigravity_page))
            .set_visible(cx, google_selected);
        self.view
            .widget(cx, ids!(openai_page))
            .set_visible(cx, openai_selected);
        self.view
            .widget(cx, ids!(opencode_page))
            .set_visible(cx, opencode_selected);
        self.view
            .widget(cx, ids!(capabilities_page))
            .set_visible(cx, capabilities_selected);
        self.view
            .widget(cx, ids!(skills_page))
            .set_visible(cx, skills_selected);
        self.view
            .widget(cx, ids!(mcp_page))
            .set_visible(cx, mcp_selected);
        self.view
            .widget(cx, ids!(acp_page))
            .set_visible(cx, acp_selected);
        self.view
            .widget(cx, ids!(about_page))
            .set_visible(cx, about_selected);
        self.sync_install_scope(cx);
    }

    pub fn open(&mut self, cx: &mut Cx) {
        self.open_page(cx, SettingsPage::GoogleAntigravity);
    }

    pub fn open_page(&mut self, cx: &mut Cx, page: SettingsPage) {
        self.page = page;
        self.opened = true;
        self.sync_page_visibility(cx);
        if let Some(draw_list) = &self.draw_list {
            draw_list.redraw(cx);
        }
        self.view.redraw(cx);
    }

    pub fn close(&mut self, cx: &mut Cx) {
        if !self.opened {
            return;
        }
        self.opened = false;
        if let Some(draw_list) = &self.draw_list {
            draw_list.redraw(cx);
        }
        self.view.redraw(cx);
    }
}
