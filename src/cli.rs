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
    /// Attach to a cell (creates branch + boots if needed)
    Shell(ShellArgs),
    /// Force stop a cell (or stop + delete with -d)
    Kill(KillArgs),
    /// List cells and their status
    List,
    /// Manage servers (registry of remote and local hosts)
    Server(ServerArgs),
    /// Deploy server config from current directory
    Deploy,
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
    /// View cella logs
    Logs(LogsArgs),
    /// Manage encrypted secrets
    Secrets(SecretsArgs),
    #[command(hide = true)]
    /// Run the network proxy (used by systemd)
    Proxy(ProxyArgs),
    #[command(hide = true)]
    /// Print the cell repo path for the current VM (used by git-remote-cella)
    ResolveCell {
        /// Match cells for this repo name
        #[arg(long)]
        repo: Option<String>,
    },
    #[command(hide = true)]
    /// Check all cells and auto-stop idle ones (called by systemd timer)
    Sweep,
    #[command(hide = true)]
    /// Manage /etc/hosts entries for tunnels
    Hosts {
        #[command(subcommand)]
        action: HostsAction,
    },
}

#[derive(Args)]
struct ShellArgs {
    /// Cell name
    name: String,
    /// Named session (reattachable with ctrl+])
    #[arg(short, long)]
    session: Option<String>,
    /// Run a command instead of interactive shell
    #[arg(short = 'c', long = "command")]
    command: Option<String>,
    /// Server-side mode (skip repo checks)
    #[arg(long, hide = true)]
    server: bool,
}

#[derive(Args, Debug)]
struct KillArgs {
    /// Cell name
    name: String,
    /// Also delete the cell and its branch
    #[arg(short, long)]
    delete: bool,
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
    /// Switch active server for this repo
    Use {
        /// Server name (or "localhost")
        name: String,
    },
    /// Show active server
    Status,
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
        /// Recipient public key or SSH public key
        #[arg(short, long)]
        recipient: String,
    },
    /// Decrypt, open in $EDITOR, re-encrypt on save
    Edit {
        /// Recipient public key or SSH public key
        #[arg(short, long)]
        recipient: String,
    },
}

#[derive(Args)]
struct LogsArgs {
    /// Follow log output
    #[arg(short, long)]
    follow: bool,
    /// Show server logs (via SSH)
    #[arg(long)]
    server: bool,
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
fn dot() -> console::StyledObject<&'static str> { style("●").green() }
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

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Proxy(args) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(proxy::run(&args.config))
        }
        Commands::ResolveCell { repo } => cmd_resolve_cell(repo.as_deref()),
        Commands::Sweep => cmd_sweep(),
        Commands::Hosts { action } => cmd_hosts(action),
        Commands::Shell(args) => cmd_shell(args),
        Commands::Logs(args) => cmd_logs(args),
        Commands::List => cmd_list(git::Repo::open().ok().as_ref()),
        Commands::Deploy => cmd_deploy(),
        Commands::Server(args) => match args.action {
            ServerAction::Add { name, target } => cmd_server_add(&name, &target),
            ServerAction::Remove { name } => cmd_server_remove(&name),
            ServerAction::List => cmd_server_list_global(),
            ServerAction::Use { name } => {
                let repo = git::Repo::open()?;
                cmd_server_use(&repo, &name)
            },
            ServerAction::Status => {
                let repo = git::Repo::open()?;
                cmd_server_status(&repo)
            },
        },
        cmd => {
            let repo = git::Repo::open()?;
            match cmd {
                Commands::Init => cmd_init(&repo),
                Commands::Kill(args) => cmd_kill(&repo, args),
                Commands::Tunnel { name, port, open } => {
                    let ports = if port.is_empty() { vec![] } else { parse_ports(&port)? };
                    cmd_tunnel(&repo, &name, &ports, open)
                },
                Commands::Secrets(args) => cmd_secrets(&repo, args),
                _ => unreachable!(),
            }
        }
    }
}

// Helpers

fn get_server(repo: &git::Repo) -> Result<server::ActiveServer> {
    server::active(repo.root())
}

