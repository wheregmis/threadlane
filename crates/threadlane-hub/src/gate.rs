//! The permission gate as one pure function (Plan.md §2.3, §9.1,
//! docs/milestone-9.md decision 1): a tool call and what governs it in, one
//! [`Verdict`] out. The gateway gathers the inputs and calls it for every
//! call.

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json};
use threadlane_core::forge::{GITHUB_CLI, ONTO_TASK, OPEN_PULL_REQUEST, PUBLISH_BRANCH};
use threadlane_core::model::{Cell, Mode, StandingRule};
use threadlane_core::tools::{Tier, Verdict};

use crate::tools::{ASK_USER, CREATE_TASK};

/// A binding's overrides: the cell each tier is set to (Plan.md §2.3).
pub type Overrides = BTreeMap<Tier, Cell>;

/// The argument fields that name what a call acts on. A standing rule
/// matches them exactly and ignores every other field (decision 4).
const TARGET_FIELDS: &[&str] = &["repository", "path", "from", "to"];

/// Everything the gate decides from.
#[derive(Debug, Clone, Copy)]
pub struct GateInput<'a> {
    pub tool: &'a str,
    pub tier: Tier,
    pub arguments: &'a Json,
    /// The binding's stored mode; Off when the agent is not bound.
    pub mode: Mode,
    pub space_ceiling: Mode,
    pub project_ceiling: Option<Mode>,
    pub overrides: &'a Overrides,
    /// The project's standing rules, whichever agent each was created from,
    /// revoked ones included.
    pub rules: &'a [StandingRule],
    /// The run consumes external content (Plan.md §9.1 step 2).
    pub tainted: bool,
    /// You triggered the run: every event it consumes is from you.
    pub requested: bool,
    /// Set for `create_task` in a run that is not work you requested (its
    /// task is not, and no event it consumes is): the assignee's binding
    /// mode in the project. The call starts new work, so the initiative row
    /// decides it, at the stricter of the two agents' modes (Plan.md §2.3).
    pub new_work: Option<Mode>,
    /// Set when the run's task is work you requested and the call acts on
    /// its own branch, or on the pull request of its own or another
    /// requested task's branch in the project, as the hub checked it
    /// (docs/autonomy-rework.md §3 stage A).
    pub own_work: Option<OwnWork>,
}

/// A call on requested work's own branch or pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnWork {
    /// For `merge`: the reviewer approved the pull request's current head,
    /// its checks passed, and it merges cleanly. Always true for the other
    /// own-work tools.
    pub checks_pass: bool,
}

/// What the matrix has a row for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// An ordinary tool, by its tier.
    Tier(Tier),
    /// A change to what agents are told: a protected memory file you did
    /// not ask for (docs/milestone-5.md). Asks in every mode.
    Instructions,
    /// Starting work you did not request: a task from unrequested work (a
    /// proposal), or an agent's routine, whose events are never requested
    /// (Plan.md §7.3). Asks below Autonomous.
    Initiative,
    /// A requested task's own step: publishing its branch, opening its draft,
    /// readying, editing or merging the pull request of requested work. Allowed in every mode but
    /// Off, reviewed when the run read outside content, and asked when a
    /// merge's checks do not pass.
    OwnWork { tainted: bool, checks_pass: bool },
}

/// The cell of `row` in the column of `mode` (Plan.md §2.3, the matrix).
fn cell(row: Row, mode: Mode) -> Cell {
    match (row, mode) {
        (_, Mode::Off) | (Row::Tier(Tier::Forbidden), _) => Cell::Block,
        (Row::Tier(Tier::Read | Tier::Local), _) => Cell::Allow,
        (Row::Tier(Tier::SharedReversible), Mode::OnRequest) => Cell::Ask,
        (Row::Tier(Tier::SharedReversible), _) => Cell::Review,
        (Row::Initiative, Mode::Autonomous) => Cell::Allow,
        (
            Row::OwnWork {
                checks_pass: false, ..
            },
            _,
        ) => Cell::Ask,
        (Row::OwnWork { tainted: true, .. }, _) => Cell::Review,
        (Row::OwnWork { .. }, _) => Cell::Allow,
        (Row::Tier(Tier::SharedHardToUndo) | Row::Instructions | Row::Initiative, _) => Cell::Ask,
    }
}

/// Whether a lane may start a run on events that are not work you requested
/// (Plan.md §2.3, the initiative row), for an agent of effective `mode`: On request records them and starts nothing; Assisted
/// and Autonomous start. Off starts nothing at all, and is decided first.
#[must_use]
pub fn may_start_unrequested(mode: Mode) -> bool {
    mode >= Mode::Assisted
}

/// The pattern a standing rule made from this call stores: its target
/// fields, as given.
#[must_use]
pub fn rule_pattern(tool: &str, arguments: &Json) -> Json {
    // A batch merge is approved once for its named, pinned list, never by a standing rule (#692).
    // A tag is decided each time: a rule would recreate a deleted tag unasked (#748).
    if (tool == crate::MERGE && arguments.get("pulls").is_some())
        || tool == threadlane_core::forge::CREATE_TAG
    {
        return Json::Object(Map::new());
    }
    let mut targets: Map<String, Json> = TARGET_FIELDS
        .iter()
        .filter_map(|field| {
            let value = arguments.get(*field)?;
            value
                .is_string()
                .then(|| ((*field).to_owned(), value.clone()))
        })
        .collect();
    // A rule for a task names its assignee: "always start this kind here".
    if tool == CREATE_TASK
        && let Some(agent) = arguments.get("agent").filter(|agent| agent.is_string())
    {
        targets.insert("agent".into(), agent.clone());
    }
    // A standing rule for these covers one pull request or issue, not the whole repository.
    // label_issue is left out: its rule covers label edits on the whole repository (#653).
    if matches!(
        tool,
        crate::FAST_FORWARD_PULL_REQUEST
            | crate::CLOSE_PULL_REQUEST
            | crate::EDIT_PULL_REQUEST
            | crate::RESOLVE_REVIEW_THREAD
            | crate::REQUEST_REVIEW
            | threadlane_core::forge::ATTACH_IMAGE
    ) && let Some(number) = arguments["number"].as_u64()
    {
        targets.insert("number".into(), number.into());
    }
    // A standing rule for these covers only the PR head it was shown.
    if matches!(
        tool,
        crate::CLOSE_PULL_REQUEST | crate::RESOLVE_REVIEW_THREAD | crate::REQUEST_REVIEW
    ) && let Some(head) = arguments.get(threadlane_core::forge::EXPECTED_HEAD)
    {
        targets.insert(threadlane_core::forge::EXPECTED_HEAD.into(), head.clone());
    }
    Json::Object(targets)
}

