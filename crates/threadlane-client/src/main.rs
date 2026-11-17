//! `threadlane`: the command-line client. See `docs/milestone-1.md` §7.
//!
//! A thin layer over [`ClientCore`]: parse, call the core, print.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use threadlane_client::{ClientCore, ClientDirs, SpaceFeedItem};
use threadlane_core::client::ApiError;
use threadlane_core::model::{Mode, OutboxKind};

/// How long one feed request waits while following.
const FOLLOW_WAIT: Duration = Duration::from_secs(30);

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
    },
    /// Create projects.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
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
    /// Send a message to a project's lead and wait for the reply.
    Send {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The project; the General project if left out.
        #[arg(short, long)]
        project: Option<String>,
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
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// Create an agent.
    Add {
        /// The space (paired hub name); may be left out if only one is paired.
        #[arg(short, long)]
        space: Option<String>,
        /// The agent's name, unique in the space.
        name: String,
        /// Its role, for example `reviewer`.
        #[arg(long)]
        role: String,
        /// Its instructions.
        #[arg(long, default_value = "")]
        instructions: String,
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
    /// List the paired hubs.
    List,
    /// Forget a hub.
    Remove {
        /// The hub's name.
        name: String,
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
    let mut core = ClientCore::load(ClientDirs::from_env()?)?;
    match command {
        Command::Hub { command } => hub(&mut core, command)?,
        Command::Status => return Ok(status(&core).await),
        Command::Projects { space } => {
            let listed = core.projects(space.as_deref()).await?;
            for project in listed.projects {
                let mut flags = Vec::new();
                if project.is_general {
                    flags.push("general");
                }
                if project.paused {
                    flags.push("paused");
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
                    role,
                    instructions,
                },
        } => {
            let agent = core
                .create_agent(space.as_deref(), &name, &role, &instructions)
                .await?;
            println!("created agent {} ({})", agent.name, agent.role);
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
            let sent = core
                .send(space, project.as_deref(), &text.join(" "))
                .await?;
            let label = match &sent.project {
                Some(project) => format!("{}/{project}", sent.space),
                None => sent.space.clone(),
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
        Command::Feed {
            space,
            follow,
            from_start,
        } => {
            let space = space.as_deref();
            if from_start {
                core.rewind_feed(space)?;
            }
            if follow {
                let mut interrupted = std::pin::pin!(tokio::signal::ctrl_c());
                loop {
                    let items = tokio::select! {
                        items = core.poll_feed(space, FOLLOW_WAIT) => items?,
                        _ = &mut interrupted => break,
                    };
                    for item in &items {
                        print_item(item);
                    }
                    core.commit_feed()?;
                }
            } else {
                for item in core.poll_feed(space, Duration::ZERO).await? {
                    print_item(&item);
                }
            }
            core.commit_feed()?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn hub(core: &mut ClientCore, command: HubCommand) -> anyhow::Result<()> {
    match command {
        HubCommand::Add { name, path } => {
            core.pair(&name, path)?;
            if let Some(hub) = core.hubs().iter().find(|hub| hub.name == name) {
                println!("paired {}: {}", hub.name, hub.socket.display());
            }
        }
        HubCommand::List => {
            if core.hubs().is_empty() {
                println!("no hubs paired");
            }
            for hub in core.hubs() {
                println!("{}  {}", hub.name, hub.socket.display());
            }
        }
        HubCommand::Remove { name } => {
            core.unpair(&name)?;
            println!("removed {name}");
        }
    }
    Ok(())
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
