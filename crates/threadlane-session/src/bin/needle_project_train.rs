use std::path::{Path, PathBuf};
use std::process;

use threadlane_runtime::needle_training::{
    export_needle_dataset, NeedleDatasetConfig, NeedleTrainingManifest, MANIFEST_FILE, TRAIN_FILE,
};
use threadlane_session::{CodingAgent, CodingAgentOptions, SystemPromptConfig};

const USAGE: &str = "usage: needle-project-train dataset --project <directory> --sessions <directory> --work-dir <directory> [--replace]";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Dataset {
        project: PathBuf,
        sessions: PathBuf,
        work_dir: PathBuf,
        replace: bool,
    },
}

fn parse_args<I, S>(args: I) -> Result<Command, &'static str>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut project = None;
    let mut sessions = None;
    let mut work_dir = None;
    let mut replace = false;
    let mut args = args.into_iter().map(Into::into);

    if args.next().as_deref() != Some("dataset") {
        return Err(USAGE);
    }

    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--project" if project.is_none() => {
                project = Some(PathBuf::from(args.next().ok_or(USAGE)?));
            }
            "--sessions" if sessions.is_none() => {
                sessions = Some(PathBuf::from(args.next().ok_or(USAGE)?));
            }
            "--work-dir" if work_dir.is_none() => {
                work_dir = Some(PathBuf::from(args.next().ok_or(USAGE)?));
            }
            "--replace" if !replace => replace = true,
            _ => return Err(USAGE),
        }
    }

    Ok(Command::Dataset {
        project: project.ok_or(USAGE)?,
        sessions: sessions.ok_or(USAGE)?,
        work_dir: work_dir.ok_or(USAGE)?,
        replace,
    })
}

fn print_help() {
    println!("{USAGE}");
}

fn print_dataset_summary(work_dir: &Path, manifest: &NeedleTrainingManifest) {
    println!("eligible_turns: {}", manifest.eligible_turns);
    println!("train_turns: {}", manifest.train_turns);
    println!("holdout_turns: {}", manifest.holdout_turns);
    println!("train_sessions: {}", manifest.train_sessions.len());
    println!("holdout_sessions: {}", manifest.holdout_sessions.len());
    println!("train_path: {}", work_dir.join(TRAIN_FILE).display());
    println!("manifest_path: {}", work_dir.join(MANIFEST_FILE).display());
}

async fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Dataset {
            project,
            sessions,
            work_dir,
            replace,
        } => {
            let agent = CodingAgent::new(CodingAgentOptions {
                api_key: String::new(),
                account_id: None,
                model: "catalogue-only".into(),
                work_dir: project,
                session_file: None,
                system_prompt: SystemPromptConfig::default(),
                agent_config: None,
                coding_config: None,
            });
            agent.refresh_mcp().await;

            let manifest = export_needle_dataset(
                &NeedleDatasetConfig {
                    sessions_dir: sessions,
                    work_dir: work_dir.clone(),
                    replace,
                },
                &agent.configured_tool_definitions(),
            )?;
            print_dataset_summary(&work_dir, &manifest);
            Ok(())
        }
    }
}

#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--help"] {
        print_help();
        return;
    }

    let command = match parse_args(args) {
        Ok(command) => command,
        Err(_) => {
            eprintln!("error: invalid arguments");
            print_help();
            process::exit(3);
        }
    };

    if let Err(error) = run(command).await {
        eprintln!("error: {error}");
        print_help();
        process::exit(3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dataset_with_project_defaults_and_replace() {
        let command = parse_args([
            "dataset",
            "--project",
            "/tmp/p",
            "--sessions",
            "/tmp/p/.threadlane/sessions",
            "--work-dir",
            "/tmp/p/.threadlane/needle-training",
            "--replace",
        ])
        .unwrap();
        assert_eq!(
            command,
            Command::Dataset {
                project: PathBuf::from("/tmp/p"),
                sessions: PathBuf::from("/tmp/p/.threadlane/sessions"),
                work_dir: PathBuf::from("/tmp/p/.threadlane/needle-training"),
                replace: true,
            }
        );
    }

    #[test]
    fn rejects_unknown_dataset_flags() {
        assert_eq!(parse_args(["dataset", "--upload"]).unwrap_err(), USAGE);
    }
}
