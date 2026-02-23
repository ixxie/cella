mod cell;
mod cli;
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
mod vm;

use anyhow::Result;

fn main() -> Result<()> {
    let argv0 = std::env::args()
        .next()
        .unwrap_or_default();
    if argv0.ends_with("git-remote-cella") {
        remote::run()
    } else {
        log::init();
        cli::run()
    }
}
