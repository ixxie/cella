mod cell;
mod cli;
mod exec;
mod client;
mod config;
mod deploy;
mod git;
mod log;
mod server;
mod secrets;
mod proxy;
mod remote;
mod transport;
mod ssh;
mod vm;
mod worktree;

use anyhow::Result;

fn main() -> Result<()> {
    let argv0 = std::env::args()
        .next()
        .unwrap_or_default();
    if argv0.ends_with("git-remote-cella") {
        remote::run()
    } else {
        // skip log::init for secrets commands (background log thread crashes some terminals)
        let is_secrets = std::env::args().nth(1).as_deref() == Some("secrets");
        if !is_secrets {
            log::init();
        }
        cli::run()
    }
}
