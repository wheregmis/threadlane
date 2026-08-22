use std::path::{Path, PathBuf};
use std::process;

use threadlane_runtime::needle_training::{
    ADAPTER_FILE, CANDIDATE_FILE, MANIFEST_FILE, NeedleDatasetConfig, NeedleTrainingManifest,
    TRAIN_FILE, export_needle_dataset, run_needle_finetune,
};
use threadlane_session::{CodingAgent, CodingAgentOptions, SystemPromptConfig};

const USAGE: &str = "usage: needle-project-train dataset --project <directory> --sessions <directory> --work-dir <directory> [--replace]\n       needle-project-train finetune --work-dir <directory> [--needle <path>]";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Dataset {
        project: PathBuf,
        sessions: PathBuf,
        work_dir: PathBuf,
        replace: bool,
    },
    Finetune {
        work_dir: PathBuf,
        needle: PathBuf,
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

    let command = args.next().ok_or(USAGE)?;

    match command.as_str() {
        "dataset" => {
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
        "finetune" => {
            let mut needle = None;
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--work-dir" if work_dir.is_none() => {
                        work_dir = Some(PathBuf::from(args.next().ok_or(USAGE)?));
                    }
                    "--needle" if needle.is_none() => {
                        needle = Some(PathBuf::from(args.next().ok_or(USAGE)?));
                    }
                    _ => return Err(USAGE),
                }
            }

            Ok(Command::Finetune {
                work_dir: work_dir.ok_or(USAGE)?,
                needle: needle.unwrap_or_else(|| PathBuf::from("needle")),
            })
        }
        _ => Err(USAGE),
    }
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

fn print_finetune_summary(work_dir: &Path, manifest: &NeedleTrainingManifest) {
    if let Some(version) = &manifest.needle_version {
        println!("needle_version: {version}");
    }
    println!("adapter_path: {}", work_dir.join(ADAPTER_FILE).display());
    println!(
        "candidate_path: {}",
        work_dir.join(CANDIDATE_FILE).display()
    );
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
        Command::Finetune { work_dir, needle } => {
            let manifest = run_needle_finetune(&work_dir, needle.as_os_str())?;
            print_finetune_summary(&work_dir, &manifest);
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

    #[test]
    fn parses_finetune_with_work_dir_and_needle() {
        let command = parse_args([
            "finetune",
            "--work-dir",
            "/tmp/p/.threadlane/needle-training",
            "--needle",
            "/tmp/needle",
        ])
        .unwrap();
        assert_eq!(
            command,
            Command::Finetune {
                work_dir: PathBuf::from("/tmp/p/.threadlane/needle-training"),
                needle: PathBuf::from("/tmp/needle"),
            }
        );
    }
}
