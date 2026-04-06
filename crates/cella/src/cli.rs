use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use console::{style, Style};

use tracing::instrument;
use crate::{client, config, deploy, git, server, proxy, secrets, transport, vm, worktree};
use crate::transport::Transport;

#[derive(Parser)]
#[command(
    name = "cella",
    about = "Sandboxed development environments",
    after_help = "Run 'cella <command> --help' for details on each command."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scaffold .cella/ for a repo
    Init,
    /// Create a new branch and add it to cella
    Create(CreateArgs),
    /// Add an existing branch to cella
    Add(AddArgs),
    /// Remove a branch from cella
    Remove(RemoveArgs),
    /// List cella-managed branches
    List {
        /// Show cells from all repos
        #[arg(short, long)]
        all: bool,
    },
    /// Print worktree path for a branch
    Path {
        /// Branch name
        name: String,
    },
    /// Print shell hook for cella cd (add to shell config)
    Hook {
        /// Shell: fish, bash, zsh, nu, powershell
        shell: String,
    },
    /// Start or resume a flow
    Run(RunArgs),
    /// Pause the running flow (cell stays up)
    Pause {
        /// Branch name (optional if in worktree)
        name: Option<String>,
    },
    /// Stop flow and shut down cell
    Stop(StopArgs),
    /// Show flow status
    Status {
        /// Branch name (omit for all)
        name: Option<String>,
    },
    /// SSH into a cell
    Shell(ShellArgs),
    /// View flow output from a cell
    Logs(LogsArgs),
    /// Forward declared ports from a remote cell to localhost
    Tunnel(TunnelArgs),
    /// Manage encrypted secrets
    Secrets(SecretsArgs),
    /// Manage servers
    Server(ServerArgs),
    #[command(hide = true)]
    /// Internal utilities
    Util(UtilArgs),
}

#[derive(Args, Debug)]
struct CreateArgs {
    /// Branch name to create
    name: String,
    /// Server to use
    #[arg(short, long)]
    server: Option<String>,
}

#[derive(Args, Debug)]
struct AddArgs {
    /// Existing branch name
    name: String,
    /// Server to use
    #[arg(short, long)]
    server: Option<String>,
}

#[derive(Args, Debug)]
struct RemoveArgs {
    /// Branch name
    name: String,
    /// Also delete the git branch
    #[arg(short, long)]
    delete: bool,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Flow name (matches .cella/flows/{name}/)
    name: Option<String>,
    /// Branch name (optional if in worktree)
    #[arg(short, long)]
    branch: Option<String>,
    /// Server to run on
    #[arg(short, long)]
    server: Option<String>,
    /// Detach from flow output (default is attached)
    #[arg(short, long)]
    detach: bool,
    /// Params as key=value pairs (e.g. cella run dev -- project="my project")
    #[arg(last = true)]
    params: Vec<String>,
}

#[derive(Args, Debug)]
struct StopArgs {
    /// Branch name (optional if in worktree)
    name: Option<String>,
    /// Also remove from cella (tear down worktree + cell)
    #[arg(short, long)]
    delete: bool,
}

#[derive(Args)]
struct ShellArgs {
    /// Branch name (optional if in worktree)
    name: Option<String>,
    /// Run a command instead of interactive shell
    #[arg(short = 'c', long = "command")]
    command: Option<String>,
    /// Server-side mode (skip repo checks)
    #[arg(long, hide = true)]
    server: bool,
}

#[derive(Args)]
struct LogsArgs {
    /// Branch name (optional if in worktree)
    name: Option<String>,
    /// Follow log output
    #[arg(short, long)]
    follow: bool,
}

#[derive(Args)]
struct TunnelArgs {
    /// Branch name (optional if in worktree)
    name: Option<String>,
    /// Ports to forward, supports ranges (e.g. -p 5173 -p 8001-8004)
    #[arg(short, long)]
    port: Vec<String>,
    /// Open in default browser
    #[arg(short, long)]
    open: bool,
}

#[derive(Args)]
struct ServerArgs {
    #[command(subcommand)]
    action: ServerAction,
}

