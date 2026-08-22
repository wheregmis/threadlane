use std::path::{Path, PathBuf};
use std::process::{self, Command as ProcessCommand};

use threadlane_runtime::needle_history_eval::{run_needle_eval_for_paths, NeedleEvalReport};
use threadlane_runtime::needle_training::{
    compare_candidate, export_needle_dataset, load_needle_eval_report, load_training_manifest,
    resolve_holdout_paths, run_needle_finetune, validate_evaluation_inputs,
    validate_evaluation_report_model, write_needle_eval_report, NeedleDatasetConfig,
    NeedleTrainingManifest, ADAPTER_FILE, CANDIDATE_EVAL_FILE, CANDIDATE_FILE, CURRENT_EVAL_FILE,
    MANIFEST_FILE, TRAIN_FILE,
};
use threadlane_runtime::types::AgentToolDefinition;
use threadlane_session::{CodingAgent, CodingAgentOptions, SystemPromptConfig};

const USAGE: &str = "usage: needle-project-train dataset --project <directory> --sessions <directory> --work-dir <directory> [--replace]\n       needle-project-train finetune --work-dir <directory> [--needle <path>]\n       needle-project-train evaluate --project <directory> --sessions <directory> --work-dir <directory>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvalReport {
    Current,
    Candidate,
}

