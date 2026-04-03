mod flow_run;
mod service;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

use flow::{Decision, parse_duration, write_result_to, write_signal};
use std::path::Path;

#[derive(Parser)]
#[command(
    name = "cellx",
    about = "In-VM executor for cella flows and services",
)]
struct Cellx {
    #[command(subcommand)]
    command: CellxCmd,
}

#[derive(Subcommand)]
enum CellxCmd {
    /// Manage flow execution
    Flow(FlowArgs),
    /// Manage background services
    Service(ServiceArgs),
}

#[derive(Args)]
struct FlowArgs {
    #[command(subcommand)]
    action: FlowAction,
}

#[derive(Subcommand)]
enum FlowAction {
    /// Run the flow loop (blocking)
    Run {
        /// Flow name
        name: String,
        /// JSON params for the start op
        #[arg(long)]
        params: Option<String>,
    },
    /// Transition to an op (used by runners)
    To {
        /// Target op name
        op: String,
        /// JSON params to pass to the next op
        #[arg(long)]
        params: Option<String>,
    },
    /// Rerun the current op (used by runners)
    Retry {
        /// Delay before retry (e.g. 30m, 1h, 300)
        #[arg(long)]
        after: Option<String>,
    },
    /// Pause the flow loop
    Pause,
    /// End the flow
    Done,
    /// Show flow status
    Status,
}

#[derive(Args)]
struct ServiceArgs {
    #[command(subcommand)]
    action: ServiceAction,
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Start a background service
    Start {
        /// Service name
        name: String,
        /// Command to run
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Stop a service
    Stop {
        /// Service name
        name: String,
    },
    /// Restart a service (optionally with a new command)
    Restart {
        /// Service name
        name: String,
        /// New command (uses previous if omitted)
        #[arg(trailing_var_arg = true)]
        cmd: Vec<String>,
    },
    /// View service logs
    Logs {
        /// Service name
        name: String,
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },
    /// List running services
    List,
}

fn main() -> Result<()> {
    let cli = Cellx::parse();
    match cli.command {
        CellxCmd::Flow(args) => match args.action {
            FlowAction::Run { name, params } => {
                let repo = detect_repo_root()?;
                let params = params
                    .map(|p| serde_json::from_str(&p))
                    .transpose()
                    .map_err(|e| anyhow::anyhow!("invalid --params JSON: {e}"))?;
                flow_run::run(&name, &repo, params)
            }
            FlowAction::To { op, params } => {
                let ws = workspace_from_env()?;
                let params = params
                    .map(|p| serde_json::from_str(&p))
                    .transpose()
                    .map_err(|e| anyhow::anyhow!("invalid --params JSON: {e}"))?;
                write_result_to(ws, &Decision::To { op, params })
            }
            FlowAction::Retry { after } => {
                let ws = workspace_from_env()?;
                let secs = after.as_deref().and_then(parse_duration);
                write_result_to(ws, &Decision::Retry { after: secs })
            }
            FlowAction::Pause => {
                write_signal("pause")
            }
            FlowAction::Done => {
                let ws = workspace_from_env()?;
                write_result_to(ws, &Decision::Done)
            }
            FlowAction::Status => {
                flow_run::print_status()
            }
        }
        CellxCmd::Service(args) => match args.action {
            ServiceAction::Start { name, cmd } => {
                service::start(&name, &cmd.join(" "))
            }
            ServiceAction::Stop { name } => {
                service::stop(&name)
            }
            ServiceAction::Restart { name, cmd } => {
                let c = if cmd.is_empty() { None } else { Some(cmd.join(" ")) };
                service::restart(&name, c.as_deref())
            }
            ServiceAction::Logs { name, follow } => {
                service::logs(&name, follow)
            }
            ServiceAction::List => {
                service::list()
            }
        }
    }
}

fn workspace_from_env() -> Result<&'static Path> {
    static WS: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let ws = WS.get_or_init(|| {
        std::env::var("OP_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp/cellx"))
    });
    Ok(ws.as_path())
}

fn detect_repo_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join(".cella").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            anyhow::bail!("not in a cella repo (no .cella/ found)");
        }
    }
}
