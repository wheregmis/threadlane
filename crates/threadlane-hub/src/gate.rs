//! The permission gate as one pure function (Plan.md §2.3, §9.1,
//! docs/milestone-9.md decision 1): a tool call and what governs it in, one
//! [`Verdict`] out. The gateway gathers the inputs and calls it for every
//! call.

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json};
use threadlane_core::model::{Cell, Mode, StandingRule};
use threadlane_core::tools::{Tier, Verdict};

use crate::tools::ASK_USER;

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
    /// The agent's standing rules in the project, revoked ones included.
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
}

/// The cell of `row` in the column of `mode` (Plan.md §2.3, the matrix).
fn cell(row: Row, mode: Mode) -> Cell {
    match (row, mode) {
        (_, Mode::Off) | (Row::Tier(Tier::Forbidden), _) => Cell::Block,
        (Row::Tier(Tier::Read | Tier::Local), _) => Cell::Allow,
        (Row::Tier(Tier::SharedReversible), Mode::OnRequest) => Cell::Ask,
        (Row::Tier(Tier::SharedReversible), _) => Cell::Review,
        (Row::Initiative, Mode::Autonomous) => Cell::Allow,
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
    let mut targets: Map<String, Json> = TARGET_FIELDS
        .iter()
        .filter_map(|field| {
            let value = arguments.get(*field)?;
            value
                .is_string()
                .then(|| ((*field).to_owned(), value.clone()))
        })
        .collect();
    if tool == crate::FAST_FORWARD_PULL_REQUEST
        && let Some(number) = arguments["number"].as_u64()
    {
        targets.insert("number".into(), number.into());
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

/// Decides one call, first rule that applies wins (Plan.md §9.1):
///
/// 1. the forbidden tier blocks;
/// 2. the effective mode is the binding's, capped by the ceilings, one step
///    lower if tainted; Off blocks, and `ask_user` always asks;
/// 3. the binding's override for the tier makes the cell stricter, or looser
///    up to the ceiling's column (stepped down too if tainted); a standing
///    rule matching the call then allows a tier 0 to 2 call in any mode, and
///    has a tier 3 call reviewed in Autonomous;
/// 4. otherwise the matrix's cell for the row and the effective mode.
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
        Row::Tier(Tier::SharedReversible | Tier::SharedHardToUndo) | Row::Instructions => {
            pattern.as_object().is_some_and(|p| !p.is_empty())
        }
        _ => false,
    };
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
            Some(_) if mode == Mode::Autonomous => verdict = Cell::Review,
            _ => {}
        }
    }
    // A tier 3 rule applies only where the agent acts in Autonomous.
    let offers_rule = ruled && (input.tier < Tier::SharedHardToUndo || mode == Mode::Autonomous);
    match verdict {
        Cell::Allow => Verdict::Allow { rule: None },
        Cell::Review => Verdict::Review { offers_rule },
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
        Review,
        Ask,
        AskOffer,
        Block,
    }

    fn short(verdict: &Verdict) -> V {
        match verdict {
            Verdict::Allow { rule: None } => V::Allow,
            Verdict::Allow { rule: Some(_) } => V::Rule,
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
        use V::{Allow, Ask, AskOffer, Block, Review, Rule};
        let tainted = |c: Case| Case { tainted: true, ..c };
        let proposal = |c: Case, assignee: Mode| Case {
            tool: CREATE_TASK,
            requested: false,
            new_work: Some(assignee),
            ..c
        };
        let ruled = |c: Case| Case {
            tool: "open_pull_request",
            rule: Some("app"),
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
                tool: "publish_branch",
                rule: Some("app"),
                ..case("a rule for another tool", T2, ON, AskOffer)
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
            proposal(case("a proposal in assisted", T1, AS, Ask), AS),
            proposal(case("a task in autonomous", T1, AU, Allow), AU),
            proposal(case("an on request assignee is asked for", T1, AU, Ask), ON),
            proposal(case("an assisted assignee is asked for", T1, AU, Ask), AS),
            proposal(
                case("an autonomous assignee under assisted", T1, AS, Ask),
                AU,
            ),
            proposal(case("an off assignee blocks", T1, AU, Block), Mode::Off),
            tainted(proposal(
                case("tainted autonomous lead proposes", T1, AU, Ask),
                AU,
            )),
            Case {
                project: Some(AS),
                ..proposal(case("a ceiling caps both agents", T1, AU, Ask), AU)
            },
            Case {
                overrides: &[(T1, Cell::Allow)],
                ..proposal(
                    case("an override cannot loosen a proposal", T1, AS, Ask),
                    AS,
                )
            },
            Case {
                overrides: &[(T1, Cell::Ask)],
                ..proposal(case("an override makes a task ask", T1, AU, Ask), AU)
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
        let mut failures = Vec::new();
        for row in &table {
            let arguments = if row.tool == MEMORY_EDIT {
                json!({ "path": "space/instructions.md", "old_text": "a", "new_text": "b" })
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
                Some("revoked") => vec![rule(
                    "open_pull_request",
                    json!({ "repository": "app" }),
                    true,
                )],
                Some(name) => vec![rule(
                    "open_pull_request",
                    json!({ "repository": name }),
                    false,
                )],
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
            });
            if short(&got) != row.expect {
                failures.push(format!("{}: {got:?}, expected {:?}", row.what, row.expect));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
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
                crate::FAST_FORWARD_PULL_REQUEST,
                &json!({"repository":"app","number":42})
            ),
            json!({"repository":"app","number":42})
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
