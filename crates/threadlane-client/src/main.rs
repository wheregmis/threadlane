//! `threadlane`: the command-line client. See `docs/milestone-1.md` §7.
//!
//! A thin layer over [`ClientCore`]: parse, call the core, print.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use clap::{ArgGroup, Parser, Subcommand};
use threadlane_client::{ClientCore, ClientDirs, SpaceFeedItem};
use threadlane_core::client::{ApiError, OperationsRequest, OrgRequest};
use threadlane_core::model::{BackendKind, Mode, OutboxKind};
use threadlane_core::model::{Recurrence, Rule, Timer, TimerCreator, TimerStatus, Weekday};
use threadlane_core::time::{Clock as _, SystemClock};

/// Talk to your Threadlane hubs.
#[derive(Debug, Parser)]
#[command(name = "threadlane", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Pair, list, and forget hubs.
    Hub {
        #[command(subcommand)]
        command: HubCommand,
    },
    /// Show each paired hub: space, ceiling, reachable or not.
    Status,
    /// List the projects of a space.
    Projects {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// List archived projects.
        #[arg(long)]
        archived: bool,
    },
    /// Create, pause, archive or delete projects.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Register repositories.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// List the repositories of a project.
    Repos {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project; the General project if left out.
        #[arg(short, long)]
        project: Option<String>,
    },
    /// List the agents of a space.
    Agents {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
    },
    /// Create agents.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// List the agents bound to a project and their modes.
    Bindings {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project; the General project if left out.
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Set an agent's mode in a project, binding it there if it is not yet.
    Mode {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project; the General project if left out.
        #[arg(short, long)]
        project: Option<String>,
        /// The agent's name.
        agent: String,
        /// off, on_request, assisted, or autonomous.
        mode: Mode,
    },
    /// Send a message to a project's lead, or to a task, and wait for the
    /// reply.
    Send {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project; the General project if left out.
        #[arg(short, long)]
        project: Option<String>,
        /// A task (T-0001) to send to instead of the lead. While a decision
        /// is open in it, the message declines it with this as the note.
        #[arg(long, value_name = "TASK", conflicts_with = "project")]
        task: Option<String>,
        /// Return after the acknowledgement instead of waiting for a reply.
        #[arg(long)]
        no_wait: bool,
        /// How long to wait for a reply, in seconds.
        #[arg(long, value_name = "SECS", default_value_t = 120)]
        timeout: u64,
        /// The message; several words are joined with spaces.
        #[arg(required = true, trailing_var_arg = true)]
        text: Vec<String>,
    },
    /// List the decisions waiting on you, in every paired space or in one.
    Decisions {
        /// Only this space (paired hub name).
        #[arg(short, long)]
        space: Option<String>,
    },
    /// Answer a decision with one of its options.
    Answer {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// A note for the agent, for example why you decline.
        #[arg(long)]
        note: Option<String>,
        /// Also allow calls like this one from now on: a standing rule for
        /// this agent, project, tool, and target.
        #[arg(long)]
        always: bool,
        /// The decision, for example D-0012.
        decision: String,
        /// The option, for example approve or decline.
        option: String,
    },
    /// Ask a hub for a new one-time join code, for pairing another client.
    PairCode {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
    },
    /// List the clients a hub has admitted.
    Clients {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
    },
    /// Revoke a client at a hub; its connections close at once.
    Revoke {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The client's name, as `threadlane clients` lists it.
        name: String,
    },
    /// Show new messages, decisions, and notices from a space.
    Feed {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// Keep waiting for new items until interrupted.
        #[arg(long)]
        follow: bool,
        /// Show the whole feed, not only what is new.
        #[arg(long)]
        from_start: bool,
        /// Show items without changing the saved feed position.
        #[arg(long)]
        peek: bool,
    },
    /// List the routines and follow-ups of a project that have not ended.
    Routines {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project; the General project if left out.
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Add, pause, resume, and cancel routines.
    Routine {
        #[command(subcommand)]
        command: RoutineCommand,
    },
    /// Cancel tasks.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Set the highest mode any agent may have in a project.
    Ceiling {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project; the General project if left out.
        #[arg(short, long)]
        project: Option<String>,
        /// Set the space's own ceiling instead, which caps every project's.
        #[arg(long, conflicts_with = "project")]
        space_wide: bool,
        /// off, on_request, assisted, or autonomous; for a project, at most
        /// the space's.
        mode: Mode,
    },
    /// List the standing rules of a project, or of every project.
    Rules {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// Only this project.
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Revoke standing rules.
    Rule {
        #[command(subcommand)]
        command: RuleCommand,
    },
    /// Set how many of an agent's tasks may run at the same time in a
    /// project.
    AtOnce {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project; the General project if left out.
        #[arg(short, long)]
        project: Option<String>,
        /// The agent's name.
        agent: String,
        /// How many at once; 0 for no limit.
        count: u32,
    },
    /// Read or change space delivery pacing.
    Pacing {
        /// The space (paired hub name).
        #[arg(short, long)]
        space: Option<String>,
        /// Maximum non-urgent deliveries per hour.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        rate: Option<u32>,
        /// Daily quiet hours as HH:MM-HH:MM.
        #[arg(long, requires = "zone")]
        quiet: Option<String>,
        /// Named time zone for quiet hours, for example America/Toronto.
        #[arg(long, requires = "quiet")]
        zone: Option<String>,
        /// Minutes to collect low-urgency deliveries into one digest.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        digest: Option<u32>,
        /// Turn off delivery pacing.
        #[arg(long, conflicts_with_all = ["rate", "quiet", "digest"])]
        off: bool,
    },
    /// Interrupt a project's lead or a task and retain its work.
    Stop(LaneArgs),
    /// Resume a stopped lead or task.
    Resume(LaneArgs),
    #[command(flatten)]
    Operations(OperationsCommand),
    /// Set the project's dreaming cadence.
    Dreaming {
        /// off, daily HH:MM, or every Nh.
        #[arg(required = true, num_args = 1..=2)]
        cadence: Vec<String>,
        /// The space (paired hub name).
        #[arg(short, long)]
        space: Option<String>,
        /// The project, or General if left out.
        #[arg(short, long)]
        project: Option<String>,
    },
}

#[derive(Debug, clap::Args)]
struct LaneArgs {
    /// The space (paired hub name).
    #[arg(short, long)]
    space: Option<String>,
    /// The project; the General project if left out.
    #[arg(short, long, conflicts_with = "task")]
    project: Option<String>,
    /// A task ID, such as T-0001.
    #[arg(long)]
    task: Option<String>,
}

impl LaneArgs {
    fn target(&self) -> anyhow::Result<threadlane_core::client::LaneTarget> {
        use threadlane_core::client::LaneTarget;
        Ok(match &self.task {
            Some(task) => LaneTarget::Task {
                task: threadlane_core::ids::TaskId::parse(task)
                    .ok_or_else(|| anyhow::anyhow!("{task:?} is not a task id like T-0001"))?,
            },
            None => LaneTarget::Lead {
                project: self.project.clone(),
            },
        })
    }
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// Cancel a task, and the tasks that wait on it; the lead hears of it.
    Cancel {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The task, for example T-0001.
        task: String,
    },
}

#[derive(Debug, Subcommand)]
enum RuleCommand {
    /// Revoke a standing rule; calls it allowed ask again.
    Revoke {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// Its number, as `threadlane rules` lists it.
        rule: String,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// Create a project; it gets the lead at once.
    Add {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project's name, unique in the space.
        name: String,
        /// What the project is about.
        #[arg(long, default_value = "")]
        description: String,
    },
    /// Pause a project: none of its work starts until it is resumed.
    Pause {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project's name.
        name: String,
    },
    /// Resume a paused project.
    Resume {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project's name.
        name: String,
    },
    /// Archive an idle project, keeping its history and repositories.
    Archive {
        #[arg(short, long)]
        space: Option<String>,
        name: String,
    },
    /// Restore an archived project.
    Unarchive {
        #[arg(short, long)]
        space: Option<String>,
        name: String,
    },
    /// Permanently delete a project and its local data.
    Delete {
        #[arg(long)]
        space: Option<String>,
        name: String,
        /// Skip the prompt by supplying the exact project name.
        #[arg(long, value_name = "NAME")]
        yes: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum RepoCommand {
    /// Register a repository in a project; tasks name it to work in it.
    Add {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project; the General project if left out.
        #[arg(short, long)]
        project: Option<String>,
        /// The default branch tasks start from; `main` if left out.
        #[arg(long)]
        branch: Option<String>,
        /// The repository's name in the project.
        name: String,
        /// Where `git` clones it from; it must be new to the space.
        remote: String,
    },
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// Create an agent, from scratch or from a template.
    Add {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The agent's name, unique in the space; the template's if left out.
        #[arg(required_unless_present = "template")]
        name: Option<String>,
        /// The agent's name, the same as the positional one.
        #[arg(long = "name", conflicts_with = "name")]
        named: Option<String>,
        /// Start from this template (see `agent templates`); the options
        /// below replace its values when given.
        #[arg(long)]
        template: Option<String>,
        /// Its role, for example `reviewer`; the template's id if left out.
        #[arg(long, required_unless_present = "template")]
        role: Option<String>,
        /// Its instructions.
        #[arg(long)]
        instructions: Option<String>,
        /// The backends it may use, in order of preference, for example
        /// claude,codex; either if left out.
        #[arg(long = "backend", value_delimiter = ',')]
        backends: Vec<BackendKind>,
    },
    /// Change an agent; options left out keep their value. It applies from
    /// the agent's next run.
    Edit {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The agent's current name.
        agent: String,
        /// Its new name.
        #[arg(long)]
        name: Option<String>,
        /// Its new role.
        #[arg(long)]
        role: Option<String>,
        /// Its new instructions.
        #[arg(long, conflicts_with = "instructions_file")]
        instructions: Option<String>,
        /// Read its new instructions from this file.
        #[arg(long, value_name = "PATH")]
        instructions_file: Option<PathBuf>,
        /// The backends it may use, in order of preference, for example
        /// claude,codex.
        #[arg(long = "backend", value_delimiter = ',')]
        backends: Option<Vec<String>>,
        /// A model for one backend, as BACKEND=MODEL; an empty model goes
        /// back to the backend's default. May be repeated.
        #[arg(long = "model", value_name = "BACKEND=MODEL", value_parser = backend_model)]
        models: Vec<(String, String)>,
    },
    /// Delete an agent with its bindings and standing rules, after you type
    /// its name. Finished tasks keep its name.
    Delete {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        name: String,
        /// Skip the prompt by supplying the exact agent name.
        #[arg(long, value_name = "NAME")]
        yes: Option<String>,
    },
    /// List the starter agents the hub offers.
    Templates {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum HubCommand {
    /// Pair a hub by its data directory (meaning <dir>/hub.sock) or socket.
    Add {
        /// The name to show its space under.
        name: String,
        /// The hub's data directory or socket file.
        path: PathBuf,
    },
    /// Pair a hub over iroh with a join code it gave.
    Join {
        /// The name to show its space under.
        name: String,
        /// The join code, from the hub's first start or `threadlane pair-code`.
        code: String,
    },
    /// List the paired hubs.
    List,
    /// Forget a hub.
    Remove {
        /// The hub's name.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum RoutineCommand {
    /// Add a routine of yours: the project's lead is woken with the
    /// instruction on a cadence, as work you asked for.
    #[command(group(ArgGroup::new("cadence").required(true)))]
    Add {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project; the General project if left out.
        #[arg(short, long)]
        project: Option<String>,
        /// Every so long: minutes, or with a unit, for example 90m or 2h.
        #[arg(long, group = "cadence", value_name = "INTERVAL", value_parser = parse_every)]
        every: Option<u32>,
        /// Every day at this local time.
        #[arg(long, group = "cadence", value_name = "HH:MM", value_parser = parse_time)]
        daily: Option<(u8, u8)>,
        /// On these days, for example mon,wed,fri, at the time of --at.
        #[arg(long, group = "cadence", value_delimiter = ',', requires = "at")]
        weekdays: Vec<Weekday>,
        /// The local time for --weekdays.
        #[arg(long, value_name = "HH:MM", value_parser = parse_time, requires = "weekdays")]
        at: Option<(u8, u8)>,
        /// The time zone, for example Europe/Berlin; this computer's if left out.
        #[arg(long)]
        zone: Option<String>,
        /// The instruction; several words are joined with spaces.
        #[arg(required = true, trailing_var_arg = true)]
        instruction: Vec<String>,
    },
    /// Pause a routine or follow-up; it fires nothing until resumed.
    Pause {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// Its number, as `threadlane routines` lists it.
        timer: String,
    },
    /// Resume a paused routine at its next occurrence.
    Resume {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// Its number, as `threadlane routines` lists it.
        timer: String,
    },
    /// Cancel a routine or follow-up for good.
    Cancel {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// Its number, as `threadlane routines` lists it.
        timer: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("threadlane: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(command: Command) -> anyhow::Result<ExitCode> {
    // Local discovery finds hubs whose address changed; THREADLANE_LOCAL_DISCOVERY=off turns it off.
    let local_discovery = std::env::var("THREADLANE_LOCAL_DISCOVERY").map_or(true, |value| {
        !matches!(value.as_str(), "off" | "0" | "false")
    });
    let mut core = ClientCore::load(ClientDirs::from_env()?)?.with_local_discovery(local_discovery);
    match command {
        Command::Hub { command } => hub(&mut core, command).await?,
        Command::PairCode { space } => {
            let issued = core.pair_code(space.as_deref()).await?;
            let minutes = issued
                .expires_at
                .saturating_since(SystemClock.now())
                .as_secs()
                .div_ceil(60);
            println!(
                "[{}] join code, for one use within {minutes} minutes:\n{}",
                issued.space, issued.code
            );
        }
        Command::Clients { space } => {
            for client in core.clients(space.as_deref()).await? {
                let revoked = if client.is_active() { "" } else { " (revoked)" };
                println!("{} ({}){revoked}", client.name, client.role);
            }
        }
        Command::Revoke { space, name } => {
            let client = core.revoke(space.as_deref(), &name).await?;
            println!("revoked {}", client.name);
        }
        Command::Status => return Ok(status(&core).await),
        Command::Projects { space, archived } => {
            let listed = core.project_list(space.as_deref(), archived).await?;
            for project in listed
                .projects
                .into_iter()
                .filter(|project| project.archived == archived)
            {
                let mut flags = Vec::new();
                if project.is_general {
                    flags.push("general");
                }
                if project.paused {
                    flags.push("paused");
                }
                if project.archived {
                    flags.push("archived");
                }
                if flags.is_empty() {
                    println!("[{}] {}", listed.space, project.name);
                } else {
                    println!("[{}] {} ({})", listed.space, project.name, flags.join(", "));
                }
            }
        }
        Command::Project {
            command:
                ProjectCommand::Add {
                    space,
                    name,
                    description,
                },
        } => {
            let project = core
                .create_project(space.as_deref(), &name, &description)
                .await?;
            println!("created project {}", project.name);
        }
        Command::Project {
            command: ProjectCommand::Pause { space, name },
        } => {
            let project = core.set_paused(space.as_deref(), Some(&name), true).await?;
            println!("paused project {}", project.name);
        }
        Command::Project {
            command: ProjectCommand::Resume { space, name },
        } => {
            let project = core
                .set_paused(space.as_deref(), Some(&name), false)
                .await?;
            println!("resumed project {}", project.name);
        }
        Command::Project {
            command: ProjectCommand::Archive { space, name },
        } => {
            let project = core.set_archived(space.as_deref(), &name, true).await?;
            println!("archived project {}", project.name);
        }
        Command::Project {
            command: ProjectCommand::Unarchive { space, name },
        } => {
            let project = core.set_archived(space.as_deref(), &name, false).await?;
            println!("unarchived project {}", project.name);
        }
        Command::Project {
            command: ProjectCommand::Delete { space, name, yes },
        } => {
            let preview = core.delete_preview(space.as_deref(), &name).await?;
            println!(
                "Deleting {name:?} permanently removes {} from Threadlane.",
                preview.inventory()
            );
            println!("Branches and pull requests on GitHub are not touched.");
            let confirmed = confirm_deletion(
                "project",
                &name,
                yes,
                io::stdin().lock(),
                io::stdout().lock(),
            )?;
            let (deleted, leftovers) = core
                .delete_project(space.as_deref(), &name, &confirmed)
                .await?;
            println!("Deleted {name:?} and {}.", deleted.inventory());
            for path in leftovers {
                eprintln!("Workspace cleanup still needed for {path}");
            }
        }
        Command::Task {
            command: TaskCommand::Cancel { space, task },
        } => {
            let task = core.cancel_task(space.as_deref(), &task).await?;
            println!("{} is {}", task.id, task.status);
        }
        Command::Repo {
            command:
                RepoCommand::Add {
                    space,
                    project,
                    branch,
                    name,
                    remote,
                },
        } => {
            let repository = core
                .add_repository(
                    space.as_deref(),
                    project.as_deref(),
                    &name,
                    &remote,
                    branch.as_deref(),
                )
                .await?;
            println!(
                "registered repository {} ({}, branch {})",
                repository.name, repository.remote_url, repository.default_branch
            );
        }
        Command::Repos { space, project } => {
            for repository in core
                .repositories(space.as_deref(), project.as_deref())
                .await?
            {
                println!(
                    "{} {} ({})",
                    repository.name, repository.remote_url, repository.default_branch
                );
            }
        }
        Command::Agents { space } => {
            for agent in core.agents(space.as_deref()).await? {
                println!("{} ({})", agent.name, agent.role);
            }
        }
        Command::Agent {
            command:
                AgentCommand::Add {
                    space,
                    name,
                    named,
                    template,
                    role,
                    instructions,
                    mut backends,
                },
        } => {
            let mut name = named.or(name);
            let (mut role, mut instructions) = (role, instructions);
            if let Some(id) = template {
                let templates = core.agent_templates(space.as_deref()).await?;
                let Some(template) = templates.into_iter().find(|template| template.id == id)
                else {
                    anyhow::bail!("no template {id:?}; see `threadlane agent templates`");
                };
                name.get_or_insert(template.name);
                role.get_or_insert(template.id);
                instructions.get_or_insert(template.instructions);
                if backends.is_empty() {
                    backends = template.backends;
                }
            }
            let agent = core
                .create_agent(
                    space.as_deref(),
                    &name.unwrap_or_default(),
                    &role.unwrap_or_default(),
                    &instructions.unwrap_or_default(),
                    &backends,
                )
                .await?;
            println!("created agent {} ({})", agent.name, agent.role);
        }
        Command::Agent {
            command:
                AgentCommand::Edit {
                    space,
                    agent,
                    name,
                    role,
                    instructions,
                    instructions_file,
                    backends,
                    models,
                },
        } => {
            let instructions = match instructions_file {
                Some(path) => Some(
                    std::fs::read_to_string(&path)
                        .with_context(|| format!("reading {}", path.display()))?,
                ),
                None => instructions,
            };
            let request = OrgRequest::UpdateAgent {
                agent,
                name,
                role,
                instructions,
                backends,
                models: (!models.is_empty()).then(|| models.into_iter().collect()),
            };
            let agent = core.update_agent(space.as_deref(), request).await?;
            println!("updated agent {} ({})", agent.name, agent.role);
        }
        Command::Agent {
            command: AgentCommand::Delete { space, name, yes },
        } => {
            let confirmed =
                confirm_deletion("agent", &name, yes, io::stdin().lock(), io::stdout().lock())?;
            let deleted = core
                .delete_agent(space.as_deref(), &name, &confirmed)
                .await?;
            println!("deleted agent {deleted}");
        }
        Command::Agent {
            command: AgentCommand::Templates { space },
        } => {
            for template in core.agent_templates(space.as_deref()).await? {
                println!("{}: {}, {}", template.id, template.name, template.role);
            }
        }
        Command::Bindings { space, project } => {
            for binding in core.bindings(space.as_deref(), project.as_deref()).await? {
                let lead = if binding.is_lead { ", lead" } else { "" };
                println!(
                    "{} ({}{lead}): {}",
                    binding.agent, binding.role, binding.mode
                );
            }
        }
        Command::Mode {
            space,
            project,
            agent,
            mode,
        } => {
            let binding = core
                .set_mode(space.as_deref(), project.as_deref(), &agent, mode)
                .await?;
            println!("{} is now {}", binding.agent, binding.mode);
        }
        Command::Send {
            space,
            project,
            task,
            no_wait,
            timeout,
            text,
        } => {
            let space = space.as_deref();
            if !no_wait {
                // Show what was already waiting first, so an older message is
                // not mistaken for the reply.
                for item in core.poll_feed(space, Duration::ZERO).await? {
                    print_item(&item);
                }
                core.commit_feed()?;
            }
            let sent = match &task {
                Some(task) => core.send_to_task(space, task, &text.join(" ")).await?,
                None => {
                    core.send(space, project.as_deref(), &text.join(" "))
                        .await?
                }
            };
            let label = match (&task, &sent.project) {
                (Some(task), _) => format!("{}/{task}", sent.space),
                (None, Some(project)) => format!("{}/{project}", sent.space),
                (None, None) => sent.space.clone(),
            };
            let duplicate = if sent.accepted.duplicate {
                " (already accepted before)"
            } else {
                ""
            };
            println!(
                "[{label}] accepted as event {}{duplicate}",
                sent.accepted.event_id
            );
            if !no_wait {
                core.wait_for_message(space, Duration::from_secs(timeout), print_item)
                    .await?;
            }
        }
        Command::Decisions { space } => return decisions(&core, space.as_deref()).await,
        Command::Answer {
            space,
            note,
            always,
            decision,
            option,
        } => {
            let answered = if always {
                core.answer_always(space.as_deref(), &decision, &option, note.as_deref())
                    .await?
            } else {
                core.answer(space.as_deref(), &decision, &option, note.as_deref())
                    .await?
            };
            let chosen = answered
                .decision
                .chosen()
                .map_or("?", |option| option.label.as_str());
            if answered.recorded {
                println!(
                    "[{}] {} answered: {chosen}",
                    answered.space, answered.decision.id
                );
            } else {
                println!(
                    "[{}] {} was already answered ({chosen}); this answer was ignored",
                    answered.space, answered.decision.id
                );
            }
        }
        Command::Feed {
            space,
            follow,
            from_start,
            peek,
        } => {
            let space = space.as_deref();
            if from_start {
                core.rewind_feed(space)?;
            }
            if follow {
                // Pushed by the hub over iroh, long-polled over a socket.
                let mut feed = core.follow(space)?;
                let mut interrupted = std::pin::pin!(tokio::signal::ctrl_c());
                loop {
                    let item = tokio::select! {
                        item = feed.recv() => item,
                        _ = &mut interrupted => break,
                    };
                    let Some(item) = item else { break };
                    let item = item?;
                    print_item(&item);
                    if !peek {
                        core.acknowledge(&item)?;
                    }
                }
            } else {
                for item in core.poll_feed(space, Duration::ZERO).await? {
                    print_item(&item);
                }
            }
            if !peek {
                core.commit_feed()?;
            }
        }
        Command::Routines { space, project } => {
            let timers = core.routines(space.as_deref(), project.as_deref()).await?;
            if timers.is_empty() {
                println!("no routines or follow-ups");
            }
            for timer in timers {
                println!("{}", timer_line(&timer));
            }
        }
        Command::Routine { command } => routine(&core, command).await?,
        Command::Ceiling {
            space,
            project,
            space_wide,
            mode,
        } => {
            let ceiling = if space_wide {
                core.set_space_ceiling(space.as_deref(), mode).await?
            } else {
                core.set_ceiling(space.as_deref(), project.as_deref(), mode)
                    .await?
            };
            println!("the ceiling is now {ceiling}");
        }
        Command::Rules { space, project } => {
            let rules = core
                .standing_rules(space.as_deref(), project.as_deref())
                .await?;
            if rules.is_empty() {
                println!("no standing rules");
            }
            for listed in rules {
                let rule = &listed.rule;
                let revoked = if rule.revoked_at.is_some() {
                    " (revoked)"
                } else {
                    ""
                };
                println!(
                    "{} {}/{}: {} {}, used {} times{revoked}",
                    rule.id, listed.project, listed.agent, rule.tool, rule.pattern, rule.uses
                );
            }
        }
        Command::Rule {
            command: RuleCommand::Revoke { space, rule },
        } => {
            let revoked = core.revoke_rule(space.as_deref(), &rule).await?;
            println!("revoked rule {}", revoked.rule.id);
        }
        Command::AtOnce {
            space,
            project,
            agent,
            count,
        } => {
            let binding = core
                .set_max_parallel(space.as_deref(), project.as_deref(), &agent, count)
                .await?;
            match binding.max_parallel {
                0 => println!("{} may run any number of tasks at once", binding.agent),
                n => println!("{} may run {n} at once", binding.agent),
            }
        }
        Command::Pacing {
            space,
            rate,
            quiet,
            zone,
            digest,
            off,
        } => {
            use threadlane_core::model::{Pacing, QuietHours};
            let quiet = quiet
                .zip(zone)
                .map(|(range, zone)| {
                    let (start, end) = range
                        .split_once('-')
                        .ok_or_else(|| anyhow::anyhow!("quiet hours must be HH:MM-HH:MM"))?;
                    Ok::<_, anyhow::Error>(QuietHours {
                        start_minute: quiet_minute(start)?,
                        end_minute: quiet_minute(end)?,
                        time_zone: zone,
                    })
                })
                .transpose()?;
            let space = space.as_deref();
            let mut pacing = if off {
                Pacing::default()
            } else {
                core.pacing(space).await?
            };
            if off || rate.is_some() || quiet.is_some() || digest.is_some() {
                pacing.rate = rate.or(pacing.rate);
                pacing.quiet = quiet.or(pacing.quiet);
                pacing.digest_minutes = digest.or(pacing.digest_minutes);
                pacing = core.set_pacing(space, pacing).await?;
            }
            println!(
                "Rate: {}",
                pacing
                    .rate
                    .map_or_else(|| "off".to_owned(), |rate| format!("{rate}/hour"))
            );
            println!(
                "Quiet hours: {}",
                pacing.quiet.map_or_else(
                    || "off".to_owned(),
                    |quiet| {
                        format!(
                            "{}-{} ({})",
                            local_time(quiet.start_minute),
                            local_time(quiet.end_minute),
                            quiet.time_zone
                        )
                    }
                )
            );
            println!(
                "Digest: {}",
                pacing
                    .digest_minutes
                    .map_or_else(|| "off".to_owned(), |minutes| format!("{minutes} minutes"))
            );
        }
        Command::Stop(args) => {
            core.stop(args.space.as_deref(), args.target()?).await?;
            println!("Stop requested.");
        }
        Command::Resume(args) => {
            core.resume_lane(args.space.as_deref(), args.target()?)
                .await?;
            println!("Lane resumed.");
        }
        Command::Operations(command) => {
            let (space, request) = operations_request(command);
            let reply = core.operations(space.as_deref(), request).await?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
        }
        Command::Dreaming {
            cadence,
            space,
            project,
        } => {
            let request = dreaming_request(&cadence, project, local_zone)?;
            let reply = core.memory(space.as_deref(), request).await?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn quiet_minute(value: &str) -> anyhow::Result<u16> {
    let [h1, h2, b':', m1, m2] = value.as_bytes() else {
        anyhow::bail!("{value:?} must be a time in HH:MM");
    };
    anyhow::ensure!(
        [h1, h2, m1, m2].into_iter().all(u8::is_ascii_digit),
        "{value:?} must be a time in HH:MM"
    );
    let (hour, minute) = parse_time(value).map_err(anyhow::Error::msg)?;
    Ok(u16::from(hour) * 60 + u16::from(minute))
}

fn local_time(minute: u16) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

async fn routine(core: &ClientCore, command: RoutineCommand) -> anyhow::Result<()> {
    match command {
        RoutineCommand::Add {
            space,
            project,
            every,
            daily,
            weekdays,
            at,
            zone,
            instruction,
        } => {
            let rule = match (every, daily, at) {
                (Some(minutes), _, _) => Rule::Interval { minutes },
                (_, Some((hour, minute)), _) => Rule::Daily { hour, minute },
                (_, _, Some((hour, minute))) => Rule::Weekdays {
                    days: weekdays,
                    hour,
                    minute,
                },
                _ => anyhow::bail!("give --every, --daily, or --weekdays with --at"),
            };
            let zone = match zone {
                Some(zone) => zone,
                None => local_zone().ok_or_else(|| {
                    anyhow::anyhow!("this computer's time zone is unknown; give --zone")
                })?,
            };
            let timer = core
                .add_routine(
                    space.as_deref(),
                    project.as_deref(),
                    &instruction.join(" "),
                    Recurrence { rule, zone },
                )
                .await?;
            println!("created routine {}", timer_line(&timer));
        }
        RoutineCommand::Pause { space, timer } => {
            let timer = core.pause_timer(space.as_deref(), &timer).await?;
            println!("paused {}", timer_line(&timer));
        }
        RoutineCommand::Resume { space, timer } => {
            let timer = core.resume_timer(space.as_deref(), &timer).await?;
            println!("resumed {}", timer_line(&timer));
        }
        RoutineCommand::Cancel { space, timer } => {
            let timer = core.cancel_timer(space.as_deref(), &timer).await?;
            println!("cancelled {}", timer.id);
        }
    }
    Ok(())
}

/// A timer on one line: its number, cadence, when next, and instruction.
fn timer_line(timer: &Timer) -> String {
    let what = timer
        .recurrence
        .as_ref()
        .map_or_else(|| "follow-up".to_owned(), ToString::to_string);
    let wait = timer.due_at.saturating_since(SystemClock.now()).as_secs() / 60;
    let next = match (wait / (24 * 60), wait / 60 % 24, wait % 60) {
        (0, 0, 0) => "due now".to_owned(),
        (0, 0, minutes) => format!("next in {minutes}m"),
        (0, hours, minutes) => format!("next in {hours}h {minutes}m"),
        (days, hours, _) => format!("next in {days}d {hours}h"),
    };
    let paused = if timer.status == TimerStatus::Paused {
        " (paused)"
    } else {
        ""
    };
    let by = match timer.created_by {
        TimerCreator::User => String::new(),
        other => format!(", set by {other}"),
    };
    format!(
        "{} {what}, {next}{paused}{by}: {}",
        timer.id, timer.instruction
    )
}

/// `HH:MM`, a local time of day.
fn parse_time(text: &str) -> Result<(u8, u8), String> {
    let parsed = text.split_once(':').and_then(|(hour, minute)| {
        let hour: u8 = hour.parse().ok()?;
        let minute: u8 = minute.parse().ok()?;
        (hour < 24 && minute < 60).then_some((hour, minute))
    });
    parsed.ok_or_else(|| format!("{text:?} is not a time like 08:30"))
}

/// An interval in minutes: `90`, `90m`, or `2h`.
fn parse_every(text: &str) -> Result<u32, String> {
    let text = text.trim();
    let (number, unit) = match text.strip_suffix('h') {
        Some(hours) => (hours, 60),
        None => (text.strip_suffix('m').unwrap_or(text), 1),
    };
    number
        .parse::<u32>()
        .ok()
        .and_then(|number| number.checked_mul(unit))
        .filter(|minutes| *minutes > 0)
        .ok_or_else(|| format!("{text:?} is not an interval like 90m or 2h"))
}

/// This computer's time zone name: `TZ`, else where `/etc/localtime` points.
fn local_zone() -> Option<String> {
    if let Ok(zone) = std::env::var("TZ")
        && !zone.trim().is_empty()
    {
        return Some(zone.trim().trim_start_matches(':').to_owned());
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let target = target.to_str()?;
    let (_, zone) = target.split_once("zoneinfo/")?;
    Some(zone.to_owned())
}

async fn hub(core: &mut ClientCore, command: HubCommand) -> anyhow::Result<()> {
    match command {
        HubCommand::Add { name, path } => {
            core.pair(&name, path)?;
            if let Some(hub) = core.hubs().iter().find(|hub| hub.name == name) {
                println!("paired {}: {}", hub.name, hub.reach);
            }
        }
        HubCommand::Join { name, code } => {
            core.join(&name, &code).await?;
            if let Some(hub) = core.hubs().iter().find(|hub| hub.name == name) {
                println!("paired {}: {}", hub.name, hub.reach);
            }
        }
        HubCommand::List => {
            if core.hubs().is_empty() {
                println!("no hubs paired");
            }
            for hub in core.hubs() {
                println!("{}  {}", hub.name, hub.reach);
            }
        }
        HubCommand::Remove { name } => {
            core.unpair(&name)?;
            println!("removed {name}");
        }
    }
    Ok(())
}

/// Prints each pending decision with its space; fails if a hub could not
/// list its decisions.
async fn decisions(core: &ClientCore, space: Option<&str>) -> anyhow::Result<ExitCode> {
    let mut code = ExitCode::SUCCESS;
    let mut none = true;
    for listed in core.decisions(space).await? {
        let decisions = match listed.decisions {
            Ok(decisions) => decisions,
            Err(error) => {
                code = ExitCode::FAILURE;
                println!("[{}] could not list decisions: {error}", listed.space);
                continue;
            }
        };
        for decision in decisions {
            none = false;
            let options: Vec<String> = decision
                .options
                .iter()
                .enumerate()
                .map(|(index, option)| {
                    if decision.recommended == u32::try_from(index).ok() {
                        format!("{} (recommended)", option.label)
                    } else {
                        option.label.clone()
                    }
                })
                .collect();
            println!("[{}] {} {}", listed.space, decision.id, decision.question);
            println!("    options: {}", options.join(", "));
        }
    }
    if none && code == ExitCode::SUCCESS {
        println!("nothing waits on you");
    }
    Ok(code)
}

/// Prints one line per hub; fails if any hub is not reachable.
async fn status(core: &ClientCore) -> ExitCode {
    let statuses = core.status().await;
    if statuses.is_empty() {
        println!("no hubs paired");
    }
    let mut all_reachable = true;
    for status in statuses {
        match status.info {
            Ok(info) => println!(
                "{}: reachable, space {}, ceiling {}",
                status.name, info.space, info.mode_ceiling
            ),
            Err(ApiError::Unreachable(reason)) => {
                all_reachable = false;
                println!("{}: unreachable ({reason})", status.name);
            }
            Err(error) => {
                all_reachable = false;
                println!("{}: error ({error})", status.name);
            }
        }
    }
    if all_reachable {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn print_item(item: &SpaceFeedItem) {
    match item.kind() {
        OutboxKind::Message => println!("[{}] {}", item.label(), item.text()),
        OutboxKind::Decision => println!("[{}] decision: {}", item.label(), item.text()),
        OutboxKind::SystemNotice => println!("[{}] notice: {}", item.space, item.text()),
    }
}

#[derive(Debug, Subcommand)]
enum OperationsCommand {
    /// Show operational metrics.
    Metrics {
        /// The space (paired hub name).
        #[arg(short, long)]
        space: Option<String>,
        /// Include only this project's records.
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Inspect a project's lead lane or a task lane.
    Lane {
        /// Project name or task ID such as T-0001.
        target: String,
        /// The space (paired hub name).
        #[arg(short, long)]
        space: Option<String>,
    },
    /// Return a dead event to its lane once.
    Replay {
        /// Numeric event ID.
        #[arg(value_parser = clap::value_parser!(i64).range(1..))]
        event: i64,
        /// The space (paired hub name).
        #[arg(short, long)]
        space: Option<String>,
    },
    /// Work with versioned memory.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Inspect the records carrying a trace ID.
    Trace {
        id: String,
        /// The space (paired hub name).
        #[arg(short, long)]
        space: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    /// Restore an earlier version as a new version by you.
    Rollback {
        path: String,
        #[arg(value_parser = clap::value_parser!(i64).range(1..))]
        version: i64,
        /// The space (paired hub name).
        #[arg(short, long)]
        space: Option<String>,
        /// The project, or General if left out.
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Find duplicate notes and stale references.
    Check {
        /// The space (paired hub name).
        #[arg(short, long)]
        space: Option<String>,
        /// The project, or General if left out.
        #[arg(short, long)]
        project: Option<String>,
    },
}

fn operations_request(command: OperationsCommand) -> (Option<String>, OperationsRequest) {
    use threadlane_core::client::LaneTarget;
    use threadlane_core::ids::{EventId, TaskId};

    match command {
        OperationsCommand::Metrics { space, project } => {
            (space, OperationsRequest::Metrics { project })
        }
        OperationsCommand::Lane { target, space } => {
            let task = target
                .get(..2)
                .filter(|prefix| prefix.eq_ignore_ascii_case("t-"))
                .and_then(|_| TaskId::parse(&target));
            let target = match task {
                Some(task) => LaneTarget::Task { task },
                None => LaneTarget::Lead {
                    project: Some(target),
                },
            };
            (space, OperationsRequest::Lane { target })
        }
        OperationsCommand::Replay { event, space } => (
            space,
            OperationsRequest::Replay {
                event: EventId(event),
            },
        ),
        OperationsCommand::Memory {
            command:
                MemoryCommand::Rollback {
                    path,
                    version,
                    space,
                    project,
                },
        } => (
            space,
            OperationsRequest::Rollback {
                project,
                path,
                version,
            },
        ),
        OperationsCommand::Trace { id, space } => (space, OperationsRequest::Trace { id }),
        OperationsCommand::Memory {
            command: MemoryCommand::Check { space, project },
        } => (space, OperationsRequest::CheckMemory { project }),
    }
}

#[cfg(test)]
mod operations_tests {
    use threadlane_core::client::LaneTarget;
    use threadlane_core::ids::{EventId, TaskId};

    use super::*;

    #[test]
    fn cli_parses_all_operations_without_opening_a_client() {
        use OperationsRequest::{Lane, Metrics, Replay, Rollback, Trace};
        for (args, expected_space, expected) in [
            (
                &["metrics", "-p", "Build", "-s", "work"][..],
                Some("work"),
                Metrics {
                    project: Some("Build".to_owned()),
                },
            ),
            (&["metrics"][..], None, Metrics { project: None }),
            (
                &["lane", "Build"][..],
                None,
                Lane {
                    target: LaneTarget::Lead {
                        project: Some("Build".to_owned()),
                    },
                },
            ),
            (
                &["lane", "t-0042"][..],
                None,
                Lane {
                    target: LaneTarget::Task { task: TaskId(42) },
                },
            ),
            (&["replay", "17"][..], None, Replay { event: EventId(17) }),
            (
                &[
                    "memory",
                    "rollback",
                    "project/notes/a.md",
                    "2",
                    "-p",
                    "Build",
                ][..],
                None,
                Rollback {
                    project: Some("Build".to_owned()),
                    path: "project/notes/a.md".to_owned(),
                    version: 2,
                },
            ),
            (
                &["trace", "event-17"][..],
                None,
                Trace {
                    id: "event-17".to_owned(),
                },
            ),
        ] {
            let parsed =
                Cli::try_parse_from(std::iter::once("threadlane").chain(args.iter().copied()))
                    .unwrap();
            let Command::Operations(command) = parsed.command else {
                panic!("expected operations command for {args:?}");
            };
            let (space, request) = operations_request(command);
            assert_eq!(space.as_deref(), expected_space, "{args:?}");
            assert_eq!(request, expected, "{args:?}");
        }
        assert!(Cli::try_parse_from(["threadlane", "replay", "0"]).is_err());
        assert!(Cli::try_parse_from(["threadlane", "memory", "rollback", "a", "0"]).is_err());
    }
}

fn dreaming_request(
    args: &[String],
    project: Option<String>,
    zone: impl FnOnce() -> Option<String>,
) -> anyhow::Result<threadlane_core::client::MemoryRequest> {
    use threadlane_core::client::MemoryRequest;

    let cadence = threadlane_client::parse_dreaming_cadence(
        &args.join(" "),
        &zone().unwrap_or_else(|| "UTC".to_owned()),
    )
    .map_err(anyhow::Error::msg)?;
    Ok(MemoryRequest::SetDreaming { project, cadence })
}

#[cfg(test)]
mod dreaming_cli_tests {
    use super::*;
    use threadlane_core::client::MemoryRequest;

    #[test]
    fn parses_dreaming_and_memory_check_without_contacting_a_hub() {
        for (words, expected) in [
            (&["off"][..], None),
            (
                &["daily", "07:30"][..],
                Some(Recurrence {
                    rule: Rule::Daily {
                        hour: 7,
                        minute: 30,
                    },
                    zone: "America/Toronto".into(),
                }),
            ),
            (
                &["every", "8h"][..],
                Some(Recurrence {
                    rule: Rule::Interval { minutes: 480 },
                    zone: "UTC".into(),
                }),
            ),
        ] {
            let parsed = Cli::try_parse_from(
                ["threadlane", "dreaming"]
                    .into_iter()
                    .chain(words.iter().copied())
                    .chain(["-p", "Build"]),
            )
            .unwrap();
            let Command::Dreaming {
                cadence, project, ..
            } = parsed.command
            else {
                panic!("wrong command")
            };
            assert_eq!(
                dreaming_request(&cadence, project, || Some("America/Toronto".into())).unwrap(),
                MemoryRequest::SetDreaming {
                    project: Some("Build".into()),
                    cadence: expected
                }
            );
        }
        for bad in [
            ["daily", "24:00"],
            ["daily", "7:20"],
            ["every", "0h"],
            ["every", "20m"],
            ["every", "4294967295h"],
        ] {
            assert!(dreaming_request(&bad.map(str::to_owned), None, || None).is_err());
        }
        let parsed =
            Cli::try_parse_from(["threadlane", "memory", "check", "-p", "Build", "-s", "work"])
                .unwrap();
        let Command::Operations(command) = parsed.command else {
            panic!("wrong command")
        };
        assert_eq!(
            operations_request(command),
            (
                Some("work".into()),
                OperationsRequest::CheckMemory {
                    project: Some("Build".into())
                }
            )
        );
    }
}

#[cfg(test)]
mod archive_cli_tests {
    use super::*;

    #[test]
    fn cli_parses_project_archive_unarchive_and_archived_listing() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(["threadlane"].into_iter().chain(args.iter().copied()))
                .unwrap()
                .command
        };
        assert!(matches!(
            parse(&["project", "archive", "API"]),
            Command::Project { command: ProjectCommand::Archive { name, space: None } }
                if name == "API"
        ));
        assert!(matches!(
            parse(&["project", "unarchive", "API", "-s", "work"]),
            Command::Project { command: ProjectCommand::Unarchive { name, space: Some(space) } }
                if name == "API" && space == "work"
        ));
        assert!(matches!(
            parse(&["projects", "--archived"]),
            Command::Projects { archived: true, .. }
        ));
        assert!(matches!(
            parse(&["projects"]),
            Command::Projects {
                archived: false,
                ..
            }
        ));
    }
}

/// `BACKEND=MODEL` from `--model`.
fn backend_model(value: &str) -> Result<(String, String), String> {
    value
        .split_once('=')
        .map(|(backend, model)| (backend.trim().to_owned(), model.trim().to_owned()))
        .ok_or_else(|| format!("{value:?} is not BACKEND=MODEL, for example claude=opus"))
}

fn confirm_deletion(
    kind: &str,
    name: &str,
    supplied: Option<String>,
    mut input: impl BufRead,
    mut output: impl Write,
) -> anyhow::Result<String> {
    let confirmed = match supplied {
        Some(typed) => typed,
        None => {
            write!(output, "Type {name:?} to confirm deletion: ")?;
            output.flush()?;
            let mut typed = String::new();
            input.read_line(&mut typed)?;
            if typed.ends_with('\n') {
                typed.pop();
                if typed.ends_with('\r') {
                    typed.pop();
                }
            }
            typed
        }
    };
    anyhow::ensure!(
        confirmed == name,
        "the confirmation must be the exact {kind} name {name:?}"
    );
    Ok(confirmed)
}

#[cfg(test)]
mod deletion_cli_tests {
    use super::*;

    #[test]
    fn deletion_prompt_accepts_only_the_exact_name() {
        let mut output = Vec::new();
        assert_eq!(
            confirm_deletion("project", "My API", None, &b"My API\r\n"[..], &mut output).unwrap(),
            "My API"
        );
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("Type \"My API\"")
        );
        for wrong in [b"My API \n".as_slice(), b"my API\n", b""] {
            assert!(confirm_deletion("project", "My API", None, wrong, io::sink()).is_err());
        }
    }

    #[test]
    fn deletion_yes_requires_a_matching_value_and_skips_the_prompt() {
        let parse = |args| Cli::try_parse_from(args);
        let parsed = parse([
            "threadlane",
            "project",
            "delete",
            "My API",
            "--yes",
            "My API",
        ])
        .unwrap();
        let Command::Project {
            command: ProjectCommand::Delete { yes, name, .. },
        } = parsed.command
        else {
            panic!("expected delete");
        };
        let mut output = Vec::new();
        assert_eq!(
            confirm_deletion("project", &name, yes, &b""[..], &mut output).unwrap(),
            name
        );
        assert!(output.is_empty());
        assert!(
            confirm_deletion("project", &name, Some("Other".into()), &b""[..], io::sink()).is_err()
        );
        assert!(
            Cli::try_parse_from(["threadlane", "project", "delete", "My API", "--yes"]).is_err()
        );
        assert!(Cli::try_parse_from(["threadlane", "project", "delete", "My API", "-y"]).is_err());
    }
}
