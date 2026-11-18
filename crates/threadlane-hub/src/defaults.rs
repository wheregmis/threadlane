//! What every hub has from its first start (docs/milestone-1.md §9 "Defaults").

use threadlane_core::model::{MachineTrust, Mode, NewAgent, NewBinding, NewMachine};
use threadlane_core::store::{MachineStore, OrgStore, StoreResult};
use threadlane_store::HubDb;

/// Role of the default lead agent.
pub const LEAD_ROLE: &str = "lead";

/// Name of the default lead agent.
pub const LEAD_NAME: &str = "Lead";

/// The default lead agent's instructions: its role, in a few sentences.
pub const LEAD_INSTRUCTIONS: &str = "You are the lead of this project. You read what \
arrives for the project, decide what it needs, and talk with the user on behalf of \
the team. Be brief and concrete.";

/// Endpoint id of the hub's own machine row until the transport key replaces
/// it in milestone 4.
pub const HUB_ENDPOINT: &str = "local";

/// Name of the hub's own machine row.
pub const HUB_MACHINE_NAME: &str = "hub";

/// Parallel runs the hub's own runner may take.
const HUB_MAX_PARALLEL: u32 = 2;

/// Makes sure a lead agent exists, every project has a lead binding, and the
/// hub has its own machine row. Idempotent.
pub(crate) async fn ensure_defaults(db: &HubDb) -> StoreResult<()> {
    let agents = db.agents().await?;
    let lead = match agents.into_iter().find(|agent| agent.role == LEAD_ROLE) {
        Some(lead) => lead,
        None => {
            db.create_agent(&NewAgent {
                instructions: LEAD_INSTRUCTIONS.to_owned(),
                ..NewAgent::named(LEAD_NAME, LEAD_ROLE)
            })
            .await?
        }
    };
    for project in db.projects().await? {
        if db.lead_binding(project.id).await?.is_none() {
            db.bind(&NewBinding {
                project_id: project.id,
                agent_id: lead.id,
                mode: Mode::OnRequest,
                is_lead: true,
                max_parallel: 1,
            })
            .await?;
        }
    }
    if !db.machines().await?.iter().any(|machine| machine.is_hub) {
        db.register_machine(&NewMachine {
            name: HUB_MACHINE_NAME.to_owned(),
            endpoint_id: HUB_ENDPOINT.to_owned(),
            is_hub: true,
            trust: MachineTrust::Trusted,
            max_parallel: HUB_MAX_PARALLEL,
        })
        .await?;
    }
    Ok(())
}