#[derive(Subcommand)]
enum ServerAction {
    /// Add a server to the registry
    Add {
        /// Server name
        name: String,
        /// SSH target (e.g. root@1.2.3.4)
        target: String,
    },
    /// Remove a server from the registry
    Remove {
        /// Server name
        name: String,
    },
    /// List registered servers
    List,
    /// Deploy server config from current directory
    Deploy,
    /// Garbage collect stopped cells older than a threshold
    Gc(GcArgs),
    #[command(hide = true)]
    /// Run the network proxy (used by systemd)
    Proxy(ProxyArgs),
}

#[derive(Args, Debug)]
struct GcArgs {
    /// Delete cells stopped longer than this (e.g. "7d", "24h", "1h")
    #[arg(long, default_value = "7d")]
    older_than: String,
    /// Dry run — show what would be deleted without deleting
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct ProxyArgs {
    /// Path to proxy config JSON
    #[arg(short, long, default_value = "/etc/cella/proxy-config.json")]
    config: String,
}

#[derive(Args)]
struct SecretsArgs {
    #[command(subcommand)]
    action: SecretsAction,
}

#[derive(Subcommand)]
enum SecretsAction {
    /// Encrypt .cella/secrets.env → .cella/secrets.age
    Encrypt {
        /// Recipient public key (overrides config)
        #[arg(short, long)]
        recipient: Option<String>,
    },
    /// Decrypt, open in $EDITOR, re-encrypt on save
    Edit {
        /// Recipient public key (overrides config)
        #[arg(short, long)]
        recipient: Option<String>,
    },
}

#[derive(Args)]
struct UtilArgs {
    #[command(subcommand)]
    action: UtilAction,
}

#[derive(Subcommand)]
enum UtilAction {
    /// Print the cell repo path for the current VM (used by git-remote-cella)
    ResolveCell {
        /// Match cells for this repo name
        #[arg(long)]
        repo: Option<String>,
    },
    /// Manage /etc/hosts entries for tunnels
    Hosts {
        #[command(subcommand)]
        action: HostsAction,
    },
}

#[derive(Subcommand)]
enum HostsAction {
    /// Add a tunnel entry to /etc/hosts
    Add {
        /// IP address
        ip: String,
        /// Hostname
        hostname: String,
    },
    /// Remove a tunnel entry from /etc/hosts
    Remove {
        /// Hostname to remove
        hostname: String,
    },
}

// Styles

fn ok() -> console::StyledObject<&'static str> { style("✓").green() }
fn dn() -> console::StyledObject<&'static str> { style("▼").red() }
fn rm() -> console::StyledObject<&'static str> { style("✕").red() }
fn arrow() -> console::StyledObject<&'static str> { style("→").cyan() }
fn add() -> console::StyledObject<&'static str> { style("+").green() }

fn dim(s: &str) -> console::StyledObject<&str> { style(s).dim() }
fn bold(s: &str) -> console::StyledObject<&str> { style(s).bold() }

fn vm_status(s: &str) -> String {
    match s {
        "running" => style(s).green().to_string(),
        "stopped" => style(s).red().to_string(),
        _ => style(s).yellow().to_string(),
    }
}

fn flow_state(s: &str) -> String {
    match s {
        "running" => style(s).green().to_string(),
        "paused" => style(s).yellow().to_string(),
        "done" => style(s).dim().to_string(),
        "failed" => style(s).red().to_string(),
        _ => s.to_string(),
    }
}

// Dispatch

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Server(args) => match args.action {
            ServerAction::Proxy(proxy_args) => {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(proxy::run(&proxy_args.config))
            }
            ServerAction::Add { name, target } => cmd_server_add(&name, &target),
            ServerAction::Remove { name } => cmd_server_remove(&name),
            ServerAction::List => cmd_server_list(),
            ServerAction::Deploy => cmd_deploy(),
            ServerAction::Gc(args) => cmd_gc(args),
        },
        Commands::Util(args) => match args.action {
            UtilAction::ResolveCell { repo } => cmd_resolve_cell(repo.as_deref()),
            UtilAction::Hosts { action } => cmd_hosts(action),
        },

