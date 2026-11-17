//! Data model for the composer's searchable model picker.
//!
//! The picker holds a *snapshot* of the catalog for the lifetime of one
//! popup: sections are computed once per open and typing re-filters that
//! snapshot, so a discovery refresh finishing mid-open can never reorder
//! rows under the keyboard cursor. Commit revalidates the captured row
//! against the live catalog before dispatching.

use std::collections::HashMap;
use std::path::PathBuf;


use threadlane_daemon::catalog::{
    ModelOption, ModelProvider, cached_acp_config_options, cached_acp_error,
};
use threadlane_protocol::AcpConfigOption;
use threadlane_ui_state::AppState;

/// Shown when a row captured at open no longer exists at commit time.
pub const STALE_CHOICE_MESSAGE: &str =
    "Model is no longer available. Reopen the picker to refresh.";

/// The project/session a picker snapshot belongs to. A navigation that
/// changes it invalidates the snapshot (the view recreates the picker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerOwner {
    pub work_dir: Option<PathBuf>,
    pub session_id: Option<String>,
}

impl PickerOwner {
    pub fn capture(state: &AppState) -> Self {
        Self {
            work_dir: state.active_work_dir.clone(),
            session_id: state.active_session_id.clone(),
        }
    }

    /// Whether the app still shows the context the snapshot was taken in.
    pub fn is_current(&self, state: &AppState) -> bool {
        self.work_dir == state.active_work_dir && self.session_id == state.active_session_id
    }
}

/// What confirming a picker row applies.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelPickerValue {
    /// Select a model — an `acp/<agent>` id selects that agent's current or
    /// default model, exactly like today's flat menu row.
    Model(String),
    /// Switch to agent `model_id`, then apply `(config_id, value)` to it.
    AgentChoice {
        model_id: String,
        config_id: String,
        value: String,
    },
    /// Recovery affordance when an agent did not advertise settings.
    OpenAgentSettings,
    /// Clear the cached provider inventories and re-fetch every model list.
    RefreshModels,
}

pub type ModelPickerItem = threadlane_ui_kit::PickerItem<ModelPickerValue>;
pub type PickerSection = threadlane_ui_kit::PickerSection<ModelPickerValue>;
pub type ModelPickerDelegate = threadlane_ui_kit::PickerDelegate<ModelPickerValue>;
#[cfg(test)]
use threadlane_ui_kit::filter_picker_sections as filter_sections;

