use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use console::{style, Style};

use tracing::instrument;
use crate::{client, config, deploy, git, server, proxy, secrets, transport, vm};
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
    /// Start or resume a flow
    Run(RunArgs),
    /// Pause the running flow (cell stays up)
    Pause {
        /// Cell name
        cell: String,
    },
    /// Stop flow and shut down cell
    Stop(StopArgs),
    /// Show flow status
    Status {
        /// Cell name (omit for all active flows)
        cell: Option<String>,
    },
    /// SSH into a cell
    Shell(ShellArgs),
    /// List cells and their status
    List {
        /// Show cells from all repos
        #[arg(short, long)]
        all: bool,
    },
    /// View flow output from a cell
    Logs(LogsArgs),
    /// Forward declared ports from a remote cell to localhost
    Tunnel {
        /// Cell name
        name: String,
        /// Ports to forward, supports ranges (e.g. -p 5173 -p 8001-8004)
        #[arg(short, long)]
        port: Vec<String>,
        /// Open in default browser
        #[arg(short, long)]
        open: bool,
    },
    /// Manage encrypted secrets
    Secrets(SecretsArgs),
    /// Manage servers
    Server(ServerArgs),
    #[command(hide = true)]
    /// Internal utilities
    Util(UtilArgs),
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Flow name (matches .cella/flows/{name}/)
    name: Option<String>,
    /// Cell name (defaults to current branch)
    #[arg(short, long)]
    cell: Option<String>,
    /// Server to run on
    #[arg(short, long)]
    server: Option<String>,
    /// Attach to flow output (default is detached)
    #[arg(short, long)]
    attach: bool,
    /// Params as key=value pairs (e.g. cella run dev -- project="my project")
    #[arg(last = true)]
    params: Vec<String>,
}

#[derive(Args, Debug)]
struct StopArgs {
    /// Cell name
    cell: String,
    /// Also delete the cell and its branch
    #[arg(short, long)]
    delete: bool,
}

#[derive(Args)]
struct ShellArgs {
    /// Cell name
    name: String,
    /// Run a command instead of interactive shell
    #[arg(short = 'c', long = "command")]
    command: Option<String>,
    /// Server-side mode (skip repo checks)
    #[arg(long, hide = true)]
    server: bool,
}