/// A binding's stored overrides. Entries that do not parse are left out,
/// which leaves those tiers to the matrix.
#[must_use]
pub fn overrides(stored: &Json) -> Overrides {
    stored
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(tier, cell)| {
            let tier = serde_json::from_value(Json::String(tier.clone())).ok()?;
            Some((tier, serde_json::from_value(cell.clone()).ok()?))
        })
        .collect()
}

/// The ceiling a project's bindings work under: the stricter of its own
/// and the space's.
#[must_use]
pub fn ceiling(space: Mode, project: Option<Mode>) -> Mode {
    project.map_or(space, |project| project.stricter(space))
}

/// The loosest cell an override may set `tier` to under `ceiling`: the
/// ceiling's column (Plan.md §2.3 "Overrides").
#[must_use]
pub fn loosest_override(tier: Tier, ceiling: Mode) -> Cell {
    cell(Row::Tier(tier), ceiling)
}

/// Whether the built-in default covers the call: opening a draft pull
/// request, or pushing the task's own branch (no `onto_task`).
fn allowed_by_default(tool: &str, arguments: &Json) -> bool {
    tool == OPEN_PULL_REQUEST
        || (tool == PUBLISH_BRANCH && arguments.get(ONTO_TASK).is_none_or(Json::is_null))
}

/// Whether the own-work row can decide the call: the default's tools, and
/// readying, editing or merging a pull request.
pub(crate) fn own_work_tool(tool: &str, arguments: &Json) -> bool {
    allowed_by_default(tool, arguments)
        || matches!(
            tool,
            crate::MARK_READY | crate::EDIT_PULL_REQUEST | crate::MERGE
        )
}

