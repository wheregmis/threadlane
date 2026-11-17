//! `threadlane`: the command-line client. See `docs/milestone-1.md` §7.
//!
//! A thin layer over [`ClientCore`]: parse, call the core, print.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{ArgGroup, Parser, Subcommand};
use threadlane_client::{ClientCore, ClientDirs, SpaceFeedItem};
use threadlane_core::client::ApiError;
use threadlane_core::model::{Mode, OutboxKind};
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
    },
    /// Create projects.
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
        Command::Decisions { space } => return decisions(&core, space.as_deref()).await,
        Command::Answer {
            space,
            note,
            decision,
            option,
        } => {
            let answered = core
                .answer(space.as_deref(), &decision, &option, note.as_deref())
                .await?;
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
                    core.acknowledge(&item)?;
                }
            } else {
                for item in core.poll_feed(space, Duration::ZERO).await? {
                    print_item(&item);
                }
            }
            core.commit_feed()?;
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
    }
    Ok(ExitCode::SUCCESS)
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