#[derive(Args)]
struct LogsArgs {
    /// Cell name
    cell: String,
    /// Follow log output
    #[arg(short, long)]
    follow: bool,
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
    #[command(hide = true)]
    /// Run the network proxy (used by systemd)
    Proxy(ProxyArgs),
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
        },
        Commands::Util(args) => match args.action {
            UtilAction::ResolveCell { repo } => cmd_resolve_cell(repo.as_deref()),
            UtilAction::Hosts { action } => cmd_hosts(action),
        },
        Commands::Shell(args) => cmd_shell(args),
        Commands::List { all } => cmd_list(git::Repo::open().ok().as_ref(), all),
        cmd => {
            let repo = git::Repo::open()?;
            match cmd {
                Commands::Init => cmd_init(&repo),
                Commands::Run(args) => cmd_run(&repo, args),
                Commands::Pause { cell } => cmd_pause(&repo, &cell),
                Commands::Stop(args) => cmd_stop(&repo, args),
                Commands::Status { cell } => cmd_status(&repo, cell.as_deref()),
                Commands::Logs(args) => cmd_logs(&repo, args),
                Commands::Tunnel { name, port, open } => {
                    let ports = if port.is_empty() { vec![] } else { parse_ports(&port)? };
                    cmd_tunnel(&repo, &name, &ports, open)
                }
                Commands::Secrets(args) => cmd_secrets(&repo, args),
                _ => unreachable!(),
            }
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

    repo.add_cella_remote("cella://localhost")?;
    println!("{} initialized cella", ok());
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

#[instrument(skip(repo), fields(cell = %args.cell))]
fn cmd_stop(repo: &git::Repo, args: StopArgs) -> Result<()> {
    let active = find_cell_server(repo, &args.cell)?;
    let t = make_transport(&active)?;

    t.flow_stop(&args.cell).ok();

    if active.is_server() {
        let c = connect_remote(&active)?;
        if args.delete {
            c.delete(&args.cell)?;
            repo.delete_branch(&args.cell).ok();
            repo.remove_clone(&args.cell).ok();
            println!("{} deleted {}", rm(), bold(&args.cell));
        } else {
            c.down(&args.cell)?;
            println!("{} stopped {}", dn(), bold(&args.cell));
        }
        return Ok(());
    }

    if args.delete {
        if vm::is_running(&args.cell)? {
            vm::stop(&args.cell)?;
        }
        repo.remove_clone(&args.cell)?;
        repo.delete_branch(&args.cell).ok();
        println!("{} deleted {}", rm(), bold(&args.cell));
    } else {
        if vm::is_running(&args.cell)? {
            vm::stop(&args.cell)?;
            println!("{} stopped {}", dn(), bold(&args.cell));
        } else {
            println!("  {} not running", bold(&args.cell));
        }
    }
    Ok(())
}

#[instrument(skip_all, fields(cell = %args.name))]
fn cmd_shell(args: ShellArgs) -> Result<()> {
    if args.server {
        return vm::shell(&args.name, args.command.as_deref());
    }

    if let Ok(repo) = git::Repo::open() {
        let active = find_cell_server(&repo, &args.name)?;
        let cfg = config::load(repo.root())?;
        let t = make_transport(&active)?;
        t.ensure_running(&args.name, &repo, &cfg)?;

        println!("{} entering {}", arrow(), bold(&args.name));
        return t.shell(&args.name, args.command.as_deref());
    }

    vm::shell(&args.name, args.command.as_deref())
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

fn cmd_tunnel(repo: &git::Repo, name: &str, cli_ports: &[u16], open: bool) -> Result<()> {
    let cfg = config::load(repo.root())?;
    let ports = if cli_ports.is_empty() { &cfg.ports } else { cli_ports };
    if ports.is_empty() {
        anyhow::bail!("no ports specified — use -p 5173 or add ports = [5173] to .cella/config.toml");
    }

    let active = find_cell_server(repo, name)?;
    let target = active.target()?
        .ok_or_else(|| anyhow::anyhow!("tunnel is for remote hosts — ports are already accessible locally"))?;

    let c = connect_remote(&active)?;
    let cells = c.list()?;
    let cell = cells.iter().find(|c| c.name == name)
        .ok_or_else(|| anyhow::anyhow!("cell '{name}' not found on server"))?;
    let vm_ip = cell.ip.as_deref()
        .ok_or_else(|| anyhow::anyhow!("cell '{name}' has no IP (not running?)"))?;

    let loopback = derive_loopback(vm_ip)
        .ok_or_else(|| anyhow::anyhow!("cannot derive loopback from IP '{vm_ip}'"))?;

    let repo_name = repo.root()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let dns = vm::dns_hostname(name, repo_name);

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
    println!("{} tunneling {} port {} → {url}", style("⇄").cyan(), bold(name), ports_str.join(", "));
    if !has_dns {
        println!("  {}", dim(".cell DNS not available — using localhost"));
    }
    println!("  {}", dim("press ctrl+c to close"));

    if open {
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

fn cmd_logs(repo: &git::Repo, args: LogsArgs) -> Result<()> {
    let active = find_cell_server(repo, &args.cell)?;
    let t = make_transport(&active)?;
    t.flow_logs(&args.cell, args.follow)
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
fn cmd_run(repo: &git::Repo, args: RunArgs) -> Result<()> {
    let branch = repo.current_branch()?;
    let cell = args.cell.as_deref().unwrap_or(&branch);
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

    // determine flow name: explicit, or resume from paused state, or default
    let flow_name = if let Some(ref name) = args.name {
        name.clone()
    } else {
        // check for paused flow to resume
        let status_path = if active.is_server() {
            // for remote, we'd need to query — for now require explicit name
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
    if args.attach {
        t.flow_logs(cell, true)?;
    } else {
        println!("  {} cella logs {} -f", dim("follow:"), cell);
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
        // show all active flows
        cmd_list(Some(repo), false)?;
    }
    Ok(())
}
