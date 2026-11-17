//! ACP session-configuration contract types shared by the agent client
//! (`threadlane-acp`), the daemon catalog, and the daemon wire protocol.
//!
//! `AcpConfigOption` is how an external agent surfaces settings Threadlane
//! has no protocol field for — which model it runs, how much effort to
//! spend, which permission mode is active. The set is agent-defined and
//! open-ended, so options are matched by `id` and `category` rather than
//! modelled as fixed fields. These types were moved verbatim from
//! `threadlane-acp` so the daemon event stream can carry them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `category` an agent reports for the option naming its model.
pub const ACP_CONFIG_CATEGORY_MODEL: &str = "model";
/// `category` an agent reports for the option controlling its permission mode.
pub const ACP_CONFIG_CATEGORY_MODE: &str = "mode";
/// `category` an agent reports for the option controlling reasoning effort.
pub const ACP_CONFIG_CATEGORY_EFFORT: &str = "thought_level";
/// `id` of the agent-persona option, matched by id because it carries no
/// `category`.
const ACP_CONFIG_ID_AGENT: &str = "agent";

/// One choice offered by an [`AcpConfigOption`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpConfigOptionChoice {
    pub value: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// A session setting the agent exposes for the client to change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpConfigOption {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    /// Kept as raw JSON: `select` options carry strings today, but the type is
    /// agent-defined and a boolean or number must not fail the whole session.
    #[serde(default)]
    pub current_value: Value,
    #[serde(default)]
    pub options: Vec<AcpConfigOptionChoice>,
}

impl AcpConfigOption {
    pub fn current_value(&self) -> Option<&str> {
        self.current_value.as_str()
    }

    fn current_choice(&self) -> Option<&AcpConfigOptionChoice> {
        let current = self.current_value()?;
        self.options.iter().find(|choice| choice.value == current)
    }

    /// Name of the current selection, as the agent labels it.
    ///
    /// This is the control-sized label — "Default", "Plan Mode". Falls back to
    /// the raw value so an agent that reports something outside its own option
    /// list still displays truthfully.
    pub fn current_label(&self) -> Option<String> {
        self.current_choice()
            .map(|choice| choice.name.clone())
            .or_else(|| self.current_value().map(str::to_string))
    }

    /// The most specific label of the current selection.
    ///
    /// Model choices keep an explicit name ("GPT-6-Astra"); generic names such
    /// as "Default (recommended)" use the leading description segment, where
    /// agents such as Claude Code identify the concrete model. Other settings
    /// prefer their description; buttons for those settings should use
    /// [`Self::current_label`] instead.
    pub fn current_detail_label(&self) -> Option<String> {
        if self.is_category(ACP_CONFIG_CATEGORY_MODEL) {
            let label = self.current_label()?;
            let name = label
                .split('(')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            // ponytail: recognize advertised placeholder names; extend this
            // list if another agent uses a different generic model label.
            if !matches!(
                name.as_str(),
                "" | "default"
                    | "default model"
                    | "recommended"
                    | "recommended model"
                    | "auto"
                    | "automatic"
            ) {
                return Some(label);
            }
        }
        self.current_description()
            .and_then(|description| description.split(" · ").next())
            .map(str::trim)
            .filter(|head| !head.is_empty())
            .map(str::to_string)
            .or_else(|| self.current_label())
    }

    /// Description the agent gave for the current selection, which is where it
    /// names the underlying model ("Opus 4.8 with 1M context").
    fn current_description(&self) -> Option<&str> {
        self.current_choice()
            .and_then(|choice| choice.description.as_deref())
    }

    pub fn has_choice(&self, value: &str) -> bool {
        self.options.iter().any(|choice| choice.value == value)
    }

    /// Clone with `current_value` overridden for optimistic display.
    ///
    /// Only applies when the agent actually offers `value`; an unknown value
    /// is ignored so a stale pending selection cannot invent a model.
    pub(crate) fn with_current_value_override(mut self, value: &str) -> Self {
        if self.has_choice(value) {
            self.current_value = Value::String(value.to_string());
        }
        self
    }

    pub fn is_category(&self, category: &str) -> bool {
        self.category.as_deref() == Some(category)
    }

    /// Whether this option should be presented to the user for configuration.
    ///
    /// Excludes effort/thought level (owned by the reasoning picker) and
    /// agent persona (owned by the agent's internal routing).
    pub fn is_user_configurable(&self) -> bool {
        self.category.as_deref() != Some(ACP_CONFIG_CATEGORY_EFFORT)
            && self.id != ACP_CONFIG_ID_AGENT
    }
}

/// Finds the option an agent reports for `category`.
pub fn config_option_for<'a>(
    options: &'a [AcpConfigOption],
    category: &str,
) -> Option<&'a AcpConfigOption> {
    options.iter().find(|option| option.is_category(category))
}

/// Applies pending `config_id -> value` selections to cached options for
/// optimistic display before a session exists to hold them.
///
/// Unknown config ids or values the agent does not offer are ignored.
pub fn apply_pending_config_values(
    options: Vec<AcpConfigOption>,
    pending: &std::collections::HashMap<String, String>,
) -> Vec<AcpConfigOption> {
    if pending.is_empty() {
        return options;
    }
    options
        .into_iter()
        .map(|option| {
            if let Some(value) = pending.get(&option.id) {
                option.with_current_value_override(value)
            } else {
                option
            }
        })
        .collect()
}
