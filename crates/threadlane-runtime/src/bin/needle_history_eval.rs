use std::path::PathBuf;
use std::process;
use threadlane_runtime::needle_history_eval::{
    run_needle_history_eval, NeedleEvalConfig,
};

const USAGE: &str = "usage: needle-history-eval --sessions <directory> --tools <provider-tools.json>";

fn parse_args<I, S>(args: I) -> Result<NeedleEvalConfig, &'static str>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut sessions_dir = None;
    let mut tools_path = None;
    let mut args = args.into_iter().map(Into::into);
    while let Some(flag) = args.next() {
        let value = match flag.as_str() {
            "--sessions" if sessions_dir.is_none() => args.next().ok_or(USAGE)?,
            "--tools" if tools_path.is_none() => args.next().ok_or(USAGE)?,
            _ => return Err(USAGE),
        };
        match flag.as_str() {
            "--sessions" => sessions_dir = Some(PathBuf::from(value)),
            "--tools" => tools_path = Some(PathBuf::from(value)),
            _ => unreachable!(),
        }
    }
    match (sessions_dir, tools_path) {
        (Some(sessions_dir), Some(tools_path)) => Ok(NeedleEvalConfig {
            sessions_dir,
            tools_path,
        }),
        _ => Err(USAGE),
    }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() == 1 && args[0] == "--help" {
        println!("{USAGE}");
        return;
    }
    let config = match parse_args(args) {
        Ok(config) => config,
        Err(_) => {
            eprintln!("error: invalid arguments\n{USAGE}");
            process::exit(3);
        }
    };
    match run_needle_history_eval(&config) {
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
    fn parses_required_explicit_paths() {
        let config = parse_args([
            "--sessions",
            "/tmp/sessions",
            "--tools",
            "/tmp/tools.json",
        ])
        .unwrap();
        assert_eq!(config.sessions_dir, PathBuf::from("/tmp/sessions"));
        assert_eq!(config.tools_path, PathBuf::from("/tmp/tools.json"));
    }

    #[test]
    fn rejects_missing_values() {
        let error = parse_args(["--sessions", "/tmp/sessions", "--tools"])
            .err()
            .unwrap();
        assert_eq!(error, USAGE);
    }

    #[test]
    fn rejects_duplicate_flags() {
        let error = parse_args([
            "--sessions",
            "/tmp/one",
            "--sessions",
            "/tmp/two",
            "--tools",
            "/tmp/tools.json",
        ])
        .err()
        .unwrap();
        assert_eq!(error, USAGE);
    }

    #[test]
    fn rejects_unknown_arguments() {
        let error = parse_args([
            "--sessions",
            "/tmp/sessions",
            "--tools",
            "/tmp/tools.json",
            "--secret",
        ])
        .err()
        .unwrap();
        assert_eq!(error, USAGE);
    }
}