        // commands that don't need cell context
        Commands::Init => {
            let repo = git::Repo::open()?;
            cmd_init(&repo)
        }
        Commands::Create(args) => {
            let repo = git::Repo::open()?;
            cmd_create(&repo, args)
        }
        Commands::Add(args) => {
            let repo = git::Repo::open()?;
            cmd_add(&repo, args)
        }
        Commands::Remove(args) => {
            let repo = git::Repo::open()?;
            cmd_remove(&repo, args)
        }
        Commands::List { all } => cmd_list(git::Repo::open().ok().as_ref(), all),
        Commands::Path { name } => {
            let repo = git::Repo::open()?;
            cmd_path(&repo, &name)
        }
        Commands::Hook { shell } => cmd_hook(&shell),
        Commands::Status { name } => {
            let repo = git::Repo::open()?;
            cmd_status(&repo, name.as_deref())
        }
        Commands::Secrets(args) => {
            let repo = git::Repo::open()?;
            cmd_secrets(&repo, args)
        }

        // commands that need cell context (resolve from worktree or explicit arg)
        Commands::Shell(args) if args.server => {
            vm::shell(&args.name.unwrap_or_default(), args.command.as_deref())
        }
        Commands::Run(args) => {
            let (repo, cell) = worktree::resolve_cell(args.branch.as_deref())?;
            cmd_run(&repo, &cell, args)
        }
        Commands::Stop(args) => {
            let (repo, cell) = worktree::resolve_cell(args.name.as_deref())?;
            cmd_stop(&repo, &cell, args)
        }
        Commands::Pause { name } => {
            let (repo, cell) = worktree::resolve_cell(name.as_deref())?;
            cmd_pause(&repo, &cell)
        }
        Commands::Logs(args) => {
            let (repo, cell) = worktree::resolve_cell(args.name.as_deref())?;
            cmd_logs(&repo, &cell, args)
        }
        Commands::Shell(args) => {
            let (repo, cell) = worktree::resolve_cell(args.name.as_deref())?;
            cmd_shell(&repo, &cell, args)
        }
        Commands::Tunnel(args) => {
            let (repo, cell) = worktree::resolve_cell(args.name.as_deref())?;
            cmd_tunnel(&repo, &cell, args)
        }
    }
}

// Helpers

/// Find which server a cell is running on by scanning all known servers.
/// Falls back to config defaults (repo > client).
fn find_cell_server(repo: &git::Repo, cell: &str) -> Result<server::ActiveServer> {
    // check localhost
    if vm::is_running(cell).unwrap_or(false) {
        return Ok(server::ActiveServer::Localhost);
    }

    // check remote servers
    for (name, target) in server::list()? {
        if let Ok(c) = client::Client::connect(&target) {
            let cells = c.list().unwrap_or_default();
            if cells.iter().any(|c| c.name == cell && c.status == "running") {
                return Ok(server::ActiveServer::Remote { name });
            }
        }
    }

    // fall back to config defaults
    let cfg = config::load(repo.root())?;
    let client_cfg = server::load_client_config();
    if let Some(srv) = cfg.server.as_ref().or(client_cfg.server.as_ref()) {
        return server::resolve(srv);
    }

    Ok(server::ActiveServer::Localhost)
}

fn connect_remote(s: &server::ActiveServer) -> Result<client::Client> {
    let target = s.target()?
        .ok_or_else(|| anyhow::anyhow!("server has no target"))?;
    client::Client::connect(&target)
}

fn make_transport(active: &server::ActiveServer) -> Result<Box<dyn Transport>> {
    if active.is_server() {
        let c = connect_remote(active)?;
        Ok(Box::new(transport::RemoteTransport { client: c }))
    } else {
        Ok(Box::new(transport::LocalTransport))
    }
}

fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

// Init

fn cmd_init(repo: &git::Repo) -> Result<()> {
    let cella_dir = repo.root().join(".cella");
    std::fs::create_dir_all(&cella_dir)?;

    let config_path = cella_dir.join("config.toml");
    if !config_path.exists() {
        std::fs::write(&config_path, "memory = \"4096M\"\nvcpu = 2\n")?;
        println!("  {} .cella/config.toml", add());
    }

    git::ensure_gitignore_entry(repo.root(), ".cella/trees/")?;

    repo.add_cella_remote("cella://localhost")?;
    println!("{} initialized cella", ok());
    Ok(())
}

// Branch lifecycle commands

fn cmd_create(repo: &git::Repo, args: CreateArgs) -> Result<()> {
    let name = &args.name;
    if repo.branch_exists(name) {
        anyhow::bail!("branch '{name}' already exists — use 'cella add {name}' instead");
    }

    repo.create_branch(name)?;
    println!("  {} branch {}", add(), bold(name));

    let path = worktree::add(repo, name)?;
    println!("  {} worktree at {}", add(), dim(&path.display().to_string()));

    println!("{} created {}", ok(), bold(name));
    Ok(())
}