fn connect_remote(s: &server::ActiveServer) -> Result<client::Client> {
    let target = s.target()?
        .ok_or_else(|| anyhow::anyhow!("server has no target"))?;
    client::Client::connect(&target)
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
    server::write_active(repo.root(), &server::ActiveServer::Localhost)?;
    println!("{} initialized cella {}", ok(), dim("(localhost)"));
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

fn cmd_server_list_global() -> Result<()> {
    let servers = server::list()?;
    let hdr = Style::new().bold();
    println!("  {:<20} {}", hdr.apply_to("SERVER"), hdr.apply_to("TARGET"));
    println!("  {:<20} {}", "localhost", dim("—"));
    for (name, target) in &servers {
        println!("  {:<20} {}", name, dim(target));
    }
    Ok(())
}

fn cmd_server_use(repo: &git::Repo, name: &str) -> Result<()> {
    let active = server::resolve(name)?;
    server::write_active(repo.root(), &active)?;

    let url = active.remote_url()?;
    repo.add_cella_remote(&url)?;

    match &active {
        server::ActiveServer::Localhost => {
            println!("{} switched to {}", dot(), bold("localhost"));
        }
        server::ActiveServer::Remote { name } => {
            let target = active.target()?.unwrap_or_default();
            println!("{} switched to {} {}", dot(), bold(name), dim(&format!("({target})")));
        }
    }
    Ok(())
}

fn cmd_server_status(repo: &git::Repo) -> Result<()> {
    let active = get_server(repo)?;
    match &active {
        server::ActiveServer::Localhost => {
            println!("{} {} localhost", dot(), bold("server"));
        }
        server::ActiveServer::Remote { name } => {
            println!("{} {} {}", dot(), bold("server"), name);
            if let Ok(Some(target)) = active.target() {
                println!("  {} {}", bold("target"), target);
            }
        }
    }
    Ok(())
}

fn cmd_deploy() -> Result<()> {
    deploy::run()
}

// Cell commands

fn cmd_resolve_cell(repo_filter: Option<&str>) -> Result<()> {
    // server-side: scan cells for a running one matching the repo name
    if let Ok(cells) = vm::list_cells() {
        for name in &cells {
            if !vm::is_running(name).unwrap_or(false) {
                continue;
            }
            // if a repo filter is given, check the runtime repo file
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

    // client-side: resolve via repo
    let repo = git::Repo::open()?;
    let path = repo.resolve_cell_path()?;
    println!("{}", path.display());
    Ok(())
}

#[instrument(skip(repo), fields(cell = %args.name))]
fn cmd_kill(repo: &git::Repo, args: KillArgs) -> Result<()> {
    let active = get_server(repo)?;

    if active.is_server() {
        let c = connect_remote(&active)?;
        if args.delete {
            c.delete(&args.name)?;
            repo.delete_branch(&args.name).ok();
            println!("{} deleted {}", rm(), bold(&args.name));
        } else {
            c.down(&args.name)?;
            println!("{} killed {}", dn(), bold(&args.name));
        }
        return Ok(());
    }

    if args.delete {
        if vm::is_running(&args.name)? {
            vm::stop(&args.name)?;
        }
        vm::clear_autostop(&args.name);
        repo.remove_clone(&args.name)?;
        repo.delete_branch(&args.name).ok();
        println!("{} deleted {}", rm(), bold(&args.name));
    } else {
        if vm::is_running(&args.name)? {
            vm::clear_autostop(&args.name);
            vm::stop(&args.name)?;
            println!("{} killed {}", dn(), bold(&args.name));
        } else {
            println!("  {} not running", bold(&args.name));
        }
    }
    Ok(())
}

#[instrument(skip_all, fields(cell = %args.name))]
fn cmd_shell(args: ShellArgs) -> Result<()> {
    // server-side direct path (called via SSH from client)
    if args.server {
        return vm::shell(&args.name, args.session.as_deref(), args.command.as_deref(), None);
    }

    if let Ok(repo) = git::Repo::open() {
        let active = get_server(&repo)?;
        let cfg = config::load(repo.root())?;

        let t: Box<dyn Transport> = if active.is_server() {
            let c = connect_remote(&active)?;
            Box::new(transport::RemoteTransport { client: c })
        } else {
            Box::new(transport::LocalTransport)
        };

        t.ensure_running(&args.name, &repo, &cfg)?;

        println!("{} entering {}", arrow(), bold(&args.name));
        return t.shell(&args.name, args.session.as_deref(), args.command.as_deref(), &cfg.session);
    }

    // no repo context — bare shell (e.g. server-side)
    vm::shell(&args.name, args.session.as_deref(), args.command.as_deref(), None)
}

fn cmd_list(repo: Option<&git::Repo>) -> Result<()> {
    let mut servers = vec![("localhost".to_string(), None)];
    for (name, target) in server::list()? {
        servers.push((name, Some(target)));
    }

    let active_name = repo
        .and_then(|r| get_server(r).ok())
        .map(|a| match a {
            server::ActiveServer::Localhost => "localhost".to_string(),
            server::ActiveServer::Remote { name } => name,
        })
        .unwrap_or_default();

    let mut any = false;

    for (srv_name, target) in &servers {
        if let Some(target) = target {
            let c = match client::Client::connect(target) {
                Ok(c) => c,
                Err(_) => {
                    println!("  {} {}", dim(&format!("{srv_name}:")), dim("unreachable"));
                    continue;
                }
            };
            let cells = c.list().unwrap_or_default();
            if cells.is_empty() {
                continue;
            }
            any = true;
            let marker = if *srv_name == active_name { " ●" } else { "" };
            println!("  {}:{}", bold(srv_name), style(marker).green());
            for cell in &cells {
                let ip = cell.ip.as_deref().unwrap_or("");
                if cell.status == "running" {
                    println!("    {} [{}] {}", bold(&cell.name), vm_status(&cell.status), dim(ip));
                    let inner = r#"tmux list-sessions -F '#{session_name}|#{session_attached}|#{session_windows}' 2>/dev/null"#;
                    let tmux_cmd = format!("cella shell --server {} -c \"{}\"", cell.name, inner);
                    if let Ok(output) = c.exec(&tmux_cmd, 10) {
                        let sessions: Vec<&str> = output.lines().filter(|l| !l.is_empty()).collect();
                        if sessions.is_empty() {
                            println!("      {} {}", dim("╰"), dim("no sessions"));
                        }
                        for line in sessions {
                            let parts: Vec<&str> = line.split('|').collect();
                            if parts.len() >= 3 {
                                let sname = parts[0];
                                let attached = parts[1] == "1";
                                let wins: usize = parts[2].parse().unwrap_or(1);
                                let status_str = if attached {
                                    style("attached").green().to_string()
                                } else {
                                    style("detached").yellow().to_string()
                                };
                                let win_str = if wins > 1 { format!(" {}w", wins) } else { String::new() };
                                println!("      {} {}{}", dim("╰"), style(sname).cyan(), dim(&format!(" {status_str}{win_str}")));
                            }
                        }
                    }
                } else {
                    println!("    {} [{}]", bold(&cell.name), vm_status(&cell.status));
                }
            }
        } else {
            let names = if let Some(repo) = repo {
                repo.list_clones().unwrap_or_default()
            } else {
                vm::list_cells().unwrap_or_default()
            };
            if names.is_empty() {
                continue;
            }
            any = true;
            let marker = if active_name == "localhost" { " ●" } else { "" };
            println!("  {}:{}", bold("localhost"), style(marker).green());
            for name in &names {
                let running = vm::is_running(name)?;
                let st = if running { "running" } else { "stopped" };
                print_cell_row(name, st, running)?;
            }
        }
    }

    if !any {
        println!("  {}", dim("no cells"));
    }
    Ok(())
}

fn print_cell_row(name: &str, st: &str, running: bool) -> Result<()> {
    if running {
        let mut cell_info = format!("    {} [{}]", bold(name), vm_status(st));
        if let Some(secs) = vm::autostop_remaining(name) {
            cell_info.push_str(&format!(" {}", dim(&format!("stopping in {}s", secs))));
        }
        println!("{cell_info}");

        if let Ok(sessions) = vm::list_sessions(name) {
            for s in &sessions {
                let status = if s.attached {
                    style("attached").green().to_string()
                } else {
                    style("detached").yellow().to_string()
                };
                let wins = if s.windows > 1 {
                    format!(" {}w", s.windows)
                } else {
                    String::new()
                };
                println!("      {} {}{}", dim("╰"), style(&s.name).cyan(), dim(&format!(" {status}{wins}")));
            }
            if sessions.is_empty() {
                println!("      {} {}", dim("╰"), dim("no sessions"));
            }
        }
    } else {
        println!("    {} [{}]", bold(name), vm_status(st));
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
    // 192.168.83.X → 127.0.83.X
    let parts: Vec<&str> = vm_ip.split('.').collect();
    if parts.len() == 4 {
        Some(format!("127.0.{}.{}", parts[2], parts[3]))
    } else {
        None
    }
}

/// Returns true if loopback alias + /etc/hosts setup succeeded
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
        .args([exe.as_os_str(), std::ffi::OsStr::new("hosts"), std::ffi::OsStr::new("add"), std::ffi::OsStr::new(loopback), std::ffi::OsStr::new(dns)])
        .status().ok();

    true
}

fn cleanup_tunnel_dns(loopback: &str, dns: &str) {
    if !cfg!(unix) {
        return;
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| "cella".into());
    std::process::Command::new("sudo")
        .args([exe.as_os_str(), std::ffi::OsStr::new("hosts"), std::ffi::OsStr::new("remove"), std::ffi::OsStr::new(dns)])
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

    let active = get_server(repo)?;
    let target = active.target()?
        .ok_or_else(|| anyhow::anyhow!("tunnel is for remote hosts — ports are already accessible locally"))?;

    // get cell IP from server
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

    // set up local DNS + loopback (graceful degradation on non-unix)
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

        // ctrl+c or clean exit — stop retrying
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

        // connection dropped — retry
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

fn cmd_logs(args: LogsArgs) -> Result<()> {
    if args.server {
        let repo = git::Repo::open()?;
        let active = get_server(&repo)?;
        let target = active.target()?
            .ok_or_else(|| anyhow::anyhow!("logs --server requires a remote server"))?;
        let tail_cmd = if args.follow { "tail -f" } else { "tail -100" };
        let status = std::process::Command::new("ssh")
            .args([&target, tail_cmd, "/var/log/cella/cella.log"])
            .status()
            .context("ssh failed")?;
        if !status.success() {
            anyhow::bail!("failed to read server logs");
        }
        return Ok(());
    }

    let log_path = crate::log::log_file_path();
    if !log_path.exists() {
        println!("  {}", dim("no logs yet"));
        return Ok(());
    }

    let cmd = if args.follow { "tail" } else { "tail" };
    let mut tail_args = vec!["-100"];
    if args.follow {
        tail_args.push("-f");
    }
    tail_args.push(log_path.to_str().unwrap_or(""));

    let status = std::process::Command::new(cmd)
        .args(&tail_args)
        .status()
        .context("tail failed")?;
    if !status.success() {
        anyhow::bail!("failed to read logs");
    }
    Ok(())
}

fn cmd_sweep() -> Result<()> {
    let actions = vm::sweep()?;
    for (name, action) in &actions {
        match action {
            vm::SweepAction::Scheduled(t) => {
                eprintln!("  {} {} idle — stopping in {}s", dim("⏲"), bold(name), t);
            }
            vm::SweepAction::Waiting(t) => {
                eprintln!("  {} {} stopping in {}s", dim("⏲"), bold(name), t);
            }
            vm::SweepAction::Stopped => {
                eprintln!("  {} {} stopped (idle)", dn(), bold(name));
            }
            vm::SweepAction::Cancelled => {
                eprintln!("  {} {} autostop cancelled (sessions active)", ok(), bold(name));
            }
        }
    }
    Ok(())
}

fn cmd_secrets(repo: &git::Repo, args: SecretsArgs) -> Result<()> {
    match args.action {
        SecretsAction::Encrypt { recipient } => {
            secrets::encrypt(repo.root(), &recipient)
        }
        SecretsAction::Edit { recipient } => {
            secrets::edit(repo.root(), &recipient)
        }
    }
}