/// Builds the picker's sections from a catalog snapshot.
///
/// Rows keep catalog order and providers group contiguously, matching the
/// previous menu exactly: native models list by provider, then every
/// external agent lists its advertised models inline (live options for the
/// selected agent, launch-time cache for the rest). An agent that has not
/// advertised settings still gets its row — the reason goes in its
/// secondary text so arrows only ever stop on selectable rows — followed
/// by the Settings recovery row.
pub fn picker_sections(
    options: &[ModelOption],
    acp_sections: &HashMap<String, Vec<AcpConfigOption>>,
    selected_model: &str,
) -> Vec<PickerSection> {
    let mut sections: Vec<PickerSection> = Vec::new();
    for option in options {
        if sections
            .last()
            .is_none_or(|section| section.header.as_ref() != option.provider.label())
        {
            sections.push(PickerSection {
                header: option.provider.label().into(),
                items: Vec::new(),
            });
        }
        let section = sections.last_mut().expect("section just pushed");
        let is_current = option.id == selected_model;
        if option.provider != ModelProvider::Acp {
            section.items.push(ModelPickerItem {
                value: ModelPickerValue::Model(option.id.clone()),
                title: option.label.clone().into(),
                secondary: None,
                icon_path: Some(option.provider.icon_path().into()),
                current: is_current,
                indented: false,
                haystack: ModelPickerItem::haystack_for(&[
                    &option.label,
                    &option.id,
                    option.provider.label(),
                ]),
            });
            continue;
        }

        // An agent row selects the agent's current/default model — the same
        // `selected_model` slot a native model uses.
        let agent_id = threadlane_acp_engine::acp_agent_id(&option.id)
            .unwrap_or_default()
            .to_string();
        let agent_options = acp_sections.get(&agent_id).cloned().unwrap_or_default();
        let agent_setting = threadlane_acp::config_option_for(
            &agent_options,
            threadlane_acp::ACP_CONFIG_CATEGORY_MODEL,
        )
        .cloned();
        let agent_secondary = match &agent_setting {
            Some(setting) => setting
                .current_detail_label()
                .unwrap_or_else(|| "runs its default model".to_string()),
            None => cached_acp_error(&agent_id)
                .map(|error| {
                    let short: String = error.chars().take(120).collect();
                    if error.chars().count() > 120 {
                        format!("{short}…")
                    } else {
                        short
                    }
                })
                .unwrap_or_else(|| format!("Connecting to {}…", option.label)),
        };
        section.items.push(ModelPickerItem {
            value: ModelPickerValue::Model(option.id.clone()),
            title: option.label.clone().into(),
            secondary: Some(agent_secondary.into()),
            icon_path: Some(option.provider.icon_path().into()),
            current: is_current,
            indented: false,
            haystack: ModelPickerItem::haystack_for(&[
                &option.label,
                &option.id,
                option.provider.label(),
                &agent_id,
            ]),
        });
        match agent_setting {
            Some(setting) => {
                let current = setting.current_value().map(str::to_string);
                for choice in &setting.options {
                    section.items.push(ModelPickerItem {
                        value: ModelPickerValue::AgentChoice {
                            model_id: option.id.clone(),
                            config_id: setting.id.clone(),
                            value: choice.value.clone(),
                        },
                        title: choice.name.clone().into(),
                        secondary: None,
                        icon_path: None,
                        // Only the selected agent's live state can mark a
                        // current model; other agents' cached currents may
                        // be stale, so they show none.
                        current: is_current
                            && current.as_deref() == Some(choice.value.as_str()),
                        indented: true,
                        haystack: ModelPickerItem::haystack_for(&[
                            &choice.name,
                            &choice.value,
                            &option.label,
                            &agent_id,
                        ]),
                    });
                }
            }
            None => section.items.push(ModelPickerItem {
                value: ModelPickerValue::OpenAgentSettings,
                title: "Check Settings → ACP Agents".into(),
                secondary: None,
                icon_path: None,
                current: false,
                indented: true,
                haystack: ModelPickerItem::haystack_for(&[
                    "check settings acp agents",
                    &option.label,
                    &agent_id,
                ]),
            }),
        }
    }

    // Manual recovery row: clears the cached provider inventories and
    // re-fetches every model list, so a newly released model can be picked
    // up without waiting out the refresh TTL.
    sections.push(PickerSection {
        header: "Actions".into(),
        items: vec![ModelPickerItem {
            value: ModelPickerValue::RefreshModels,
            title: "Refresh model list".into(),
            secondary: Some("Fetch the latest models from each connected provider".into()),
            icon_path: None,
            current: false,
            indented: false,
            haystack: ModelPickerItem::haystack_for(&[
                "refresh models",
                "model list",
                "clear cache",
            ]),
        }],
    });

    // Distinguishing ids as secondary text only where labels collide.
    let mut title_counts: HashMap<String, usize> = HashMap::new();
    for section in &sections {
        for item in &section.items {
            *title_counts.entry(item.title.to_string()).or_default() += 1;
        }
    }
    for section in &mut sections {
        for item in &mut section.items {
            if item.secondary.is_none()
                && title_counts.get(item.title.as_ref()).copied().unwrap_or(0) > 1
            {
                let identity = match &item.value {
                    ModelPickerValue::Model(id) => id.clone(),
                    ModelPickerValue::AgentChoice { value, .. } => value.clone(),
                    ModelPickerValue::OpenAgentSettings | ModelPickerValue::RefreshModels => {
                        continue;
                    }
                };
                item.secondary = Some(identity.into());
            }
        }
    }
    sections
}