fn cmd_add(repo: &git::Repo, args: AddArgs) -> Result<()> {
    let name = &args.name;
    if !repo.branch_exists(name) {
        anyhow::bail!("branch '{name}' does not exist — use 'cella create {name}' to create it");
    }

    let path = worktree::add(repo, name)?;
    println!("  {} worktree at {}", add(), dim(&path.display().to_string()));

    println!("{} added {}", ok(), bold(name));
    Ok(())
}

fn cmd_remove(repo: &git::Repo, args: RemoveArgs) -> Result<()> {
    let name = &args.name;

    // stop cell if running
    if let Ok(active) = find_cell_server(repo, name) {
        let t = make_transport(&active)?;
        t.flow_stop(name).ok();
        if active.is_server() {
            let c = connect_remote(&active)?;
            c.delete(name).ok();
        } else if vm::is_running(name).unwrap_or(false) {
            vm::stop(name)?;
        }
    }

    worktree::remove(repo, name)?;
    println!("  {} worktree removed", rm());

    if args.delete {
        repo.delete_branch(name).ok();
        println!("  {} branch deleted", rm());
    }

    println!("{} removed {}", ok(), bold(name));
    Ok(())
}

fn cmd_path(repo: &git::Repo, name: &str) -> Result<()> {
    let path = worktree::tree_path(repo.root(), name);
    if !path.exists() {
        anyhow::bail!("no worktree for '{name}' — use 'cella add {name}' first");
    }
    println!("{}", path.display());
    Ok(())
}

fn cmd_hook(shell: &str) -> Result<()> {
    let hook = match shell {
        "fish" => r#"function cella
    if test "$argv[1]" = "cd"
        cd (command cella path $argv[2])
    else
        command cella $argv
    end
end"#,
        "bash" | "zsh" => r#"cella() {
    if [ "$1" = "cd" ]; then
        cd "$(command cella path "$2")"
    else
        command cella "$@"
    fi
}"#,
        "nu" | "nushell" => r#"def --wrapped cella [...args: string] {
    if ($args | first) == "cd" {
        cd (^cella path ($args | get 1))
    } else {
        ^cella ...$args
    }
}"#,
        "powershell" | "pwsh" => r#"function cella {
    if ($args[0] -eq "cd") {
        Set-Location (& cella.exe path $args[1])
    } else {
        & cella.exe @args
    }
}"#,
        _ => anyhow::bail!("unsupported shell '{shell}' — use fish, bash, zsh, nu, or powershell"),
    };
    println!("{hook}");
    Ok(())
}

// Server commands

fn cmd_server_add(name: &str, target: &str) -> Result<()> {
    server::add(name, target)?;
    println!("{} added {} {}", add(), bold(name), dim(&format!("({target})")));
    Ok(())
}

fn cmd_server_remove(name: &str) -> Result<()> {
    server::remove(name)?;
    println!("{} removed {}", rm(), bold(name));
    Ok(())
}

fn cmd_server_list() -> Result<()> {
    let servers = server::list()?;
    let hdr = Style::new().bold();
    println!("  {:<20} {}", hdr.apply_to("SERVER"), hdr.apply_to("TARGET"));
    println!("  {:<20} {}", "localhost", dim("—"));
    for (name, target) in &servers {
        println!("  {:<20} {}", name, dim(target));
    }
    Ok(())
}

fn cmd_deploy() -> Result<()> {
    deploy::run()
}

