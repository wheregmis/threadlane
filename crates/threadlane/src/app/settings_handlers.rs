use super::*;

impl App {
    pub(super) fn handle_provider_settings_action(&mut self, cx: &mut Cx, actions: &Actions) {
        let providers_modal_uid = self.ui.widget(cx, ids!(providers_modal)).widget_uid();
        if let Some(action) = actions.find_widget_action(providers_modal_uid) {
            match action.cast::<ProviderSettingsModalAction>() {
                ProviderSettingsModalAction::ShowExtensions
                | ProviderSettingsModalAction::Refresh => self.refresh_capability_state(cx),
                ProviderSettingsModalAction::ShowSkills
                | ProviderSettingsModalAction::RefreshSkills => self.refresh_skill_state(cx),
                ProviderSettingsModalAction::ShowMcpServers
                | ProviderSettingsModalAction::RefreshMcpServers => self.refresh_mcp_state(cx),
                ProviderSettingsModalAction::Add(scope) => {
                    self.open_extension_picker(scope);
                }
                ProviderSettingsModalAction::SetEnabled { row, enabled } => {
                    self.set_extension_enabled(cx, row, enabled);
                }
                ProviderSettingsModalAction::Remove(row) => {
                    self.remove_extension(cx, row);
                }
                ProviderSettingsModalAction::SetSkillEnabled { row, enabled } => {
                    self.set_skill_enabled(cx, row, enabled);
                }
                ProviderSettingsModalAction::SetMcpEnabled { row, enabled } => {
                    self.set_mcp_enabled(cx, row, enabled);
                }
                ProviderSettingsModalAction::RemoveMcpServer(row) => {
                    self.remove_mcp_server(cx, row);
                }
                ProviderSettingsModalAction::AddMcpServer {
                    scope,
                    name,
                    command,
                } => {
                    self.add_mcp_server(cx, scope, name, command);
                }
                ProviderSettingsModalAction::ShowAcpAgents
                | ProviderSettingsModalAction::RefreshAcpAgents => self.refresh_acp_state(cx),
                ProviderSettingsModalAction::SetAcpEnabled { row, enabled } => {
                    self.set_acp_enabled(cx, row, enabled);
                }
                ProviderSettingsModalAction::RemoveAcpAgent(row) => {
                    self.remove_acp_agent(cx, row);
                }
                ProviderSettingsModalAction::AddAcpAgent {
                    scope,
                    name,
                    command,
                } => {
                    self.add_acp_agent(cx, scope, name, command);
                }
                ProviderSettingsModalAction::None => {}
            }
        }
    }
}
