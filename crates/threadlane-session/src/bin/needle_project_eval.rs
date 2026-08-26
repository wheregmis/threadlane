use std::path::PathBuf;
use std::process;
use threadlane_session::needle_history_eval::run_needle_history_eval_with_definitions;
use threadlane_session::{CodingAgent, CodingAgentOptions, SystemPromptConfig};

const USAGE: &str = "usage: needle-project-eval --project <directory> --sessions <directory>";

fn parse_args<I, S>(args: I) -> Result<(PathBuf, PathBuf), &'static str>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut project = None;
    let mut sessions = None;
    let mut args = args.into_iter().map(Into::into);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(USAGE)?;
        match flag.as_str() {
            "--project" if project.is_none() => project = Some(PathBuf::from(value)),
            "--sessions" if sessions.is_none() => sessions = Some(PathBuf::from(value)),
            _ => return Err(USAGE),
        }
    }
    project.zip(sessions).ok_or(USAGE)
}

#[tokio::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--help"] {
        println!("{USAGE}");
        return;
    }
    let (project, sessions) = match parse_args(args) {
        Ok(config) => config,
        Err(_) => {
            eprintln!("error: invalid arguments\n{USAGE}");
            process::exit(3);
        }
    };

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

    match run_needle_history_eval_with_definitions(&sessions, agent.configured_tool_definitions()) {
        Ok(report) => {
            println!("{report}");
            process::exit(report.decision.exit_code());
        }
        Err(error) => {
            eprintln!("error: {error}\n{USAGE}");
            process::exit(3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_project_and_sessions() {
        let (project, sessions) =
            parse_args(["--project", "/tmp/project", "--sessions", "/tmp/sessions"]).unwrap();
        assert_eq!(project, PathBuf::from("/tmp/project"));
        assert_eq!(sessions, PathBuf::from("/tmp/sessions"));
    }
}