fn cmd_gc(args: GcArgs) -> Result<()> {
    let threshold_secs = flow::parse_duration(&args.older_than)
        .ok_or_else(|| anyhow::anyhow!("invalid duration '{}' — use e.g. 7d, 24h, 1h", args.older_than))?;
    let now = flow::now_secs();

    let cells = vm::list_cells().unwrap_or_default();
    let mut deleted = 0;

    for name in &cells {
        if vm::is_running(name).unwrap_or(false) {
            continue;
        }

        // check flow-status.json for last activity timestamp
        let status_path = crate::cell::cell_dir(name).join("flow-status.json");
        let last_active = std::fs::read_to_string(&status_path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["op_started_at"].as_u64())
            .unwrap_or(0);

        // fallback: check cell dir mtime
        let last_active = if last_active > 0 {
            last_active
        } else {
            std::fs::metadata(crate::cell::cell_dir(name))
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0)
        };

        if last_active == 0 {
            continue;
        }

        let age = now.saturating_sub(last_active);
        if age < threshold_secs {
            continue;
        }

        let age_str = if age >= 86400 {
            format!("{}d", age / 86400)
        } else if age >= 3600 {
            format!("{}h", age / 3600)
        } else {
            format!("{}m", age / 60)
        };

        if args.dry_run {
            println!("  {} would delete {} (stopped {})", dim("~"), bold(name), dim(&age_str));
        } else {
            vm::delete(name)?;
            println!("  {} deleted {} (stopped {})", rm(), bold(name), dim(&age_str));
            deleted += 1;
        }
    }

    if args.dry_run {
        println!("{} dry run — no cells deleted", dim("ℹ"));
    } else if deleted == 0 {
        println!("  {}", dim("no stale cells to clean up"));
    } else {
        println!("{} cleaned up {} cell{}", ok(), deleted, if deleted == 1 { "" } else { "s" });
    }
    Ok(())
}

// Cell commands

fn cmd_resolve_cell(repo_filter: Option<&str>) -> Result<()> {
    if let Ok(cells) = vm::list_cells() {
        for name in &cells {
            if !vm::is_running(name).unwrap_or(false) {
                continue;
            }
            if let Some(filter) = repo_filter {
                let rt = vm::runtime_dir(name);
                let cell_repo = std::fs::read_to_string(rt.join("repo"))
                    .unwrap_or_default().trim().to_string();
                if cell_repo != filter {
                    continue;
                }
            }
            println!("{}", vm::cell_repo_dir(name).display());
            return Ok(());
        }
    }

    let repo = git::Repo::open()?;
    let path = repo.resolve_cell_path()?;
    println!("{}", path.display());
    Ok(())
}

#[instrument(skip(repo), fields(cell = %cell))]
fn cmd_stop(repo: &git::Repo, cell: &str, args: StopArgs) -> Result<()> {
    let active = find_cell_server(repo, cell)?;
    let t = make_transport(&active)?;

    t.flow_stop(cell).ok();

    if active.is_server() {
        let c = connect_remote(&active)?;
        c.down(cell)?;
        println!("{} stopped {}", dn(), bold(cell));
    } else if vm::is_running(cell).unwrap_or(false) {
        vm::stop(cell)?;
        println!("{} stopped {}", dn(), bold(cell));
    } else {
        println!("  {} not running", bold(cell));
    }

    if args.delete {
        cmd_remove(repo, RemoveArgs {
            name: cell.to_string(),
            delete: true,
        })?;
    }
    Ok(())
}

#[instrument(skip_all, fields(cell = %cell))]
fn cmd_shell(repo: &git::Repo, cell: &str, args: ShellArgs) -> Result<()> {
    let active = find_cell_server(repo, cell)?;
    let cfg = config::load(repo.root())?;
    let t = make_transport(&active)?;
    t.ensure_running(cell, repo, &cfg)?;

    println!("{} entering {}", arrow(), bold(cell));
    t.shell(cell, args.command.as_deref())
}

