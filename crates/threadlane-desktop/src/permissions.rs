//! What each agent may do in a project (wireframe 4, Plan.md §2.3): the
//! project ceiling, each agent's mode with the modes above the ceiling
//! locked, the matrix as plain statements with its overrides, the agent's
//! standing rules, how many of its tasks run at once, and the space's own
//! ceiling on Settings.

use serde_json::Value as Json;
use threadlane_core::client::{BindingSummary, StandingRuleSummary};
use threadlane_core::ids::StandingRuleId;
use threadlane_core::model::{Cell, Mode};
use threadlane_core::tools::Tier;

use crate::{AppState, Gap, Part, count_form};

/// The four positions of a mode control, in order (Plan.md §2.3).
pub const MODES: [(Mode, &str); 4] = [
    (Mode::Off, "Off"),
    (Mode::OnRequest, "On request"),
    (Mode::Assisted, "Assisted"),
    (Mode::Autonomous, "Autonomous"),
];

/// How a mode control names `mode`.
#[must_use]
pub fn mode_label(mode: Mode) -> &'static str {
    MODES
        .iter()
        .find(|(known, _)| *known == mode)
        .map_or("?", |(_, label)| label)
}

/// The tiers as the expanded row states them: what, and examples.
const TIERS: [(Tier, &str, &str); 5] = [
    (Tier::Read, "Read", "repositories, search, documentation"),
    (
        Tier::Local,
        "Work locally",
        "edit, build, test, commit on the task branch, write memory",
    ),
    (
        Tier::SharedReversible,
        "Shared, reversible",
        "comment, label and assign issues, update another pull request",
    ),
    (
        Tier::SharedHardToUndo,
        "Hard to undo",
        "merge, release, deploy, message other people",
    ),
    (
        Tier::Forbidden,
        "Forbidden",
        "delete a repository, change secrets, force-push",
    ),
];

/// The cell of `tier` in the column of `mode`: the matrix of Plan.md §2.3,
/// as `threadlane-hub`'s gate has it for an ordinary tool.
#[must_use]
pub fn matrix(tier: Tier, mode: Mode) -> Cell {
    match (tier, mode) {
        (_, Mode::Off) | (Tier::Forbidden, _) => Cell::Block,
        (Tier::Read | Tier::Local, _) => Cell::Allow,
        (Tier::SharedReversible, Mode::OnRequest) => Cell::Ask,
        (Tier::SharedReversible, _) => Cell::Review,
        (Tier::SharedHardToUndo, _) => Cell::Ask,
    }
}

/// The ceiling a project's bindings work under: the stricter of its own
/// and the space's.
#[must_use]
pub fn ceiling(space: Mode, project: Option<Mode>) -> Mode {
    project.map_or(space, |project| project.stricter(space))
}

/// The cell the gate applies to an ordinary `tier` call of an untainted
/// run of a binding at `mode` under `ceiling` with `overrides`, before
/// standing rules: an override makes it stricter, or looser up to the
/// ceiling's column (the hub's `gate`, step 3).
#[must_use]
pub fn applied(
    tier: Tier,
    mode: Mode,
    ceiling: Mode,
    overrides: &std::collections::BTreeMap<Tier, Cell>,
) -> Cell {
    let mode = mode.stricter(ceiling);
    if tier == Tier::Forbidden || mode == Mode::Off {
        return Cell::Block;
    }
    let cell = matrix(tier, mode);
    match overrides.get(&tier) {
        Some(&set) if set >= cell => set,
        Some(&set) => set.max(matrix(tier, ceiling)),
        None => cell,
    }
}

/// How a statement names a cell.
#[must_use]
pub fn cell_label(cell: Cell) -> &'static str {
    match cell {
        Cell::Allow => "Yes",
        Cell::Review => "Reviewed automatically",
        Cell::Ask => "Asks you",
        Cell::Block => "Blocked",
    }
}

/// One position of a mode control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeChoice {
    pub mode: Mode,
    pub label: &'static str,
    pub selected: bool,
    /// Why it cannot be picked: it is above a ceiling.
    pub locked: Option<String>,
}

/// The "Project ceiling" card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CeilingCard {
    /// `No agent in this project can go above this. Inherited from Work.`
    pub note: String,
    pub choices: Vec<ModeChoice>,
}

/// One agent's row on the project Team tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub agent: String,
    pub role: String,
    pub is_lead: bool,
    pub choices: Vec<ModeChoice>,
    /// Whether its "may do here" part is expanded.
    pub open: bool,
}

/// One cell an override can set a tier to, or `None` to clear it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellChoice {
    pub cell: Option<Cell>,
    pub label: String,
    pub selected: bool,
}

