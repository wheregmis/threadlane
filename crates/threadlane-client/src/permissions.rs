//! Ceilings, overrides, and standing rules through [`ClientCore`]
//! (docs/milestone-9.md §2).

use std::collections::BTreeMap;

use threadlane_core::client::{
    ApiError, BindingSummary, DecisionReply, DecisionRequest, OrgReply, OrgRequest,
    StandingRuleSummary,
};
use threadlane_core::ids::StandingRuleId;
use threadlane_core::model::{Cell, Mode};
use threadlane_core::tools::Tier;

use crate::{ClientCore, ClientError, SpaceAnswer, api_error, wrong_reply};

impl ClientCore {
    /// Sets the shared run slots for one machine in a space.
    pub async fn set_machine_slots(
        &self,
        space: Option<&str>,
        machine: threadlane_core::ids::MachineId,
        slots: u32,
    ) -> Result<(), ClientError> {
        match self
            .org(space, OrgRequest::SetMachineSlots { machine, slots })
            .await?
        {
            (_, OrgReply::MachineSlotsSet) => Ok(()),
            (space, _) => Err(wrong_reply(&space, "set machine slots")),
        }
    }

    /// Sets a project's ceiling, and returns what it now is. `space: None`
    /// means the only paired hub; `project: None` the General project.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error, for
    /// example a ceiling above the space's.
    pub async fn set_ceiling(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        mode: Mode,
    ) -> Result<Mode, ClientError> {
        let request = OrgRequest::SetCeiling {
            project: project.map(str::to_owned),
            mode,
        };
        match self.org(space, request).await? {
            (_, OrgReply::Ceiling { ceiling, .. }) => Ok(ceiling),
            (space, _) => Err(wrong_reply(&space, "set ceiling")),
        }
    }

    /// Sets, or with `None` clears, the cell of one tier for an agent's
    /// binding, and returns its overrides.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error, for
    /// example a cell looser than the ceiling allows.
    pub async fn set_override(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        agent: &str,
        tier: Tier,
        cell: Option<Cell>,
    ) -> Result<BTreeMap<Tier, Cell>, ClientError> {
        let request = OrgRequest::SetOverride {
            project: project.map(str::to_owned),
            agent: agent.to_owned(),
            tier,
            cell,
        };
        match self.org(space, request).await? {
            (_, OrgReply::Overrides { overrides, .. }) => Ok(overrides),
            (space, _) => Err(wrong_reply(&space, "set override")),
        }
    }

    /// Answers a decision as [`ClientCore::answer`] does, and makes a
    /// standing rule from it: "always allow this here".
    ///
    /// # Errors
    ///
    /// As [`ClientCore::answer`], and the hub's refusal of a decision that
    /// offers no rule.
    pub async fn answer_always(
        &self,
        space: Option<&str>,
        decision: &str,
        option: &str,
        note: Option<&str>,
    ) -> Result<SpaceAnswer, ClientError> {
        self.answer_as(space, decision, option, note, true).await
    }

    /// The standing rules of a project, or of every project when `None`.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn standing_rules(
        &self,
        space: Option<&str>,
        project: Option<&str>,
    ) -> Result<Vec<StandingRuleSummary>, ClientError> {
        let request = DecisionRequest::StandingRules {
            project: project.map(str::to_owned),
        };
        match self.decision_request(space, request).await? {
            (_, DecisionReply::StandingRules { rules }) => Ok(rules),
            (space, _) => Err(wrong_reply(&space, "standing rules")),
        }
    }

    /// Revokes standing rule `rule` (its number, as `threadlane rules`
    /// lists it).
    ///
    /// # Errors
    ///
    /// No such hub or rule, several hubs and none named, or the hub's error.
    pub async fn revoke_rule(
        &self,
        space: Option<&str>,
        rule: &str,
    ) -> Result<StandingRuleSummary, ClientError> {
        let id = rule
            .trim()
            .parse()
            .ok()
            .filter(|id| *id > 0)
            .map(StandingRuleId);
        let Some(id) = id else {
            let (space, _) = self.target(space)?;
            return Err(api_error(
                &space,
                ApiError::Invalid(format!("{rule:?} is not a rule number")),
            ));
        };
        match self
            .decision_request(space, DecisionRequest::RevokeRule { rule: id })
            .await?
        {
            (_, DecisionReply::StandingRule { rule }) => Ok(rule),
            (space, _) => Err(wrong_reply(&space, "revoke rule")),
        }
    }

    /// Sets how many of an agent's tasks may run at once in a project (0
    /// for no limit), and returns the binding.
    ///
    /// # Errors
    ///
    /// No such hub, project or binding, several hubs and none named, or the
    /// hub's error.
    pub async fn set_max_parallel(
        &self,
        space: Option<&str>,
        project: Option<&str>,
        agent: &str,
        max_parallel: u32,
    ) -> Result<BindingSummary, ClientError> {
        let request = OrgRequest::SetMaxParallel {
            project: project.map(str::to_owned),
            agent: agent.to_owned(),
            max_parallel,
        };
        match self.org(space, request).await? {
            (_, OrgReply::Binding { binding }) => Ok(binding),
            (space, _) => Err(wrong_reply(&space, "set max parallel")),
        }
    }

    /// Sets the space's own ceiling, and returns it.
    ///
    /// # Errors
    ///
    /// No such hub, several hubs and none named, or the hub's error.
    pub async fn set_space_ceiling(
        &self,
        space: Option<&str>,
        mode: Mode,
    ) -> Result<Mode, ClientError> {
        match self
            .org(space, OrgRequest::SetSpaceCeiling { mode })
            .await?
        {
            (_, OrgReply::SpaceCeiling { ceiling }) => Ok(ceiling),
            (space, _) => Err(wrong_reply(&space, "set space ceiling")),
        }
    }

    async fn decision_request(
        &self,
        space: Option<&str>,
        request: DecisionRequest,
    ) -> Result<(String, DecisionReply), ClientError> {
        let (space, api) = self.target(space)?;
        let reply = api
            .decisions(request)
            .await
            .map_err(|error| api_error(&space, error))?;
        Ok((space, reply))
    }
}