fn cmd_list(repo: Option<&git::Repo>, show_all: bool) -> Result<()> {
    let current_branch = repo.and_then(|r| r.current_branch().ok());
    let now = flow::now_secs();

    struct Row {
        name: String,
        status: String,
        server: String,
        repo: Option<String>,
        flow: Option<client::FlowInfo>,
    }

    let mut rows: Vec<Row> = Vec::new();

    // local cells
    let local_names = if let Some(repo) = repo {
        repo.list_clones().unwrap_or_default()
    } else {
        vm::list_cells().unwrap_or_default()
    };
    for name in local_names {
        let running = vm::is_running(&name).unwrap_or(false);
        let rt = vm::runtime_dir(&name);
        let cell_repo = std::fs::read_to_string(rt.join("repo"))
            .ok().map(|s| s.trim().to_string());
        let flow = {
            let path = crate::cell::cell_dir(&name).join("flow-status.json");
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        };
        rows.push(Row {
            name,
            status: if running { "running" } else { "stopped" }.to_string(),
            server: "localhost".to_string(),
            repo: cell_repo,
            flow,
        });
    }

    // remote cells
    for (srv_name, target) in server::list()? {
        let c = match client::Client::connect(&target) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let cells = c.list().unwrap_or_default();
        for cell in cells {
            rows.push(Row {
                name: cell.name,
                status: cell.status,
                server: srv_name.clone(),
                repo: cell.repo,
                flow: cell.flow,
            });
        }
    }

    if rows.is_empty() {
        println!("  {}", dim("no cells"));
        return Ok(());
    }

    let current_repo = repo.and_then(|r| r.root().file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());

    let filter_repo = if show_all { None } else { current_repo.clone() };

    for row in &rows {
        // inside a repo (without --all): skip cells from other repos
        if let Some(ref cr) = filter_repo {
            if let Some(ref cell_repo) = row.repo {
                if cell_repo != cr {
                    continue;
                }
            }
        }

        let marker = if current_branch.as_deref() == Some(row.name.as_str()) {
            style("▶").cyan().to_string()
        } else {
            " ".to_string()
        };

        let flow_str = if let Some(f) = &row.flow {
            let elapsed = now.saturating_sub(f.started_at);
            if f.state == "running" || f.state == "paused" {
                format!("  {}  {}", f.flow_name, dim(&format_elapsed(elapsed)))
            } else {
                format!("  {}", f.flow_name)
            }
        } else {
            String::new()
        };

        let cell_label = match (row.repo.as_deref(), current_repo.as_deref(), show_all) {
            (Some(r), Some(cr), false) if r == cr => row.name.clone(),
            (Some(r), _, _) => format!("{}/{}", r, row.name),
            (None, _, _) => row.name.clone(),
        };

        println!("{} {:<24}{}  [{}]  {}",
            marker, bold(&cell_label), flow_str,
            vm_status(&row.status), dim(&row.server));
    }
    Ok(())
}

fn parse_ports(specs: &[String]) -> Result<Vec<u16>> {
    let mut ports = Vec::new();
    for spec in specs {
        if let Some((start, end)) = spec.split_once('-') {
            let s: u16 = start.parse().context(format!("invalid port: {start}"))?;
            let e: u16 = end.parse().context(format!("invalid port: {end}"))?;
            if s > e {
                anyhow::bail!("invalid range: {spec}");
            }
            ports.extend(s..=e);
        } else {
            ports.push(spec.parse().context(format!("invalid port: {spec}"))?);
        }
    }
    Ok(ports)
}

fn derive_loopback(vm_ip: &str) -> Option<String> {
    let parts: Vec<&str> = vm_ip.split('.').collect();
    if parts.len() == 4 {
        Some(format!("127.0.{}.{}", parts[2], parts[3]))
    } else {
        None
    }
}