/// One plain statement of what an agent may do without asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// `Shared, reversible`.
    pub what: &'static str,
    /// What it covers, for example `merge, release, deploy`.
    pub examples: &'static str,
    /// The tier an override sets; `None` for the initiative row and the
    /// built-in default.
    pub tier: Option<Tier>,
    /// What applies now: `Yes`, `Asks you`, `Proposes first`, ...
    pub cell: String,
    /// The cells it can be set to here; empty when it cannot change.
    pub choices: Vec<CellChoice>,
}

/// One standing rule as the expanded row lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRow {
    pub id: StandingRuleId,
    /// `Open draft pull requests in app`.
    pub allows: String,
    /// `Granted from D-0009 · used 6 times`.
    pub from: String,
    /// While you are asked to confirm revoking it, what the question says.
    pub confirm: Option<String>,
}

/// The expanded part of an agent's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MayDo {
    /// `What Engineer may do here without asking`.
    pub title: String,
    pub statements: Vec<Statement>,
    /// `Standing rules (2)`.
    pub rules_title: String,
    /// Its rules, or why they could not be listed.
    pub rules: Result<Vec<RuleRow>, Gap>,
    /// The note that a run triggered by outside content works one mode lower.
    pub tainted: String,
}

/// What a standing rule for `tool` with target `pattern` allows, in words
/// starting in lower case: `open draft pull requests in app`.
#[must_use]
pub fn allows(tool: &str, pattern: &Json) -> String {
    let field = |name: &str| pattern.get(name).and_then(Json::as_str);
    let action = match tool {
        "open_pull_request" => "open draft pull requests".to_owned(),
        "publish_branch" => "push its task branch".to_owned(),
        "assign_issue" => "assign issues to you".to_owned(),
        "label_issue" => "label issues".to_owned(),
        "memory_edit" | "memory_create" => "change".to_owned(),
        "memory_delete" => "delete".to_owned(),
        "memory_rename" => "rename".to_owned(),
        other => format!("use {}", other.replace('_', " ")),
    };
    match (
        field("repository"),
        field("path"),
        field("from"),
        field("to"),
    ) {
        (Some(repository), ..) => format!("{action} in {repository}"),
        (None, _, Some(from), Some(to)) => format!("{action} {from} to {to}"),
        (None, Some(path), ..) => format!("{action} {path}"),
        _ => action,
    }
}

/// `text` with its first letter in upper case.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

impl AppState {
    /// The space's own ceiling, as its hub last said; Autonomous (nothing
    /// locked for it) while it has not said.
    #[must_use]
    pub fn space_ceiling(&self, space: &str) -> Mode {
        self.space(space)
            .and_then(|known| known.info.as_ref())
            .map_or(Mode::Autonomous, |info| info.mode_ceiling)
    }

    /// The ceiling of `project` in `space`: the stricter of its own and the
    /// space's.
    #[must_use]
    pub fn project_ceiling(&self, space: &str, project: &str) -> Mode {
        let own = self
            .space(space)
            .and_then(|known| known.projects.iter().find(|known| known.name == project))
            .and_then(|summary| summary.mode_ceiling);
        ceiling(self.space_ceiling(space), own)
    }

    /// The "Project ceiling" card of `project` in `space` (wireframe 4,
    /// note 1): the modes above the space's shown locked, with why.
    #[must_use]
    pub fn ceiling_card(&self, space: &str, project: &str) -> CeilingCard {
        let space_ceiling = self.space_ceiling(space);
        let own = self
            .space(space)
            .and_then(|known| known.projects.iter().find(|known| known.name == project))
            .and_then(|summary| summary.mode_ceiling);
        let current = ceiling(space_ceiling, own);
        let note = match own {
            Some(own) if own > space_ceiling => format!(
                "No agent in this project can go above this. Its own ceiling is {}, but \
                 {space} allows only {}.",
                mode_label(own),
                mode_label(space_ceiling)
            ),
            Some(_) => {
                "No agent in this project can go above this. Set for this project.".to_owned()
            }
            None => {
                format!("No agent in this project can go above this. Inherited from {space}.")
            }
        };
        CeilingCard {
            note,
            choices: MODES
                .iter()
                .map(|&(mode, label)| ModeChoice {
                    mode,
                    label,
                    selected: mode == current,
                    locked: (mode > space_ceiling).then(|| {
                        format!(
                            "Above {space}'s ceiling, {}; raise it in Settings",
                            mode_label(space_ceiling)
                        )
                    }),
                })
                .collect(),
        }
    }

    /// Takes a project ceiling its hub set: the card shows it at once.
    pub fn ceiling_set(&mut self, space: &str, project: &str, mode: Mode) {
        if let Some(summary) = self
            .spaces
            .iter_mut()
            .find(|known| known.name == space)
            .and_then(|known| {
                known
                    .projects
                    .iter_mut()
                    .find(|known| known.name == project)
            })
        {
            summary.mode_ceiling = Some(mode);
        }
    }