impl EvalReport {
    fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "current" => Ok(Self::Current),
            "candidate" => Ok(Self::Candidate),
            _ => Err(USAGE),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Candidate => "candidate",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Current => CURRENT_EVAL_FILE,
            Self::Candidate => CANDIDATE_EVAL_FILE,
        }
    }
}

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
    Evaluate {
        project: PathBuf,
        sessions: PathBuf,
        work_dir: PathBuf,
    },
    EvaluateOne {
        project: PathBuf,
        sessions: PathBuf,
        work_dir: PathBuf,
        model: PathBuf,
        report: EvalReport,
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
        "evaluate" => {
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
                    _ => return Err(USAGE),
                }
            }
            Ok(Command::Evaluate {
                project: project.ok_or(USAGE)?,
                sessions: sessions.ok_or(USAGE)?,
                work_dir: work_dir.ok_or(USAGE)?,
            })
        }
        "evaluate-one" => {
            let mut model = None;
            let mut report = None;
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
                    "--model" if model.is_none() => {
                        model = Some(PathBuf::from(args.next().ok_or(USAGE)?));
                    }
                    "--report" if report.is_none() => {
                        report = Some(EvalReport::parse(&args.next().ok_or(USAGE)?)?);
                    }
                    _ => return Err(USAGE),
                }
            }
            Ok(Command::EvaluateOne {
                project: project.ok_or(USAGE)?,
                sessions: sessions.ok_or(USAGE)?,
                work_dir: work_dir.ok_or(USAGE)?,
                model: model.ok_or(USAGE)?,
                report: report.ok_or(USAGE)?,
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

async fn project_definitions(project: PathBuf) -> Vec<AgentToolDefinition> {
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
    agent.configured_tool_definitions()
}

fn current_model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../needle/needle2.cact")
}

fn spawn_evaluation(
    executable: &Path,
    project: &Path,
    sessions: &Path,
    work_dir: &Path,
    model: &Path,
    report: EvalReport,
) -> Result<(), String> {
    let status = ProcessCommand::new(executable)
        .arg("evaluate-one")
        .arg("--project")
        .arg(project)
        .arg("--sessions")
        .arg(sessions)
        .arg("--work-dir")
        .arg(work_dir)
        .arg("--model")
        .arg(model)
        .arg("--report")
        .arg(report.name())
        .status()
        .map_err(|_| "Needle evaluation child process could not be started.".to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Needle evaluation child process failed.".into())
    }
}

fn print_evaluation_summary(
    current: &NeedleEvalReport,
    candidate: &NeedleEvalReport,
    comparison: &threadlane_runtime::needle_training::NeedleCandidateComparison,
) {
    println!("current evaluation:\n{current}");
    println!("candidate evaluation:\n{candidate}");
    println!("promotable: {}", comparison.promotable);
    for reason in &comparison.reasons {
        println!("reason: {reason}");
    }
}

async fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Dataset {
            project,
            sessions,
            work_dir,
            replace,
        } => {
            let definitions = project_definitions(project).await;

            let manifest = export_needle_dataset(
                &NeedleDatasetConfig {
                    sessions_dir: sessions,
                    work_dir: work_dir.clone(),
                    replace,
                },
                &definitions,
            )?;
            print_dataset_summary(&work_dir, &manifest);
            Ok(())
        }
        Command::Finetune { work_dir, needle } => {
            let manifest = run_needle_finetune(&work_dir, needle.as_os_str())?;
            print_finetune_summary(&work_dir, &manifest);
            Ok(())
        }
        Command::Evaluate {
            project,
            sessions,
            work_dir,
        } => {
            let manifest = load_training_manifest(&work_dir)?;
            let definitions = project_definitions(project.clone()).await;
            let candidate_model = work_dir.join(CANDIDATE_FILE);
            validate_evaluation_inputs(&manifest, &definitions, &candidate_model)?;
            resolve_holdout_paths(&sessions, &manifest)?;

            let executable = std::env::current_exe()
                .map_err(|_| "Needle evaluation executable could not be resolved.".to_string())?;
            let current_model = current_model_path();
            for (model, report) in [
                (&current_model, EvalReport::Current),
                (&candidate_model, EvalReport::Candidate),
            ] {
                spawn_evaluation(&executable, &project, &sessions, &work_dir, model, report)?;
            }

            let current = load_needle_eval_report(&work_dir.join(CURRENT_EVAL_FILE))?;
            let candidate = load_needle_eval_report(&work_dir.join(CANDIDATE_EVAL_FILE))?;
            validate_evaluation_report_model(&current, &current_model)?;
            validate_evaluation_report_model(&candidate, &candidate_model)?;
            let comparison = compare_candidate(&manifest, &current, &candidate);
            print_evaluation_summary(&current, &candidate, &comparison);
            Ok(())
        }
        Command::EvaluateOne {
            project,
            sessions,
            work_dir,
            model,
            report,
        } => {
            let manifest = load_training_manifest(&work_dir)?;
            let definitions = project_definitions(project).await;
            validate_evaluation_inputs(&manifest, &definitions, &work_dir.join(CANDIDATE_FILE))?;
            let paths = resolve_holdout_paths(&sessions, &manifest)?;
            let result = run_needle_eval_for_paths(&paths, &definitions, &model)?;
            write_needle_eval_report(&work_dir.join(report.file()), &result)
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

    #[test]
    fn parses_public_evaluate_command() {
        assert_eq!(
            parse_args([
                "evaluate",
                "--project",
                "/tmp/p",
                "--sessions",
                "/tmp/p/.threadlane/sessions",
                "--work-dir",
                "/tmp/p/.threadlane/needle-training",
            ])
            .unwrap(),
            Command::Evaluate {
                project: PathBuf::from("/tmp/p"),
                sessions: PathBuf::from("/tmp/p/.threadlane/sessions"),
                work_dir: PathBuf::from("/tmp/p/.threadlane/needle-training"),
            }
        );
    }

    #[test]
    fn parses_internal_evaluate_one_command() {
        assert_eq!(
            parse_args([
                "evaluate-one",
                "--project",
                "/tmp/p",
                "--sessions",
                "/tmp/sessions",
                "--work-dir",
                "/tmp/work",
                "--model",
                "/tmp/model.cact",
                "--report",
                "candidate",
            ])
            .unwrap(),
            Command::EvaluateOne {
                project: PathBuf::from("/tmp/p"),
                sessions: PathBuf::from("/tmp/sessions"),
                work_dir: PathBuf::from("/tmp/work"),
                model: PathBuf::from("/tmp/model.cact"),
                report: EvalReport::Candidate,
            }
        );
        assert!(parse_args(["evaluate-one", "--report", "other"]).is_err());
    }
}