/// Whether a row captured at open can no longer be applied to `state`:
/// the owner is checked by the caller, this checks the choice's exact
/// identity (model id, config id, choice value) against the live catalog.
pub fn choice_is_stale(value: &ModelPickerValue, state: &AppState) -> bool {
    match value {
        ModelPickerValue::OpenAgentSettings | ModelPickerValue::RefreshModels => false,
        ModelPickerValue::Model(id) => {
            !state.available_models().iter().any(|option| option.id == *id)
        }
        ModelPickerValue::AgentChoice {
            model_id,
            config_id,
            value,
        } => {
            if !state
                .available_models()
                .iter()
                .any(|option| option.id == *model_id)
            {
                return true;
            }
            let options = if state.selected_model == *model_id {
                state.active_acp_config_options()
            } else {
                threadlane_acp_engine::acp_agent_id(model_id)
                    .map(cached_acp_config_options)
                    .unwrap_or_default()
            };
            !options
                .iter()
                .any(|option| option.id == *config_id && option.has_choice(value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelPickerValue, choice_is_stale, filter_sections, picker_sections};
    use std::collections::HashMap;
    use threadlane_daemon::catalog::{ModelOption, ModelProvider};
    use threadlane_protocol::AcpConfigOption;
    use threadlane_ui_state::AppState;

    fn model(id: &str, label: &str, provider: ModelProvider) -> ModelOption {
        ModelOption {
            id: id.into(),
            label: label.into(),
            provider,
        }
    }

    fn acp_setting(choices: &[(&str, &str)], current: Option<&str>) -> AcpConfigOption {
        AcpConfigOption {
            id: "model".into(),
            name: "Model".into(),
            description: None,
            category: Some(threadlane_acp::ACP_CONFIG_CATEGORY_MODEL.into()),
            current_value: current
                .map(|value| serde_json::Value::String(value.into()))
                .unwrap_or_default(),
            options: choices
                .iter()
                .map(|(value, name)| threadlane_acp::AcpConfigOptionChoice {
                    value: value.to_string(),
                    name: name.to_string(),
                    description: None,
                })
                .collect(),
        }
    }

    #[test]
    fn sections_group_contiguous_providers_in_catalog_order() {
        let options = vec![
            model("gpt-5", "GPT-5", ModelProvider::OpenAi),
            model("gpt-5-mini", "GPT-5 Mini", ModelProvider::OpenAi),
            model("antigravity/gemini", "Gemini", ModelProvider::Antigravity),
            model("opencode/deep", "Deep", ModelProvider::OpenCode),
        ];
        let sections = picker_sections(&options, &HashMap::new(), "gpt-5-mini");
        let headers: Vec<&str> = sections.iter().map(|s| s.header.as_ref()).collect();
        assert_eq!(headers, ["OpenAI", "Antigravity", "OpenCode", "Actions"]);
        assert_eq!(
            sections[3].items[0].value,
            ModelPickerValue::RefreshModels,
            "the manual refresh row trails the provider sections"
        );
        assert_eq!(sections[0].items.len(), 2);
        assert!(sections[0].items[1].current);
        assert!(!sections[0].items[0].current);
    }

    #[test]
    fn acp_agent_rows_carry_choices_and_current_mark() {
        let options = vec![
            model("acp/claude", "Claude Code", ModelProvider::Acp),
            model("acp/gemini", "Gemini CLI", ModelProvider::Acp),
        ];
        let mut acp_sections = HashMap::new();
        acp_sections.insert(
            "claude".to_string(),
            vec![acp_setting(
                &[("opus", "Opus 4.8"), ("sonnet", "Sonnet 4.6")],
                Some("sonnet"),
            )],
        );
        acp_sections.insert(
            "gemini".to_string(),
            vec![acp_setting(&[("g3", "Gemini 3 Pro")], Some("g3"))],
        );
        // Claude is the selected agent: only its choices may be marked.
        let sections = picker_sections(&options, &acp_sections, "acp/claude");
        assert_eq!(sections.len(), 2);
        let items = &sections[0].items;
        // agent + 2 choices + agent + 1 choice
        assert_eq!(items.len(), 5);
        assert!(items[0].current, "selected agent row is current");
        assert_eq!(
            items[1].value,
            ModelPickerValue::AgentChoice {
                model_id: "acp/claude".into(),
                config_id: "model".into(),
                value: "opus".into(),
            }
        );
        assert!(!items[1].current);
        assert!(items[2].current, "claude's live current is checked");
        assert!(items[3].current == false && items[4].current == false,
            "inactive agents' cached currents must not read as current");
    }

    #[test]
    fn agent_without_settings_gets_reason_and_settings_row() {
        let options = vec![model("acp/solo", "Solo", ModelProvider::Acp)];
        let sections = picker_sections(&options, &HashMap::new(), "other");
        let items = &sections[0].items;
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[1].value,
            ModelPickerValue::OpenAgentSettings,
            "recovery row follows the agent row"
        );
        assert!(
            items[0].secondary.is_some(),
            "the connecting/error reason rides on the selectable agent row"
        );
    }

    #[test]
    fn colliding_labels_get_distinguishing_secondary() {
        use gpui_component::searchable_list::SearchableListItem as _;
        let options = vec![
            model("openai/a", "Dup", ModelProvider::OpenAi),
            model("openai/b", "Dup", ModelProvider::OpenAi),
            model("openai/c", "Unique", ModelProvider::OpenAi),
        ];
        let sections = picker_sections(&options, &HashMap::new(), "");
        assert_eq!(sections[0].items[0].secondary.as_deref(), Some("openai/a"));
        assert_eq!(sections[0].items[1].secondary.as_deref(), Some("openai/b"));
        assert!(sections[0].items[2].secondary.is_none());
        assert_eq!(sections[0].items[0].title().as_ref(), "Dup · openai/a");
        assert_eq!(sections[0].items[1].title().as_ref(), "Dup · openai/b");
        assert_eq!(sections[0].items[2].title().as_ref(), "Unique");
    }

    #[test]
    fn filter_requires_every_token_and_prunes_empty_sections() {
        let options = vec![
            model("gpt-5", "GPT-5", ModelProvider::OpenAi),
            model("antigravity/claude", "Claude Opus", ModelProvider::Antigravity),
            model("acp/claude", "Claude Code", ModelProvider::Acp),
        ];
        let mut acp_sections = HashMap::new();
        acp_sections.insert(
            "claude".to_string(),
            vec![acp_setting(&[("opus", "Opus 4.8")], None)],
        );
        let sections = picker_sections(&options, &acp_sections, "");

        let all = filter_sections(&sections, "");
        assert_eq!(all.len(), 4);

        let hits = filter_sections(&sections, "claude opus");
        assert_eq!(hits.len(), 2, "native row + agent choice survive");
        assert_eq!(hits[0].header.as_ref(), "Antigravity");
        assert_eq!(hits[1].items.len(), 1, "only the matching choice remains");

        let by_provider = filter_sections(&sections, "external claude");
        assert_eq!(by_provider.len(), 1);
        assert_eq!(by_provider[0].header.as_ref(), "External agents");

        let none = filter_sections(&sections, "nomatch token");
        assert!(none.is_empty(), "empty query surface prunes every heading");
    }

    #[test]
    fn stale_choice_detection_checks_exact_identity() {
        let mut state = AppState::default();
        state.test_set_available_models(vec![
            model("gpt-5", "GPT-5", ModelProvider::OpenAi),
            model("acp/claude", "Claude Code", ModelProvider::Acp),
        ]);
        assert!(!choice_is_stale(&ModelPickerValue::Model("gpt-5".into()), &state));
        assert!(choice_is_stale(&ModelPickerValue::Model("gone".into()), &state));
        assert!(!choice_is_stale(&ModelPickerValue::OpenAgentSettings, &state));
        // No advertised options for the agent anywhere: the captured choice
        // cannot be verified, so it is stale.
        assert!(choice_is_stale(
            &ModelPickerValue::AgentChoice {
                model_id: "acp/claude".into(),
                config_id: "model".into(),
                value: "opus".into(),
            },
            &state
        ));
    }
}
