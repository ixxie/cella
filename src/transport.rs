use anyhow::{Context, Result};
use console::style;
use tracing::instrument;

use crate::{client, config, git, secrets, server, vm};

fn ok() -> console::StyledObject<&'static str> { style("✓").green() }
fn up_icon() -> console::StyledObject<&'static str> { style("▲").green() }
fn add() -> console::StyledObject<&'static str> { style("+").green() }
fn bold(s: &str) -> console::StyledObject<&str> { style(s).bold() }

fn spinner(msg: &str) -> indicatif::ProgressBar {
    let pb = indicatif::ProgressBar::new_spinner();
    pb.set_style(
        indicatif::ProgressStyle::default_spinner()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
            .template("  {spinner} {msg}")
            .unwrap()
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}

pub trait Transport {
    fn is_running(&self, cell: &str) -> Result<bool>;
    fn ensure_running(&self, cell: &str, repo: &git::Repo, cfg: &config::CellaConfig) -> Result<()>;
    fn shell(&self, cell: &str, session: Option<&str>, command: Option<&str>, cfg: &config::SessionConfig) -> Result<()>;
}

// Local transport — calls vm.rs directly

pub struct LocalTransport;

impl Transport for LocalTransport {
    fn is_running(&self, cell: &str) -> Result<bool> {
        vm::is_running(cell)
    }

    #[instrument(skip(self, repo, cfg))]
    fn ensure_running(&self, cell: &str, repo: &git::Repo, cfg: &config::CellaConfig) -> Result<()> {
        if self.is_running(cell)? {
            return Ok(());
        }

        if !repo.branch_exists(cell) {
            let sp = spinner(&format!("creating branch {}", cell));
            repo.create_branch(cell)?;
            sp.finish_with_message(format!("{} created branch {}", add(), bold(cell)));
        }

        let sp = spinner(&format!("booting {}", cell));
        secrets::resolve(cell, repo.root(), cfg)?;
        repo.init_clone(cell, cfg)?;
        let repo_name = repo.root()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        vm::start(cell, repo_name, cfg)?;
        sp.finish_with_message(format!("{} booted {}", up_icon(), bold(cell)));

        vm::write_autostop_timeout(cell, cfg.shell_timeout);
        Ok(())
    }

    fn shell(&self, cell: &str, session: Option<&str>, command: Option<&str>, cfg: &config::SessionConfig) -> Result<()> {
        vm::shell(cell, session, command, Some(cfg))
    }
}

// Remote transport — calls client.rs over SSH

pub struct RemoteTransport {
    pub client: client::Client,
}

impl Transport for RemoteTransport {
    fn is_running(&self, cell: &str) -> Result<bool> {
        let cells = self.client.list().unwrap_or_default();
        Ok(cells.iter().any(|c| c.name == cell && c.status == "running"))
    }

    #[instrument(skip(self, repo, cfg))]
    fn ensure_running(&self, cell: &str, repo: &git::Repo, cfg: &config::CellaConfig) -> Result<()> {
        if self.is_running(cell)? {
            return Ok(());
        }

        let repo_name = repo.root()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        if !repo.branch_exists(cell) {
            let sp = spinner(&format!("creating branch {}", cell));
            repo.create_branch(cell)?;
            sp.finish_with_message(format!("{} created branch {}", add(), bold(cell)));
        }

        // prepare: init repo on server (no build)
        let sp = spinner("preparing cell");
        self.client.prepare(cell, repo_name, cfg)?;
        sp.finish_with_message(format!("{} cell ready", ok()));

        // update remote URL with cell name for resolve-cell
        let remote_url = format!("cella://{}/{}", self.client.user_host(), cell);
        repo.add_cella_remote(&remote_url).ok();

        // push code BEFORE build — .cella/flake.nix is available
        let sp = spinner("pushing code");
        let push = std::process::Command::new("git")
            .args(["push", "cella", cell])
            .current_dir(repo.root())
            .output()
            .context("git push failed")?;
        if push.status.success() {
            sp.finish_with_message(format!("{} pushed", ok()));
        } else {
            sp.finish_with_message(format!("{} push failed", style("!").yellow()));
        }

        // build and start with full config
        let sp = spinner(&format!("booting {}", cell));
        self.client.up(cell, repo_name, false, cfg)?;
        sp.finish_with_message(format!("{} booted {}", up_icon(), bold(cell)));

        // sync user files
        let client_cfg = server::load_client_config();
        if !client_cfg.sync.is_empty() {
            let sp = spinner("syncing files");
            self.client.sync_files(cell, &client_cfg.sync)?;
            sp.finish_with_message(format!("{} synced", ok()));
        }

        Ok(())
    }

    fn shell(&self, cell: &str, session: Option<&str>, _command: Option<&str>, _cfg: &config::SessionConfig) -> Result<()> {
        self.client.shell(cell, session)
    }
}