    /// One row per agent bound in `project` of `space`, its mode control
    /// with the modes above the project's ceiling locked (wireframe 4,
    /// note 2).
    #[must_use]
    pub fn agent_rows(&self, space: &str, project: &str) -> Vec<AgentRow> {
        let ceiling = self.project_ceiling(space, project);
        let bindings = self
            .project(space, project)
            .map(|view| view.bindings.as_slice())
            .unwrap_or_default();
        bindings
            .iter()
            .map(|binding| AgentRow {
                agent: binding.agent.clone(),
                role: binding.role.clone(),
                is_lead: binding.is_lead,
                choices: MODES
                    .iter()
                    .map(|&(mode, label)| ModeChoice {
                        mode,
                        label,
                        selected: mode == binding.mode,
                        locked: (mode > ceiling).then(|| {
                            format!("Above this project's ceiling, {}", mode_label(ceiling))
                        }),
                    })
                    .collect(),
                open: self.open_agent.as_ref()
                    == Some(&(space.to_owned(), project.to_owned(), binding.agent.clone())),
            })
            .collect()
    }

    /// Expands the "may do here" part of `agent`'s row, or collapses it.
    pub fn toggle_agent(&mut self, space: &str, project: &str, agent: &str) {
        let key = (space.to_owned(), project.to_owned(), agent.to_owned());
        self.open_agent = if self.open_agent.as_ref() == Some(&key) {
            None
        } else {
            Some(key)
        };
    }

    fn binding(&self, space: &str, project: &str, agent: &str) -> Option<&BindingSummary> {
        self.project(space, project)?
            .bindings
            .iter()
            .find(|binding| binding.agent == agent)
    }

    /// What `agent` may do in `project` of `space` without asking, as the
    /// gate would apply it now (wireframe 4, notes 3 to 5).
    #[must_use]
    pub fn may_do(&self, space: &str, project: &str, agent: &str) -> Option<MayDo> {
        let binding = self.binding(space, project, agent)?;
        let ceiling = self.project_ceiling(space, project);
        let mode = binding.mode.stricter(ceiling);
        let initiative = match mode {
            Mode::Off => "Never runs here",
            Mode::OnRequest => "Recorded, not acted on",
            Mode::Assisted => "Proposes first",
            Mode::Autonomous => "Starts it",
        };
        let mut statements = vec![Statement {
            what: "Start work you did not request",
            examples: "from outside events and its own routines",
            tier: None,
            cell: initiative.to_owned(),
            choices: Vec::new(),
        }];
        for (tier, what, examples) in TIERS {
            let cell = applied(tier, binding.mode, ceiling, &binding.overrides);
            let set = binding.overrides.get(&tier).copied();
            let changeable = tier != Tier::Forbidden && mode != Mode::Off;
            let mut choices = Vec::new();
            if changeable {
                let loosest = matrix(tier, ceiling);
                choices = [Cell::Allow, Cell::Review, Cell::Ask, Cell::Block]
                    .into_iter()
                    .filter(|choice| *choice >= loosest)
                    .map(|choice| CellChoice {
                        cell: Some(choice),
                        label: cell_label(choice).to_owned(),
                        selected: set == Some(choice),
                    })
                    .collect();
                if set.is_some() {
                    choices.push(CellChoice {
                        cell: None,
                        label: "As its mode sets it".to_owned(),
                        selected: false,
                    });
                }
            }
            statements.push(Statement {
                what,
                examples,
                tier: Some(tier),
                cell: cell_label(cell).to_owned(),
                choices,
            });
            // The gate's built-in default, which a tier 2 override turns off.
            if tier == Tier::SharedReversible && set.is_none() && mode != Mode::Off {
                statements.push(Statement {
                    what: "Its own branch and draft pull request",
                    examples: "push the task branch, open a draft pull request; decided as \
                               other shared effects are in a run triggered by outside content",
                    tier: None,
                    cell: "Yes".to_owned(),
                    choices: Vec::new(),
                });
            }
        }
        let rules = match self.project_gap(space, project, Part::Rules) {
            Some(gap) => Err(gap),
            None => Ok(self
                .project(space, project)
                .map(|view| view.rules.as_slice())
                .unwrap_or_default()
                .iter()
                .filter(|listed| listed.agent == agent && listed.rule.revoked_at.is_none())
                .map(|listed| self.rule_row(space, listed))
                .collect::<Vec<_>>()),
        };
        let count = rules.as_ref().map_or(0, Vec::len);
        Some(MayDo {
            title: format!("What {agent} may do here without asking"),
            statements,
            rules_title: format!(
                "{} ({count})",
                count_form(count, "Standing rule", "Standing rules")
            ),
            rules,
            tainted: format!(
                "When a run is triggered by outside content, {agent} works one mode lower \
                 for that run."
            ),
        })
    }