fn setup_tunnel_dns(loopback: &str, dns: &str) -> bool {
    if !cfg!(unix) {
        return false;
    }

    let lo_ok = std::process::Command::new("sudo")
        .args(["ip", "addr", "add", &format!("{loopback}/8"), "dev", "lo"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !lo_ok {
        return false;
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| "cella".into());
    std::process::Command::new("sudo")
        .args([exe.as_os_str(), std::ffi::OsStr::new("util"), std::ffi::OsStr::new("hosts"), std::ffi::OsStr::new("add"), std::ffi::OsStr::new(loopback), std::ffi::OsStr::new(dns)])
        .status().ok();

    true
}

fn cleanup_tunnel_dns(loopback: &str, dns: &str) {
    if !cfg!(unix) {
        return;
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| "cella".into());
    std::process::Command::new("sudo")
        .args([exe.as_os_str(), std::ffi::OsStr::new("util"), std::ffi::OsStr::new("hosts"), std::ffi::OsStr::new("remove"), std::ffi::OsStr::new(dns)])
        .status().ok();

    std::process::Command::new("sudo")
        .args(["ip", "addr", "del", &format!("{loopback}/8"), "dev", "lo"])
        .output().ok();
}

fn cmd_tunnel(repo: &git::Repo, cell: &str, args: TunnelArgs) -> Result<()> {
    let cfg = config::load(repo.root())?;
    let cli_ports = if args.port.is_empty() { vec![] } else { parse_ports(&args.port)? };
    let ports = if cli_ports.is_empty() { &cfg.ports } else { &cli_ports };
    if ports.is_empty() {
        anyhow::bail!("no ports specified — use -p 5173 or add ports = [5173] to .cella/config.toml");
    }

    let active = find_cell_server(repo, cell)?;
    let target = active.target()?
        .ok_or_else(|| anyhow::anyhow!("tunnel is for remote hosts — ports are already accessible locally"))?;

    let c = connect_remote(&active)?;
    let cells = c.list()?;
    let found = cells.iter().find(|c| c.name == cell)
        .ok_or_else(|| anyhow::anyhow!("cell '{cell}' not found on server"))?;
    let vm_ip = found.ip.as_deref()
        .ok_or_else(|| anyhow::anyhow!("cell '{cell}' has no IP (not running?)"))?;

    let loopback = derive_loopback(vm_ip)
        .ok_or_else(|| anyhow::anyhow!("cannot derive loopback from IP '{vm_ip}'"))?;

    let repo_name = repo.root()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let dns = vm::dns_hostname(cell, repo_name);

    let has_dns = setup_tunnel_dns(&loopback, &dns);
    let bind_addr = if has_dns { &loopback } else { "127.0.0.1" };

    let mut cmd = std::process::Command::new("ssh");
    cmd.args(["-N", "-o", "LogLevel=ERROR"]);
    for port in ports {
        cmd.arg("-L").arg(format!("{bind_addr}:{port}:{dns}:{port}"));
    }
    cmd.arg(&target);

    let url = if has_dns {
        format!("http://{dns}:{}", ports[0])
    } else {
        format!("http://127.0.0.1:{}", ports[0])
    };
    let ports_str: Vec<String> = ports.iter().map(|p| p.to_string()).collect();
    println!("{} tunneling {} port {} → {url}", style("⇄").cyan(), bold(cell), ports_str.join(", "));
    if !has_dns {
        println!("  {}", dim(".cell DNS not available — using localhost"));
    }
    println!("  {}", dim("press ctrl+c to close"));

    if args.open {
        std::process::Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .ok();
    }

    loop {
        let status = cmd.status().context("ssh tunnel failed")?;

        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if status.success() || status.signal().is_some() {
                break;
            }
        }
        #[cfg(not(unix))]
        if status.success() {
            break;
        }

        std::thread::sleep(std::time::Duration::from_secs(3));
        cmd = std::process::Command::new("ssh");
        cmd.args(["-N", "-o", "LogLevel=ERROR"]);
        for port in ports {
            cmd.arg("-L").arg(format!("{loopback}:{port}:{dns}:{port}"));
        }
        cmd.arg(&target);
    }

    if has_dns {
        cleanup_tunnel_dns(&loopback, &dns);
    }
    Ok(())
}

const HOSTS_MARKER: &str = "# cella-tunnel";

fn cmd_hosts(action: HostsAction) -> Result<()> {
    match action {
        HostsAction::Add { ip, hostname } => {
            let hosts = std::fs::read_to_string("/etc/hosts").unwrap_or_default();
            if !hosts.contains(&hostname) {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().append(true).open("/etc/hosts")
                    .context("cannot write /etc/hosts (are you root?)")?;
                writeln!(f, "{ip} {hostname} {HOSTS_MARKER}")?;
            }
        }
        HostsAction::Remove { hostname } => {
            let hosts = std::fs::read_to_string("/etc/hosts")
                .context("cannot read /etc/hosts")?;
            let updated: String = hosts
                .lines()
                .filter(|line| !(line.contains(&hostname) && line.contains(HOSTS_MARKER)))
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write("/etc/hosts", format!("{updated}\n"))
                .context("cannot write /etc/hosts (are you root?)")?;
        }
    }
    Ok(())
}

fn cmd_logs(repo: &git::Repo, cell: &str, args: LogsArgs) -> Result<()> {
    let active = find_cell_server(repo, cell)?;
    let t = make_transport(&active)?;
    t.flow_logs(cell, args.follow)
}

fn cmd_secrets(repo: &git::Repo, args: SecretsArgs) -> Result<()> {
    let cfg = config::load(repo.root())?;
    let resolve_recipient = |arg: Option<String>| -> Result<String> {
        arg.or(cfg.secrets.recipient.clone())
            .ok_or_else(|| anyhow::anyhow!(
                "no recipient — pass -r or set secrets.recipient in config"
            ))
    };
    match args.action {
        SecretsAction::Encrypt { recipient } => {
            secrets::encrypt(repo.root(), &resolve_recipient(recipient)?)
        }
        SecretsAction::Edit { recipient } => {
            secrets::edit(repo.root(), &resolve_recipient(recipient)?)
        }
    }
}

// Flow commands

#[instrument(skip(repo))]
fn cmd_run(repo: &git::Repo, cell: &str, args: RunArgs) -> Result<()> {
    let cfg = config::load(repo.root())?;

    let client_cfg = server::load_client_config();
    let srv_name = args.server.as_ref()
        .or(cfg.server.as_ref())
        .or(client_cfg.server.as_ref());

    let active = if let Some(srv) = srv_name {
        server::resolve(srv)?
    } else {
        find_cell_server(repo, cell)?
    };
    let t = make_transport(&active)?;
    t.ensure_running(cell, repo, &cfg)?;

    let flow_name = if let Some(ref name) = args.name {
        name.clone()
    } else {
        let status_path = if active.is_server() {
            None
        } else {
            Some(crate::cell::cell_dir(cell).join("flow-status.json"))
        };

        let paused_flow = status_path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| {
                if v["state"].as_str() == Some("paused") {
                    v["flow_name"].as_str().map(|s| s.to_string())
                } else {
                    None
                }
            });

        match paused_flow {
            Some(name) => name,
            None => anyhow::bail!("specify a flow name: cella run <flow>"),
        }
    };

    let params_json = if args.params.is_empty() {
        None
    } else {
        let mut map = serde_json::Map::new();
        for kv in &args.params {
            if let Some((k, v)) = kv.split_once('=') {
                map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
            } else {
                anyhow::bail!("invalid param '{}' — expected key=value", kv);
            }
        }
        Some(serde_json::Value::Object(map).to_string())
    };
    t.flow_start(cell, &flow_name, params_json.as_deref())?;
    println!("{} flow {} on {}", arrow(), bold(&flow_name), bold(cell));
    if args.detach {
        println!("  {} cella logs -f", dim("follow:"));
    } else {
        t.flow_logs(cell, true)?;
    }
    Ok(())
}

