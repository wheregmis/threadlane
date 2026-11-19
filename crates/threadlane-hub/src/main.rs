//! `threadlane-hub`: run the always-on node of one space.
//!
//! ```text
//! threadlane-hub [--data-dir DIR] [run|check]
//! ```
//!
//! The data directory defaults to `$THREADLANE_HUB_DIR`, then `./.threadlane-dev/hub`.
//! `run` serves the client interface on `<data dir>/hub.sock` and runs the
//! hub's roles until Ctrl-C; `check` checks the database and exits.

use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;

use anyhow::{Context, bail};
use threadlane_hub::config::{DataDir, HubConfig};
use threadlane_hub::{Hub, init_logging};

const USAGE: &str = "usage: threadlane-hub [--data-dir DIR] [run|check]";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Run,
    Check,
}

fn parse_args() -> anyhow::Result<(DataDir, Command)> {
    let mut data_dir = std::env::var_os("THREADLANE_HUB_DIR").map(PathBuf::from);
    let mut command = Command::Run;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                data_dir = Some(args.next().context("--data-dir needs a path")?.into());
            }
            "run" => command = Command::Run,
            "check" => command = Command::Check,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    let data_dir = data_dir.unwrap_or_else(|| PathBuf::from(".threadlane-dev/hub"));
    Ok((DataDir::new(data_dir), command))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (data_dir, command) = parse_args()?;
    let config = HubConfig::load(data_dir.root())?;
    init_logging(&config.log)?;
    let hub = Hub::open(data_dir, config).await?;
    hub.check().await?;
    match command {
        Command::Check => Ok(()),
        Command::Run => {
            let restart = hub
                .run_updatable(async {
                    if let Err(error) = tokio::signal::ctrl_c().await {
                        tracing::error!(%error, "cannot listen for shutdown; stopping");
                    }
                })
                .await?;
            drop(hub);
            if let Some(executable) = restart {
                return Err(std::process::Command::new(executable)
                    .args(std::env::args_os().skip(1))
                    .exec())
                .context("The update was installed, but the hub could not restart");
            }
            Ok(())
        }
    }
}