    fn rule_row(&self, space: &str, listed: &StandingRuleSummary) -> RuleRow {
        let rule = &listed.rule;
        let allows = capitalized(&allows(&rule.tool, &rule.pattern));
        let uses = match rule.uses {
            1 => "used once".to_owned(),
            n => format!("used {n} times"),
        };
        let from = match rule.created_from_decision {
            Some(decision) => format!("Granted from {decision} · {uses}"),
            None => capitalized(&uses),
        };
        let confirm = (self.revoking.as_ref() == Some(&(space.to_owned(), rule.id))).then(|| {
            format!(
                "Revoke \"{allows}\" for {}? Its next matching call asks you again.",
                listed.agent
            )
        });
        RuleRow {
            id: rule.id,
            allows,
            from,
            confirm,
        }
    }

    /// Takes overrides its hub set for `agent`: the statements show them.
    pub fn overrides_set(
        &mut self,
        space: &str,
        project: &str,
        agent: &str,
        overrides: std::collections::BTreeMap<Tier, Cell>,
    ) {
        if let Some(binding) = self
            .projects
            .get_mut(&(space.to_owned(), project.to_owned()))
            .and_then(|view| view.bindings.iter_mut().find(|b| b.agent == agent))
        {
            binding.overrides = overrides;
        }
    }

    /// Asks to revoke standing rule `rule` of `space`, which waits for you
    /// to confirm.
    pub fn ask_revoke(&mut self, space: &str, rule: StandingRuleId) {
        self.revoking = Some((space.to_owned(), rule));
    }

    /// Keeps the rule you asked to revoke.
    pub fn keep_rule(&mut self) {
        self.revoking = None;
    }

    /// Takes a rule its hub revoked: it leaves the list.
    pub fn rule_revoked(&mut self, space: &str, revoked: &StandingRuleSummary) {
        self.revoking = None;
        for ((at, _), view) in &mut self.projects {
            if at != space {
                continue;
            }
            for listed in &mut view.rules {
                if listed.rule.id == revoked.rule.id {
                    *listed = revoked.clone();
                }
            }
        }
    }

    /// The space ceiling control of `space` on Settings: each mode, the
    /// space's own selected.
    #[must_use]
    pub fn space_ceiling_choices(&self, space: &str) -> Vec<ModeChoice> {
        let current = self
            .space(space)
            .and_then(|known| known.info.as_ref())
            .map(|info| info.mode_ceiling);
        MODES
            .iter()
            .map(|&(mode, label)| ModeChoice {
                mode,
                label,
                selected: Some(mode) == current,
                locked: None,
            })
            .collect()
    }

    /// Asks to set the ceiling of `space` to `mode`, which waits for you to
    /// confirm.
    pub fn ask_space_ceiling(&mut self, space: &str, mode: Mode) {
        self.space_ceiling_asked = Some((space.to_owned(), mode));
    }

    /// Keeps the space's ceiling as it is.
    pub fn keep_space_ceiling(&mut self) {
        self.space_ceiling_asked = None;
    }

    /// The mode you asked to set `space`'s ceiling to, and what confirming
    /// it permits, while it waits for you.
    #[must_use]
    pub fn space_ceiling_confirm(&self, space: &str) -> Option<(Mode, String)> {
        let (asked, mode) = self.space_ceiling_asked.as_ref()?;
        if asked != space {
            return None;
        }
        let permits = match mode {
            Mode::Off => "No agent will run in any project of this space.",
            Mode::OnRequest => {
                "Agents start work only when you ask, and ask before any shared effect \
                 except pushing their task branch and opening a draft pull request. \
                 Bindings above it act at On request."
            }
            Mode::Assisted => {
                "Agents may also propose work, and perform shared effects other than pushing \
                 their task branch and opening a draft pull request after an automatic \
                 review. They still ask for anything hard to undo."
            }
            Mode::Autonomous => {
                "Agents may also start work on their own from events, routines and their own \
                 follow-ups, and perform hard-to-undo actions you granted a standing rule for."
            }
        };
        Some((
            *mode,
            format!(
                "Set the ceiling of {space} to {}? It applies to every project without a \
                 stricter ceiling of its own. {permits}",
                mode_label(*mode)
            ),
        ))
    }

    /// Takes the ceiling its hub set for `space`.
    pub fn space_ceiling_set(&mut self, space: &str, mode: Mode) {
        self.space_ceiling_asked = None;
        if let Some(info) = self
            .spaces
            .iter_mut()
            .find(|known| known.name == space)
            .and_then(|known| known.info.as_mut())
        {
            info.mode_ceiling = mode;
        }
    }
}