/// Decides one call, first rule that applies wins (Plan.md §9.1):
///
/// 1. the forbidden tier blocks;
/// 2. the effective mode is the binding's, capped by the ceilings, one step
///    lower if tainted; Off blocks, and `ask_user` always asks;
/// 3. the binding's override for the tier makes the cell stricter, or looser
///    up to the ceiling's column (stepped down too if tainted); a standing
///    rule matching the call then allows a tier 0 to 2 call in any mode, and
///    has a tier 3 call reviewed in Autonomous; a rule for a task's assignee
///    allows an untainted proposal where both agents act in Assisted;
/// 4. otherwise, a call the cell would ask about or review is allowed by
///    default when it opens a draft pull request or pushes the task's own
///    branch and the binding sets no override for the tier; if the run is
///    tainted, it is reviewed instead; an untainted `github_cli` call that
///    only reads (a strict allowlist) is allowed by default too;
/// 5. otherwise the matrix's cell for the row and the effective mode. A
///    requested task's own step is on the own-work row, whose allow is
///    recorded as allowed by default; an override only makes it stricter.
#[must_use]
pub fn gate(input: &GateInput<'_>) -> Verdict {
    // shortcut: the forbidden list is the built-in tier only; add space and
    // project additions when a system tool can be forbidden by name there.
    if input.tier == Tier::Forbidden {
        return Verdict::Block {
            reason: format!("{} is forbidden", input.tool),
        };
    }
    let ceiling = ceiling(input.space_ceiling, input.project_ceiling);
    let (mode, cap) = match input.mode.stricter(ceiling) {
        Mode::Off => {
            return Verdict::Block {
                reason: "the agent is off in this project".to_owned(),
            };
        }
        mode if input.tainted => (mode.stepped_down(), ceiling.stepped_down()),
        mode => (mode, ceiling),
    };
    if input.tool == ASK_USER {
        return Verdict::Ask { offers_rule: false };
    }
    let row = if input.tool == crate::timers::CREATE_SCHEDULE || input.new_work.is_some() {
        Row::Initiative
    } else if !input.requested && crate::memory::protected(input.tool, input.arguments) {
        Row::Instructions
    } else if let Some(own) = input
        .own_work
        .filter(|_| own_work_tool(input.tool, input.arguments))
    {
        Row::OwnWork {
            tainted: input.tainted,
            checks_pass: own.checks_pass,
        }
    } else {
        Row::Tier(input.tier)
    };
    let row_mode = input
        .new_work
        .map_or(mode, |assignee| mode.stricter(assignee));
    let mut verdict = cell(row, row_mode);
    if let Some(&set) = input.overrides.get(&input.tier) {
        verdict = match row {
            _ if set >= verdict => set,
            // An override loosens only a tier's own cell.
            Row::Tier(_) => set.max(cell(row, cap)),
            _ => verdict,
        };
    }
    let pattern = rule_pattern(input.tool, input.arguments);
    let ruled = match row {
        Row::Tier(Tier::SharedReversible | Tier::SharedHardToUndo)
        | Row::Instructions
        | Row::OwnWork { .. } => pattern.as_object().is_some_and(|p| !p.is_empty()),
        // A task for an assignee you said to always start: only where both
        // agents start unrequested work, so never Off or On request, and not
        // from a run that read outside content, whose task would be yours.
        Row::Initiative => {
            input.new_work.is_some()
                && !input.tainted
                && row_mode == Mode::Assisted
                && pattern.as_object().is_some_and(|p| !p.is_empty())
        }
        _ => false,
    };
    let mut reviewed_by = None;
    if ruled && matches!(verdict, Cell::Ask | Cell::Review) {
        let rule = input.rules.iter().find(|rule| {
            rule.revoked_at.is_none() && rule.tool == input.tool && rule.pattern == pattern
        });
        match rule {
            Some(rule) if input.tier < Tier::SharedHardToUndo => {
                return Verdict::Allow {
                    rule: Some(rule.id),
                };
            }
            Some(rule) if mode == Mode::Autonomous => {
                reviewed_by = (verdict == Cell::Ask).then_some(rule.id);
                verdict = Cell::Review;
            }
            _ => {}
        }
    }
    if matches!(verdict, Cell::Ask | Cell::Review)
        && row == Row::Tier(input.tier)
        && !input.overrides.contains_key(&input.tier)
        && allowed_by_default(input.tool, input.arguments)
    {
        if !input.tainted {
            return Verdict::AllowByDefault;
        }
        // Taint limits reach, not routine: the run's own draft is reviewed.
        verdict = Cell::Review;
    }
    // A read-only GitHub check asks nothing of you; a tainted run keeps the cell (#706).
    if matches!(verdict, Cell::Ask | Cell::Review)
        && row == Row::Tier(input.tier)
        && !input.tainted
        && !input.overrides.contains_key(&input.tier)
        && input.tool == GITHUB_CLI
        && crate::publish::read_only_gh(input.arguments)
    {
        return Verdict::AllowByDefault;
    }
    // A tier 3 rule applies only where the agent acts in Autonomous.
    let offers_rule = ruled && (input.tier < Tier::SharedHardToUndo || mode == Mode::Autonomous);
    match verdict {
        Cell::Allow if matches!(row, Row::OwnWork { .. }) => Verdict::AllowByDefault,
        Cell::Allow => Verdict::Allow { rule: None },
        Cell::Review => Verdict::Review {
            offers_rule,
            rule: reviewed_by,
        },
        Cell::Ask => Verdict::Ask { offers_rule },
        Cell::Block => Verdict::Block {
            reason: format!(
                "this agent's settings in this project do not allow {}",
                input.tool
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use threadlane_core::ids::{AgentId, ProjectId, StandingRuleId};
    use threadlane_core::time::Timestamp;

    use super::*;
    use crate::memory::MEMORY_EDIT;
    use crate::timers::CREATE_SCHEDULE;
    use crate::tools::CREATE_TASK;

    const ON: Mode = Mode::OnRequest;
    const AS: Mode = Mode::Assisted;
    const AU: Mode = Mode::Autonomous;

    /// What the table expects: the verdict, short.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum V {
        Allow,
        Rule,
        Default,
        Review,
        Ask,
        AskOffer,
        Block,
    }

    fn short(verdict: &Verdict) -> V {
        match verdict {
            Verdict::Allow { rule: None } => V::Allow,
            Verdict::Allow { rule: Some(_) } => V::Rule,
            Verdict::AllowByDefault => V::Default,
            Verdict::Review { .. } => V::Review,
            Verdict::Ask { offers_rule: false } => V::Ask,
            Verdict::Ask { offers_rule: true } => V::AskOffer,
            Verdict::Block { .. } => V::Block,
        }
    }

    fn rule(tool: &str, pattern: Json, revoked: bool) -> StandingRule {
        StandingRule {
            id: StandingRuleId(1),
            project_id: ProjectId(1),
            agent_id: AgentId(1),
            tool: tool.to_owned(),
            pattern,
            tier: Tier::SharedReversible,
            created_from_decision: None,
            created_at: Timestamp::from_millis(0),
            revoked_at: revoked.then(|| Timestamp::from_millis(1)),
            uses: 0,
        }
    }

    /// One row of the table: the call, what governs it, and the verdict.
    struct Case {
        what: &'static str,
        tool: &'static str,
        tier: Tier,
        mode: Mode,
        space: Mode,
        project: Option<Mode>,
        overrides: &'static [(Tier, Cell)],
        rule: Option<&'static str>,
        tainted: bool,
        requested: bool,
        /// The assignee's mode, for a task from unrequested work.
        new_work: Option<Mode>,
        /// Whether a merge's checks pass, for a requested task's own step.
        own: Option<bool>,
        expect: V,
    }

    const fn case(what: &'static str, tier: Tier, mode: Mode, expect: V) -> Case {
        Case {
            what,
            tool: "tool",
            tier,
            mode,
            space: AU,
            project: None,
            overrides: &[],
            rule: None,
            tainted: false,
            requested: true,
            new_work: None,
            own: None,
            expect,
        }
    }

    const T0: Tier = Tier::Read;
    const T1: Tier = Tier::Local;
    const T2: Tier = Tier::SharedReversible;
    const T3: Tier = Tier::SharedHardToUndo;
    const TF: Tier = Tier::Forbidden;

    #[test]
    fn the_gate_follows_the_matrix_with_ceilings_overrides_rules_and_taint() {
        use V::{Allow, Ask, AskOffer, Block, Default, Review, Rule};
        let tainted = |c: Case| Case { tainted: true, ..c };
        let proposal = |c: Case, assignee: Mode| Case {
            tool: CREATE_TASK,
            requested: false,
            new_work: Some(assignee),
            ..c
        };
        let ruled = |c: Case| Case {
            tool: "comment",
            rule: Some("app"),
            ..c
        };
        let drafted = |c: Case| Case {
            tool: "open_pull_request",
            ..c
        };
        let pushed = |c: Case| Case {
            tool: "publish_branch",
            ..c
        };
        let table: Vec<Case> = vec![
            // The matrix, untainted: Plan.md §2.3, column by column.
            case("tier 0 on request", T0, ON, Allow),
            case("tier 0 assisted", T0, AS, Allow),
            case("tier 0 autonomous", T0, AU, Allow),
            case("tier 1 on request", T1, ON, Allow),
            case("tier 1 assisted", T1, AS, Allow),
            case("tier 1 autonomous", T1, AU, Allow),
            case("tier 2 on request", T2, ON, AskOffer),
            case("tier 2 assisted", T2, AS, Review),
            case("tier 2 autonomous", T2, AU, Review),
            case("tier 3 on request", T3, ON, Ask),
            case(
                "tier 3 assisted offers no rule it would not apply",
                T3,
                AS,
                Ask,
            ),
            case("tier 3 autonomous", T3, AU, AskOffer),
            case("forbidden on request", TF, ON, Block),
            case("forbidden assisted", TF, AS, Block),
            case("forbidden autonomous", TF, AU, Block),
            case("off may do nothing", T0, Mode::Off, Block),
            // The same cells, tainted: one mode lower, at least On request.
            tainted(case("tainted tier 0 on request", T0, ON, Allow)),
            tainted(case("tainted tier 0 assisted", T0, AS, Allow)),
            tainted(case("tainted tier 0 autonomous", T0, AU, Allow)),
            tainted(case("tainted tier 1 on request", T1, ON, Allow)),
            tainted(case("tainted tier 1 assisted", T1, AS, Allow)),
            tainted(case("tainted tier 1 autonomous", T1, AU, Allow)),
            tainted(case("tainted tier 2 on request", T2, ON, AskOffer)),
            tainted(case("tainted tier 2 assisted asks", T2, AS, AskOffer)),
            tainted(case("tainted tier 2 autonomous", T2, AU, Review)),
            tainted(case("tainted tier 3 on request", T3, ON, Ask)),
            tainted(case("tainted tier 3 assisted", T3, AS, Ask)),
            tainted(case("tainted tier 3 autonomous", T3, AU, Ask)),
            tainted(case("tainted forbidden", TF, AU, Block)),
            tainted(case("tainted off", T0, Mode::Off, Block)),
            // Ceilings: the binding acts at the lower of its mode and both.
            Case {
                space: ON,
                ..case("space ceiling caps assisted", T2, AS, AskOffer)
            },
            Case {
                project: Some(AS),
                ..case("project ceiling caps autonomous", T2, AU, Review)
            },
            Case {
                project: Some(ON),
                ..case("project ceiling on request", T2, AU, AskOffer)
            },
            Case {
                project: Some(Mode::Off),
                ..case("project ceiling off", T0, AU, Block)
            },
            Case {
                space: AS,
                project: Some(AU),
                ..case("a project cannot lift the space's", T2, AU, Review)
            },
            Case {
                space: AS,
                ..case("tier 3 offers no rule below Autonomous", T3, AS, Ask)
            },
            // Overrides: stricter always, looser up to the ceiling's column.
            Case {
                overrides: &[(T1, Cell::Ask)],
                ..case("override asks for tier 1", T1, AU, Ask)
            },
            Case {
                overrides: &[(T0, Cell::Block)],
                ..case("override blocks reads", T0, ON, Block)
            },
            Case {
                overrides: &[(T2, Cell::Ask)],
                ..case("override asks instead of review", T2, AU, AskOffer)
            },
            Case {
                overrides: &[(T2, Cell::Review)],
                ..case("override loosens to the ceiling's review", T2, ON, Review)
            },
            Case {
                overrides: &[(T2, Cell::Allow)],
                ..case("override cannot pass the ceiling's column", T2, ON, Review)
            },
            Case {
                project: Some(ON),
                overrides: &[(T2, Cell::Review)],
                ..case("override capped by a lower ceiling", T2, AU, AskOffer)
            },
            Case {
                overrides: &[(T3, Cell::Allow)],
                ..case("tier 3 never loosens past ask", T3, AU, AskOffer)
            },
            tainted(Case {
                overrides: &[(T2, Cell::Review)],
                ..case("taint lowers an override's cap", T2, ON, Review)
            }),
            tainted(Case {
                space: AS,
                overrides: &[(T2, Cell::Review)],
                ..case("taint under Assisted caps at ask", T2, ON, AskOffer)
            }),
            Case {
                overrides: &[(T2, Cell::Block)],
                ..ruled(case("a blocking override beats a rule", T2, AU, Block))
            },
            // Standing rules: tier 2 in any mode, tier 3 only in Autonomous.
            ruled(case("tier 2 rule on request", T2, ON, Rule)),
            ruled(case("tier 2 rule assisted", T2, AS, Rule)),
            ruled(case("tier 2 rule autonomous", T2, AU, Rule)),
            tainted(ruled(case("tier 2 rule tainted", T2, ON, Rule))),
            ruled(case("tier 3 rule on request", T3, ON, Ask)),
            ruled(case("tier 3 rule assisted", T3, AS, Ask)),
            ruled(case("tier 3 rule autonomous is reviewed", T3, AU, Review)),
            tainted(ruled(case("tier 3 rule tainted", T3, AU, Ask))),
            Case {
                rule: Some("other"),
                ..ruled(case("a rule for another target", T2, ON, AskOffer))
            },
            Case {
                tool: "create_issue",
                rule: Some("app"),
                ..case("a rule for another tool", T2, ON, AskOffer)
            },
            // The default: a draft pull request and the task's own branch,
            // untainted and with no override for the tier.
            drafted(case("a draft pull request on request", T2, ON, Default)),
            drafted(case("a draft pull request assisted", T2, AS, Default)),
            drafted(case("a draft pull request autonomous", T2, AU, Default)),
            pushed(case("the task branch on request", T2, ON, Default)),
            pushed(case("the task branch assisted", T2, AS, Default)),
            pushed(case("the task branch autonomous", T2, AU, Default)),
            tainted(drafted(case("a tainted draft on request", T2, ON, Review))),
            tainted(drafted(case("a tainted draft assisted", T2, AS, Review))),
            tainted(drafted(case("a tainted draft autonomous", T2, AU, Review))),
            tainted(pushed(case("a tainted push on request", T2, ON, Review))),
            tainted(pushed(case("a tainted push assisted", T2, AS, Review))),
            tainted(pushed(case("a tainted push autonomous", T2, AU, Review))),
            tainted(drafted(Case {
                overrides: &[(T2, Cell::Ask)],
                ..case("an override decides a tainted draft", T2, AU, AskOffer)
            })),
            tainted(pushed(Case {
                overrides: &[(T2, Cell::Ask)],
                ..case("an override decides a tainted push", T2, ON, AskOffer)
            })),
            tainted(Case {
                tool: "comment",
                ..case("a tainted comment asks", T2, AS, AskOffer)
            }),
            drafted(Case {
                overrides: &[(T2, Cell::Ask)],
                ..case("an override decides a draft", T2, AU, AskOffer)
            }),
            pushed(Case {
                overrides: &[(T2, Cell::Review)],
                ..case("an override decides a push", T2, ON, Review)
            }),
            pushed(Case {
                overrides: &[(T2, Cell::Block)],
                ..case("an override blocks a push", T2, AU, Block)
            }),
            drafted(case("an off agent opens nothing", T2, Mode::Off, Block)),
            drafted(Case {
                project: Some(Mode::Off),
                ..case("an off ceiling opens nothing", T2, AU, Block)
            }),
            Case {
                tool: "comment",
                ..case("a comment still asks", T2, ON, AskOffer)
            },
            Case {
                tool: "assign_issue",
                ..case("an assignment is still reviewed", T2, AS, Review)
            },
            Case {
                tool: "merge",
                ..case("a merge still asks", T3, ON, Ask)
            },
            Case {
                rule: Some("revoked"),
                ..ruled(case("a revoked rule", T2, ON, AskOffer))
            },
            // Today's special rows: a question, an agent's routine, and a
            // protected memory file you did not ask for.
            Case {
                tool: ASK_USER,
                ..case("ask_user asks", T0, AU, Ask)
            },
            // The initiative row: an agent's routine, and a task from work
            // you did not request, at the stricter of the two agents' modes.
            Case {
                tool: CREATE_SCHEDULE,
                ..case("an agent's routine asks", T1, ON, Ask)
            },
            Case {
                tool: CREATE_SCHEDULE,
                ..case("an agent's routine asks in assisted", T1, AS, Ask)
            },
            Case {
                tool: CREATE_SCHEDULE,
                ..case("an agent's routine is allowed in autonomous", T1, AU, Allow)
            },
            tainted(Case {
                tool: CREATE_SCHEDULE,
                ..case("a tainted autonomous routine asks", T1, AU, Ask)
            }),
            Case {
                tool: CREATE_SCHEDULE,
                overrides: &[(T1, Cell::Ask)],
                ..case("an override makes a routine ask", T1, AU, Ask)
            },
            Case {
                tool: CREATE_SCHEDULE,
                project: Some(AS),
                ..case("a ceiling makes a routine ask", T1, AU, Ask)
            },
            proposal(case("a proposal in assisted", T1, AS, AskOffer), AS),
            proposal(case("a task in autonomous", T1, AU, Allow), AU),
            proposal(case("an on request assignee is asked for", T1, AU, Ask), ON),
            proposal(
                case("an assisted assignee is asked for", T1, AU, AskOffer),
                AS,
            ),
            proposal(
                case("an autonomous assignee under assisted", T1, AS, AskOffer),
                AU,
            ),
            proposal(case("an off assignee blocks", T1, AU, Block), Mode::Off),
            tainted(proposal(
                case("tainted autonomous lead proposes", T1, AU, Ask),
                AU,
            )),
            Case {
                project: Some(AS),
                ..proposal(case("a ceiling caps both agents", T1, AU, AskOffer), AU)
            },
            Case {
                overrides: &[(T1, Cell::Allow)],
                ..proposal(
                    case("an override cannot loosen a proposal", T1, AS, AskOffer),
                    AS,
                )
            },
            Case {
                overrides: &[(T1, Cell::Ask)],
                ..proposal(case("an override makes a task ask", T1, AU, Ask), AU)
            },
            // "Always start this kind here": a rule for the assignee starts
            // it where both agents act in Assisted, and nowhere else.
            Case {
                rule: Some("assignee"),
                ..proposal(case("an assignee's rule starts it", T1, AS, Rule), AS)
            },
            Case {
                rule: Some("assignee"),
                ..proposal(case("an assignee's rule under assisted", T1, AU, Rule), AS)
            },
            Case {
                rule: Some("other assignee"),
                ..proposal(case("a rule for another assignee", T1, AS, AskOffer), AS)
            },
            Case {
                rule: Some("revoked assignee"),
                ..proposal(case("a revoked assignee's rule", T1, AS, AskOffer), AS)
            },
            Case {
                rule: Some("assignee"),
                ..proposal(case("no rule starts on request work", T1, AS, Ask), ON)
            },
            Case {
                rule: Some("assignee"),
                ..proposal(case("nor an on request lead's", T1, ON, Ask), AS)
            },
            Case {
                rule: Some("assignee"),
                ..proposal(case("nor an off assignee's", T1, AS, Block), Mode::Off)
            },
            Case {
                rule: Some("assignee"),
                ..proposal(case("autonomous needs no rule", T1, AU, Allow), AU)
            },
            tainted(Case {
                rule: Some("assignee"),
                ..proposal(case("nor a tainted run's", T1, AU, Ask), AU)
            }),
            Case {
                rule: Some("assignee"),
                overrides: &[(T1, Cell::Block)],
                ..proposal(case("a blocking override beats it", T1, AS, Block), AS)
            },
            Case {
                tool: CREATE_TASK,
                ..case("a task from work you requested", T1, ON, Allow)
            },
            Case {
                tool: MEMORY_EDIT,
                requested: false,
                ..case("an unrequested protected write asks", T1, AU, AskOffer)
            },
            Case {
                tool: MEMORY_EDIT,
                ..case("a protected write you asked for", T1, ON, Allow)
            },
            Case {
                tool: MEMORY_EDIT,
                requested: false,
                rule: Some("memory"),
                ..case("a rule for the protected file", T1, ON, Rule)
            },
        ];
        // A requested task's own steps (docs/autonomy-rework.md §3 stage A):
        // allowed by default in every mode, reviewed when tainted, and a
        // merge whose checks fail asks.
        let own = |c: Case, tool: &'static str, checks_pass: bool| Case {
            tool,
            own: Some(checks_pass),
            ..c
        };
        let own_tools = [
            ("publish_branch", T2),
            ("open_pull_request", T2),
            ("mark_ready", T2),
            ("edit_pull_request", T2),
            ("merge", T3),
        ];
        let mut table = table;
        for (tool, tier) in own_tools {
            for mode in [ON, AS, AU] {
                table.extend([
                    own(case("own work", tier, mode, Default), tool, true),
                    tainted(own(
                        case("tainted own work", tier, mode, Review),
                        tool,
                        true,
                    )),
                    own(case("own work off", tier, Mode::Off, Block), tool, true),
                    Case {
                        project: Some(Mode::Off),
                        ..own(case("own work off ceiling", tier, mode, Block), tool, true)
                    },
                    Case {
                        space: ON,
                        ..own(
                            case("own work under on request", tier, mode, Default),
                            tool,
                            true,
                        )
                    },
                    Case {
                        overrides: &[(T2, Cell::Block), (T3, Cell::Block)],
                        ..own(
                            case("an override blocks own work", tier, mode, Block),
                            tool,
                            true,
                        )
                    },
                    Case {
                        overrides: &[(T2, Cell::Allow), (T3, Cell::Allow)],
                        ..tainted(own(
                            case("an override cannot loosen own work", tier, mode, Review),
                            tool,
                            true,
                        ))
                    },
                    own(case("own work forbidden", TF, mode, Block), tool, true),
                ]);
            }
        }
        for mode in [ON, AS, AU] {
            table.extend([
                // As any tier 3 ask: a rule is offered where one would apply.
                own(
                    case(
                        "a merge whose checks fail",
                        T3,
                        mode,
                        if mode == AU { AskOffer } else { Ask },
                    ),
                    "merge",
                    false,
                ),
                tainted(own(
                    case("a tainted merge whose checks fail", T3, mode, Ask),
                    "merge",
                    false,
                )),
            ]);
        }
        table.extend([
            Case {
                overrides: &[(T2, Cell::Ask)],
                ..own(
                    case("an override asks about an own push", T2, AU, AskOffer),
                    "publish_branch",
                    true,
                )
            },
            Case {
                overrides: &[(T2, Cell::Review)],
                ..own(
                    case("an override reviews an own push", T2, ON, Review),
                    "publish_branch",
                    true,
                )
            },
            Case {
                overrides: &[(T3, Cell::Ask)],
                ..own(
                    case("an override asks about own merge", T3, ON, Ask),
                    "merge",
                    true,
                )
            },
            Case {
                overrides: &[(T3, Cell::Ask)],
                ..own(
                    case(
                        "an override asks about own merge autonomous",
                        T3,
                        AU,
                        AskOffer,
                    ),
                    "merge",
                    true,
                )
            },
            Case {
                rule: Some("merge"),
                ..own(
                    case("a merge rule still reviews failing checks", T3, AU, Review),
                    "merge",
                    false,
                )
            },
            tainted(Case {
                rule: Some("merge"),
                ..own(
                    case("a tainted merge rule asks on failing checks", T3, AU, Ask),
                    "merge",
                    false,
                )
            }),
            own(
                case("a comment is not own work", T2, ON, AskOffer),
                "comment",
                true,
            ),
            own(
                case("nor is a close", T3, AU, AskOffer),
                "close_pull_request",
                true,
            ),
        ]);
        let mut failures = Vec::new();
        for row in &table {
            let arguments = if row.tool == MEMORY_EDIT {
                json!({ "path": "space/instructions.md", "old_text": "a", "new_text": "b" })
            } else if row.tool == CREATE_TASK {
                json!({ "agent": "Reviewer", "title": "Free text is ignored", "goal": "g" })
            } else {
                json!({ "repository": "app", "title": "Free text is ignored" })
            };
            let rules: Vec<StandingRule> = match row.rule {
                None => Vec::new(),
                Some("memory") => vec![rule(
                    MEMORY_EDIT,
                    json!({ "path": "space/instructions.md" }),
                    false,
                )],
                Some("revoked") => vec![rule("comment", json!({ "repository": "app" }), true)],
                Some("merge") => vec![rule(crate::MERGE, json!({ "repository": "app" }), false)],
                Some("assignee") => vec![rule(CREATE_TASK, json!({ "agent": "Reviewer" }), false)],
                Some("other assignee") => {
                    vec![rule(CREATE_TASK, json!({ "agent": "Writer" }), false)]
                }
                Some("revoked assignee") => {
                    vec![rule(CREATE_TASK, json!({ "agent": "Reviewer" }), true)]
                }
                Some(name) => vec![rule("comment", json!({ "repository": name }), false)],
            };
            let overrides: Overrides = row.overrides.iter().copied().collect();
            let got = gate(&GateInput {
                tool: row.tool,
                tier: row.tier,
                arguments: &arguments,
                mode: row.mode,
                space_ceiling: row.space,
                project_ceiling: row.project,
                overrides: &overrides,
                rules: &rules,
                tainted: row.tainted,
                requested: row.requested,
                new_work: row.new_work,
                own_work: row.own.map(|checks_pass| OwnWork { checks_pass }),
            });
            if short(&got) != row.expect {
                failures.push(format!("{}: {got:?}, expected {:?}", row.what, row.expect));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn a_label_rule_covers_labelling_any_issue_in_its_repository_only() {
        let first = json!({"repository":"app","number":7,"add":["bug"]});
        let rules = vec![rule(
            crate::LABEL_ISSUE,
            rule_pattern(crate::LABEL_ISSUE, &first),
            false,
        )];
        let overrides = Overrides::new();
        let decide = |tool: &str, arguments: Json| {
            short(&gate(&GateInput {
                tool,
                tier: T2,
                arguments: &arguments,
                mode: ON,
                space_ceiling: AU,
                project_ceiling: None,
                overrides: &overrides,
                rules: &rules,
                tainted: false,
                requested: true,
                new_work: None,
                own_work: None,
            }))
        };
        let other_issue = json!({"repository":"app","number":8,"remove":["question"]});
        assert_eq!(decide(crate::LABEL_ISSUE, other_issue.clone()), V::Rule);
        assert_eq!(
            decide(
                crate::LABEL_ISSUE,
                json!({"repository":"web","number":8,"add":["bug"]})
            ),
            V::AskOffer
        );
        assert_eq!(decide(crate::COMMENT, other_issue), V::AskOffer);
    }

    #[test]
    fn a_merge_rule_never_covers_a_batch_and_a_batch_offers_no_rule() {
        let batch =
            json!({"repository":"app","pulls":[{"number":7,"expected_head":"0".repeat(40)}]});
        assert_eq!(rule_pattern(crate::MERGE, &batch), json!({}));
        let rules = vec![rule(
            crate::MERGE,
            rule_pattern(crate::MERGE, &json!({"repository":"app","number":7})),
            false,
        )];
        let overrides = Overrides::new();
        for mode in [ON, AS, AU] {
            let verdict = gate(&GateInput {
                tool: crate::MERGE,
                tier: Tier::SharedHardToUndo,
                arguments: &batch,
                mode,
                space_ceiling: AU,
                project_ceiling: None,
                overrides: &overrides,
                rules: &rules,
                tainted: false,
                requested: true,
                new_work: None,
                own_work: None,
            });
            assert_eq!(short(&verdict), V::Ask, "{mode}");
        }
    }

    #[test]
    fn a_tag_is_asked_every_time_even_with_a_matching_rule() {
        let arguments = json!({"repository":"app","tag":"v1.0.0","commit":"0".repeat(40)});
        // A rule stored with the tag's own fields, as an earlier build made.
        let rules = vec![rule(
            threadlane_core::forge::CREATE_TAG,
            arguments.clone(),
            false,
        )];
        let overrides = Overrides::new();
        for mode in [ON, AS, AU] {
            let verdict = gate(&GateInput {
                tool: threadlane_core::forge::CREATE_TAG,
                tier: Tier::SharedHardToUndo,
                arguments: &arguments,
                mode,
                space_ceiling: AU,
                project_ceiling: None,
                overrides: &overrides,
                rules: &rules,
                tainted: false,
                requested: true,
                new_work: None,
                own_work: None,
            });
            assert_eq!(short(&verdict), V::Ask, "{mode}");
        }
    }

    #[test]
    fn work_you_did_not_request_starts_in_assisted_and_autonomous_only() {
        for (mode, ceiling, starts) in [
            (Mode::Off, AU, false),
            (ON, AU, false),
            (AS, AU, true),
            (AU, AU, true),
            (AU, AS, true),
            (AU, ON, false),
            (AS, Mode::Off, false),
        ] {
            assert_eq!(
                may_start_unrequested(mode.stricter(ceiling)),
                starts,
                "{mode} under {ceiling}"
            );
        }
    }

    #[test]
    fn a_pattern_keeps_the_target_fields_and_drops_free_text() {
        assert_eq!(
            rule_pattern(
                "create_issue",
                &json!({ "repository": "app", "title": "x", "body": "y" })
            ),
            json!({ "repository": "app" })
        );
        assert_eq!(
            rule_pattern("memory_rename", &json!({ "from": "a.md", "to": "b.md" })),
            json!({ "from": "a.md", "to": "b.md" })
        );
        assert_eq!(
            rule_pattern(MEMORY_EDIT, &json!({ "content": "z" })),
            json!({})
        );
        assert_eq!(
            rule_pattern(
                CREATE_TASK,
                &json!({ "agent": "Reviewer", "title": "x", "goal": "y" })
            ),
            json!({ "agent": "Reviewer" })
        );
        assert_eq!(
            rule_pattern(
                "create_issue",
                &json!({ "repository": "app", "agent": "x" })
            ),
            json!({ "repository": "app" }),
            "agent is a target of create_task only"
        );
        assert_eq!(
            rule_pattern(CREATE_TASK, &json!({ "title": "x" })),
            json!({})
        );
        assert_eq!(
            rule_pattern(
                crate::FAST_FORWARD_PULL_REQUEST,
                &json!({"repository":"app","number":42})
            ),
            json!({"repository":"app","number":42})
        );
        assert_eq!(
            rule_pattern(
                crate::LABEL_ISSUE,
                &json!({"repository":"app","number":7,"add":["bug"]})
            ),
            json!({"repository":"app"})
        );
        let head = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            rule_pattern(
                crate::CLOSE_PULL_REQUEST,
                &json!({"repository":"app","number":42,"expected_head":head,"delete_branch":true})
            ),
            json!({"repository":"app","number":42,"expected_head":head})
        );
        assert_eq!(
            rule_pattern(
                threadlane_core::forge::CREATE_TAG,
                &json!({"repository":"app","tag":"v1.0.0","commit":head})
            ),
            json!({})
        );
        for tool in [crate::RESOLVE_REVIEW_THREAD, crate::REQUEST_REVIEW] {
            assert_eq!(
                rule_pattern(
                    tool,
                    &json!({"repository":"app","number":42,"expected_head":head,"thread_id":"PRRT_1"})
                ),
                json!({"repository":"app","number":42,"expected_head":head}),
                "{tool}"
            );
        }
    }

    #[test]
    fn read_only_gh_checks_need_no_card_and_writes_still_ask() {
        let call = |args: &[&str]| json!({"repository":"app","args":args,"missing_tool":"x"});
        let decide = |arguments: &Json, tainted: bool, overrides: &Overrides| {
            short(&gate(&GateInput {
                tool: GITHUB_CLI,
                tier: T3,
                arguments,
                mode: ON,
                space_ceiling: AU,
                project_ceiling: None,
                overrides,
                rules: &[],
                tainted,
                requested: true,
                new_work: None,
                own_work: None,
            }))
        };
        let none = Overrides::new();
        for args in [
            &[
                "pr",
                "view",
                "12",
                "--json",
                "headRefOid,mergeable,statusCheckRollup",
            ][..],
            &["pr", "checks", "12", "--required"],
            &["pr", "list", "--state=open", "-L", "5"],
            &["pr", "diff", "12", "--name-only"],
            &["run", "view", "123", "--log-failed"],
            &["run", "list", "--branch", "main", "--limit=3"],
            &["issue", "view", "706", "--comments"],
            &["release", "list"],
            &["workflow", "view", "ci.yml", "--yaml"],
        ] {
            assert_eq!(decide(&call(args), false, &none), V::Default, "{args:?}");
        }
        for args in [
            &["pr", "merge", "12"][..],
            &["pr", "close", "12"],
            &["pr", "comment", "12", "--body", "hi"],
            &["pr", "edit", "12", "--add-label", "bug"],
            &["pr", "checkout", "12"],
            &["issue", "close", "706"],
            &["issue", "edit", "706", "--milestone", "v1"],
            &["run", "rerun", "123"],
            &["run", "cancel", "123"],
            &["run", "download", "123"],
            &["release", "download", "v1"],
            &["workflow", "run", "ci.yml"],
            &["label", "create", "x"],
            &["pr", "view", "12", "--web"],
            &["pr", "view", "12", "--unknown-flag"],
            &["pr", "view", "12", "--jq", ".x | .y"],
            &["pr", "view", "$(touch x)"],
            &["pr", "view", "12;", "true"],
            &["api", "repos/{owner}/{repo}/pulls/12"],
        ] {
            assert_eq!(decide(&call(args), false, &none), V::Ask, "{args:?}");
        }
        let view = call(&["pr", "view", "12"]);
        assert_eq!(
            decide(&view, true, &none),
            V::Ask,
            "a tainted run still asks"
        );
        let stricter: Overrides = [(T3, Cell::Block)].into_iter().collect();
        assert_eq!(
            decide(&view, false, &stricter),
            V::Block,
            "an override decides"
        );
    }

    #[test]
    fn the_default_covers_only_drafts_and_the_task_branch() {
        for (tool, arguments, covered) in [
            ("open_pull_request", json!({"repository":"app"}), true),
            ("publish_branch", json!({"repository":"app"}), true),
            (
                "publish_branch",
                json!({"repository":"app","onto_task":null}),
                true,
            ),
            (
                "publish_branch",
                json!({"repository":"app","onto_task":"T-0001"}),
                false,
            ),
            (
                "fast_forward_pull_request",
                json!({"repository":"app","number":1}),
                false,
            ),
        ] {
            assert_eq!(
                allowed_by_default(tool, &arguments),
                covered,
                "{tool} {arguments}"
            );
        }
        let onto = json!({"repository":"app","onto_task":"T-0001"});
        assert!(
            !own_work_tool("publish_branch", &onto),
            "a push onto another task is not own work"
        );
        let input = GateInput {
            tool: "publish_branch",
            tier: T2,
            arguments: &onto,
            mode: ON,
            space_ceiling: AU,
            project_ceiling: None,
            overrides: &Overrides::new(),
            rules: &[],
            tainted: false,
            requested: true,
            new_work: None,
            own_work: Some(OwnWork { checks_pass: true }),
        };
        let verdict = gate(&input);
        assert_eq!(
            short(&verdict),
            V::AskOffer,
            "a push onto another task asks"
        );
        let tainted = gate(&GateInput {
            mode: AS,
            tainted: true,
            ..input
        });
        assert_eq!(
            short(&tainted),
            V::AskOffer,
            "a tainted push onto another task still asks"
        );
    }

    #[test]
    fn stored_overrides_parse_and_bad_entries_are_left_out() {
        let parsed = overrides(&json!({
            "shared_reversible": "ask",
            "read": "block",
            "nonsense": "ask",
            "local": "maybe",
        }));
        assert_eq!(
            parsed,
            Overrides::from([(T2, Cell::Ask), (T0, Cell::Block)])
        );
        assert!(overrides(&json!({})).is_empty());
    }
}