fn cmd_pause(repo: &git::Repo, cell: &str) -> Result<()> {
    let active = find_cell_server(repo, cell)?;
    let t = make_transport(&active)?;
    t.flow_pause(cell)?;
    println!("{} pausing {}", arrow(), bold(cell));
    Ok(())
}

fn cmd_status(repo: &git::Repo, cell: Option<&str>) -> Result<()> {
    let now = flow::now_secs();

    if let Some(name) = cell {
        let active = find_cell_server(repo, name)?;

        if active.is_server() {
            let c = connect_remote(&active)?;
            let cells = c.list()?;
            if let Some(cell) = cells.iter().find(|c| c.name == name) {
                println!("cell: {}", bold(name));
                println!("vm: {}", vm_status(&cell.status));
                if let Some(ref f) = cell.flow {
                    println!("flow: {}", f.flow_name);
                    println!("state: {}", flow_state(&f.state));
                    println!("op: {}", f.current_op);
                    let elapsed = now.saturating_sub(f.started_at);
                    if elapsed > 0 {
                        println!("elapsed: {}", format_elapsed(elapsed));
                    }
                } else {
                    println!("flow: {}", dim("none"));
                }
            } else {
                anyhow::bail!("cell '{name}' not found");
            }
        } else {
            let running = vm::is_running(name)?;
            println!("cell: {}", bold(name));
            println!("vm: {}", vm_status(if running { "running" } else { "stopped" }));

            let flow: Option<client::FlowInfo> = {
                let path = crate::cell::cell_dir(name).join("flow-status.json");
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
            };
            if let Some(f) = flow {
                println!("flow: {}", f.flow_name);
                println!("state: {}", flow_state(&f.state));
                println!("op: {}", f.current_op);
                let elapsed = now.saturating_sub(f.started_at);
                if elapsed > 0 {
                    println!("elapsed: {}", format_elapsed(elapsed));
                }
            } else {
                println!("flow: {}", dim("none"));
            }
        }
    } else {
        cmd_list(Some(repo), false)?;
    }
    Ok(())
}
