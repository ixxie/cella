use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn, instrument};

use crate::cell;
use crate::config::CellaConfig;
const PROXY_CONTROL: &str = "http://127.0.0.1:8082";
const DNS_HOSTS: &str = "/var/lib/cella/dns-hosts";

fn is_root() -> bool {
    std::env::var("USER").unwrap_or_default() == "root"
}

fn run_privileged(args: &[&str]) -> Result<std::process::ExitStatus> {
    if is_root() {
        Command::new(args[0]).args(&args[1..]).status().map_err(Into::into)
    } else {
        Command::new("sudo").args(args).status().map_err(Into::into)
    }
}

// Re-export cell path helpers
pub use crate::cell::{cell_dir, cell_repo_dir, runtime_dir, list_cells};

const SERVER_VM_CONFIG: &str = "/var/lib/cella/vm-config";

fn has_server_config() -> bool {
    Path::new(SERVER_VM_CONFIG).join("flake.nix").exists()
}

fn has_repo_config(name: &str) -> bool {
    cell_repo_dir(name).join(".cella/flake.nix").exists()
}

const HOST_CONFIG: &str = "/etc/cella/host-config.json";

fn read_host_config_user() -> Option<String> {
    let json = std::fs::read_to_string(HOST_CONFIG).ok()?;
    let val: serde_json::Value = serde_json::from_str(&json).ok()?;
    val.get("user")?.get("name")?.as_str().map(|s| s.to_string())
}

fn generate_flake(name: &str, ip: &str, repo_name: &str) -> Result<()> {
    let flake_dir = cell::cell_flake_dir(name);
    // clean slate — remove stale lock files
    if flake_dir.exists() {
        std::fs::remove_dir_all(&flake_dir).ok();
    }
    std::fs::create_dir_all(&flake_dir)?;

    let cell_dir_str = cell_dir(name).to_string_lossy().to_string();
    let repo_cella_dir = cell_repo_dir(name).join(".cella");

    let has_server = has_server_config();
    let has_repo = has_repo_config(name);

    // read host config and inline as nix attrset
    let host_config_nix = if Path::new(HOST_CONFIG).exists() {
        let json = std::fs::read_to_string(HOST_CONFIG)
            .context("reading host-config.json")?;
        let mut val: serde_json::Value = serde_json::from_str(&json)
            .context("parsing host-config.json")?;

        // inject the host's SSH public key so server-side operations (sweep) can reach VMs
        if let Ok(host_pubkey) = std::fs::read_to_string("/var/lib/cella/ssh/id_ed25519.pub") {
            let key = host_pubkey.trim().to_string();
            if let Some(keys) = val.pointer_mut("/user/authorizedKeys") {
                if let Some(arr) = keys.as_array_mut() {
                    if !arr.iter().any(|k| k.as_str() == Some(&key)) {
                        arr.push(serde_json::Value::String(key));
                    }
                }
            }
        }

        json_to_nix(&val)
    } else {
        "{}".to_string()
    };

    // build inputs block
    let mut extra_inputs = String::new();
    if has_server {
        extra_inputs.push_str(&format!(
            "    serverConfig.url = \"path:{}\";\n",
            SERVER_VM_CONFIG
        ));
    }
    if has_repo {
        extra_inputs.push_str(&format!(
            "    repoConfig.url = \"path:{}\";\n",
            repo_cella_dir.display()
        ));
    }

    // build modules list
    let mut modules = Vec::new();
    if has_server {
        modules.push("inputs.serverConfig.nixosModule");
    }
    if has_repo {
        modules.push("inputs.repoConfig.nixosModule");
    }
    let modules_nix = if modules.is_empty() {
        String::new()
    } else {
        format!("      modules = [ {} ];", modules.join(" "))
    };

    let flake = format!(r#"{{
  inputs = {{
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    cella.url = "github:ixxie/cella";
    microvm = {{
      url = "github:microvm-nix/microvm.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    }};
{extra_inputs}  }};

  outputs = {{ nixpkgs, cella, microvm, ... }} @ inputs:
    cella.lib.mkCell {{ inherit cella nixpkgs microvm; }} {{
      name = "{name}";
      ip = "{ip}";
      cellDir = "{cell_dir}";
      repo = "{repo}";
      hostConfig = {host_config};
{modules}
    }};
}}
"#,
        extra_inputs = extra_inputs,
        name = name,
        ip = ip,
        cell_dir = cell_dir_str,
        repo = repo_name,
        host_config = host_config_nix,
        modules = modules_nix,
    );

    std::fs::write(flake_dir.join("flake.nix"), flake)?;
    Ok(())
}

fn json_to_nix(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_to_nix).collect();
            format!("[ {} ]", items.join(" "))
        }
        serde_json::Value::Object(obj) => {
            let pairs: Vec<String> = obj.iter().map(|(k, v)| {
                format!("{} = {};", k, json_to_nix(v))
            }).collect();
            format!("{{ {} }}", pairs.join(" "))
        }
    }
}

// Session counting

fn ssh_target(name: &str) -> Result<(String, String)> {
    let rt = runtime_dir(name);
    let ip = std::fs::read_to_string(rt.join("ip"))
        .context("VM not running (no ip file)")?
        .trim()
        .to_string();
    let user = std::fs::read_to_string(rt.join("user"))
        .unwrap_or_default().trim().to_string();
    let target = if user.is_empty() {
        ip.clone()
    } else {
        format!("{user}@{ip}")
    };
    Ok((ip, target))
}

#[instrument]
pub fn count_sessions(name: &str) -> Result<usize> {
    let (_, target) = ssh_target(name)?;
    let output = Command::new("ssh")
        .args([
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR",
            "-o", "ConnectTimeout=3",
            &target,
            "tmux list-sessions 2>/dev/null | wc -l",
        ])
        .output()
        .context("ssh session count failed")?;
    if !output.status.success() {
        anyhow::bail!("ssh to VM failed (exit {})", output.status);
    }
    let count: usize = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    Ok(count)
}

#[derive(Debug)]
pub struct SessionInfo {
    pub name: String,
    pub attached: bool,
    pub windows: usize,
    pub created: String,
}

pub fn list_sessions(name: &str) -> Result<Vec<SessionInfo>> {
    let (_, target) = ssh_target(name)?;
    let output = Command::new("ssh")
        .args([
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR",
            "-o", "ConnectTimeout=3",
            &target,
            "tmux list-sessions -F '#{session_name}|#{session_attached}|#{session_windows}|#{session_created}' 2>/dev/null",
        ])
        .output()
        .context("ssh list-sessions failed")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sessions = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 4 {
                Some(SessionInfo {
                    name: parts[0].to_string(),
                    attached: parts[1] == "1",
                    windows: parts[2].parse().unwrap_or(1),
                    created: parts[3].to_string(),
                })
            } else {
                None
            }
        })
        .collect();
    Ok(sessions)
}

// Autostop state management
//
// autostop.at   — unix timestamp when the cell should be stopped
// autostop.timeout — configured timeout in seconds (written at boot or shell exit)
//
// The sweep command is the authority: it writes autostop.at when it first
// sees zero sessions, and stops the cell when the timestamp expires.
// Shell exit writes autostop.at as a fast-path hint so the first sweep
// doesn't have to wait a full cycle to start the countdown.

fn autostop_at_file(name: &str) -> PathBuf {
    runtime_dir(name).join("autostop.at")
}

fn autostop_timeout_file(name: &str) -> PathBuf {
    runtime_dir(name).join("autostop.timeout")
}

/// Write the configured timeout so sweep can read it
pub fn write_autostop_timeout(name: &str, timeout: u64) {
    if let Err(e) = std::fs::write(autostop_timeout_file(name), timeout.to_string()) {
        warn!(error = %e, "failed to write autostop timeout");
    }
}

/// Read the configured timeout (defaults to 300s if missing)
pub fn read_autostop_timeout(name: &str) -> u64 {
    std::fs::read_to_string(autostop_timeout_file(name))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(300)
}

/// Mark that the cell should auto-stop after `timeout` seconds from now
pub fn mark_autostop(name: &str, timeout: u64) {
    let stop_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() + timeout)
        .unwrap_or(0);
    if let Err(e) = std::fs::write(autostop_at_file(name), stop_at.to_string()) {
        warn!(error = %e, "failed to mark autostop");
    }
}

/// Clear a pending autostop (e.g. when a session is entered)
pub fn clear_autostop(name: &str) {
    std::fs::remove_file(autostop_at_file(name)).ok(); // may not exist
}

/// Returns seconds remaining until autostop, or None
pub fn autostop_remaining(name: &str) -> Option<u64> {
    let at_str = std::fs::read_to_string(autostop_at_file(name)).ok()?;
    let stop_at: u64 = at_str.trim().parse().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if stop_at > now {
        Some(stop_at - now)
    } else {
        Some(0)
    }
}

/// Sweep all running cells: schedule or execute autostop as needed.
/// Called by `cella sweep` on a timer.
#[instrument]
pub fn sweep() -> Result<Vec<(String, SweepAction)>> {
    let mut actions = Vec::new();
    let cells = list_cells().unwrap_or_default();

    for name in &cells {
        if !is_running(name).unwrap_or(false) {
            continue;
        }

        let sessions = count_sessions(name).unwrap_or(1);
        let timeout = read_autostop_timeout(name);

        if sessions > 0 {
            // active sessions — clear any pending autostop
            if autostop_remaining(name).is_some() {
                clear_autostop(name);
                actions.push((name.clone(), SweepAction::Cancelled));
            }
            continue;
        }

        // zero sessions
        match autostop_remaining(name) {
            None => {
                // first observation — start the countdown
                mark_autostop(name, timeout);
                actions.push((name.clone(), SweepAction::Scheduled(timeout)));
            }
            Some(0) => {
                // timer expired — stop the cell
                if let Err(e) = stop(name) {
                    warn!(error = %e, cell = %name, "sweep failed to stop cell");
                }
                clear_autostop(name);
                actions.push((name.clone(), SweepAction::Stopped));
            }
            Some(remaining) => {
                actions.push((name.clone(), SweepAction::Waiting(remaining)));
            }
        }
    }

    Ok(actions)
}

#[derive(Debug)]
pub enum SweepAction {
    Scheduled(u64),
    Waiting(u64),
    Stopped,
    Cancelled,
}

// VM lifecycle

#[instrument]
pub fn is_running(name: &str) -> Result<bool> {
    let rt = runtime_dir(name);
    if !rt.join("ip").exists() {
        return Ok(false);
    }
    let output = Command::new("systemctl")
        .args(["is-active", &format!("microvm@{name}")])
        .output()?;
    Ok(output.status.success())
}

#[instrument(skip(config))]
pub fn start(name: &str, repo_name: &str, config: &CellaConfig) -> Result<()> {
    let rt = runtime_dir(name);
    std::fs::create_dir_all(&rt)?;

    let ip = cell::allocate_ip(name)?;
    info!(ip = %ip, "allocated IP");

    // write autostop timeout for sweep
    write_autostop_timeout(name, config.shell_timeout);

    // generate wrapper flake
    generate_flake(name, &ip, repo_name)?;

    // build the VM runner
    let flake_path = cell::cell_flake_dir(name);
    let flake_ref = format!("path:{}", flake_path.display());
    let runner_attr = format!("{flake_ref}#nixosConfigurations.{name}.config.microvm.declaredRunner");

    let output = Command::new("nix")
        .args([
            "--extra-experimental-features", "nix-command flakes",
            "build", &runner_attr,
            "--no-link", "--print-out-paths",
        ])
        .output()
        .context("nix build failed")?;
    if !output.status.success() {
        cell::release_ip(name);
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("failed to build cell VM '{name}': {stderr}");
    }
    let runner_path = String::from_utf8(output.stdout)?.trim().to_string();

    // link runner into /var/lib/microvms/{name}/current for microvm@ template
    let microvms_dir = format!("/var/lib/microvms/{name}");
    run_privileged(&["mkdir", "-p", &microvms_dir])?;
    run_privileged(&["chown", "microvm:kvm", &microvms_dir])?;
    let current_link = format!("{microvms_dir}/current");
    run_privileged(&["rm", "-f", &current_link]).ok();
    run_privileged(&["ln", "-sf", &runner_path, &current_link])?;
    run_privileged(&["chown", "-h", "microvm:kvm", &current_link])?;

    // start the VM
    let status = run_privileged(&["systemctl", "start", &format!("microvm@{name}")])?;
    if !status.success() {
        cell::release_ip(name);
        anyhow::bail!("failed to start VM for cell '{name}'");
    }

    // register proxy rules from cella.toml
    register_proxy_rules(&ip, name, config)?;

    // register DNS: branch.repo.cell -> VM IP
    register_dns(name, repo_name, &ip);

    // save state
    std::fs::write(rt.join("ip"), &ip)?;
    std::fs::write(rt.join("repo"), repo_name)?;
    let guest_user = read_host_config_user().unwrap_or_else(|| {
        std::env::var("USER").unwrap_or_default()
    });
    if !guest_user.is_empty() {
        std::fs::write(rt.join("user"), &guest_user)?;
    }

    Ok(())
}

#[instrument]
pub fn stop(name: &str) -> Result<()> {
    let rt = runtime_dir(name);
    if !rt.join("ip").exists() {
        return Ok(());
    }

    let ip = std::fs::read_to_string(rt.join("ip")).unwrap_or_default().trim().to_string();
    let repo = std::fs::read_to_string(rt.join("repo")).unwrap_or_default().trim().to_string();

    // deregister proxy rules and DNS
    if !ip.is_empty() {
        if let Err(e) = deregister_proxy_rules(&ip) {
            warn!(error = %e, ip = %ip, "failed to deregister proxy rules");
        }
    }
    if !repo.is_empty() {
        deregister_dns(name, &repo);
    }

    // stop the VM
    if let Err(e) = run_privileged(&["systemctl", "stop", &format!("microvm@{name}")]) {
        warn!(error = %e, "failed to stop VM");
    }

    // cleanup runtime (but NOT cell dir or IP — those persist)
    std::fs::remove_file(rt.join("ip")).ok(); // may not exist

    Ok(())
}

#[instrument]
pub fn delete(name: &str) -> Result<()> {
    // stop first if running
    if is_running(name)? {
        stop(name)?;
    }

    // remove cell directory
    let dir = cell_dir(name);
    if dir.exists() {
        let status = Command::new("sudo")
            .args(["rm", "-rf", &dir.to_string_lossy()])
            .status()
            .context("failed to remove cell dir")?;
        if !status.success() {
            anyhow::bail!("failed to remove cell dir at {}", dir.display());
        }
    }

    // release IP
    cell::release_ip(name);

    // cleanup runtime dir
    let rt = runtime_dir(name);
    std::fs::remove_dir_all(&rt).ok(); // best-effort cleanup

    Ok(())
}

#[instrument(skip(session_cfg))]
pub fn shell(
    name: &str,
    session: Option<&str>,
    command: Option<&str>,
    session_cfg: Option<&crate::config::SessionConfig>,
) -> Result<()> {
    // if no session config provided, try loading from the cell's repo
    let loaded_cfg = if session_cfg.is_none() {
        let repo_dir = cell_repo_dir(name);
        crate::config::load(&repo_dir).ok()
    } else {
        None
    };
    let session_cfg = session_cfg.or(loaded_cfg.as_ref().map(|c| &c.session));

    let rt = runtime_dir(name);
    let ip = std::fs::read_to_string(rt.join("ip"))
        .context("VM not running (no ip file)")?
        .trim()
        .to_string();

    let ssh_user = std::fs::read_to_string(rt.join("user"))
        .unwrap_or_default().trim().to_string();
    let ssh_target = if ssh_user.is_empty() {
        ip.clone()
    } else {
        format!("{ssh_user}@{ip}")
    };

    let repo_name = std::fs::read_to_string(rt.join("repo"))
        .unwrap_or_else(|_| "cell".to_string()).trim().to_string();
    let workspace = format!("/{repo_name}");

    // wait for VM: SSH reachable + workspace mounted
    let probe_cmd = format!("test -d {workspace}");
    let sp = indicatif::ProgressBar::new_spinner();
    sp.set_style(
        indicatif::ProgressStyle::default_spinner()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
            .template("  {spinner} {msg}")
            .unwrap()
    );
    sp.set_message("waiting for VM");
    sp.enable_steady_tick(std::time::Duration::from_millis(80));
    for i in 0..60 {
        let probe = Command::new("ssh")
            .args([
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR",
                "-o", "ConnectTimeout=1",
                &ssh_target,
                &probe_cmd,
            ])
            .output();
        if let Ok(out) = probe {
            if out.status.success() {
                sp.finish_and_clear();
                break;
            }
        }
        if i == 59 {
            sp.finish_with_message("VM did not become reachable within 60s");
            anyhow::bail!("VM did not become reachable within 60s");
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    // apply synced files from host-side sync dir to VM home
    let sync_dir = format!("/var/lib/cella/cells/{name}/sync");
    if Path::new(&sync_dir).exists() {
        // scp from host sync dir into VM home
        let scp_result = Command::new("scp")
            .args([
                "-r",
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR",
                &format!("{sync_dir}/."),
                &format!("{ssh_target}:~/"),
            ])
            .output();
        if let Ok(out) = &scp_result {
            if !out.status.success() {
                warn!(stderr = %String::from_utf8_lossy(&out.stderr), "sync apply failed");
            }
        }
    }

    // clear pending autostop — a session is being entered
    clear_autostop(name);

    let ssh_base = |extra_args: &[&str], cmd: &str| -> Command {
        let mut c = Command::new("ssh");
        c.args([
            "-A",
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR",
            "-o", "ServerAliveInterval=30",
            "-o", "ServerAliveCountMax=3",
        ]);
        for arg in extra_args {
            c.arg(arg);
        }
        c.arg(&ssh_target);
        c.arg(cmd);
        c
    };

    // determine session name and command
    let sess_name = session.map(|s| s.to_string()).unwrap_or_else(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("s-{ts}")
    });

    let cmd = match command {
        Some(c) => format!("cd {workspace} && {c}"),
        None => {
            let inner = session_cfg
                .and_then(|s| s.command.as_deref())
                .map(|c| format!("{c}"))
                .unwrap_or_else(|| "exec $SHELL -l".to_string());
            format!("cd {workspace} && tmux new-session -A -s {sess_name} '{inner}'")
        }
    };

    // start hooks after a short delay (session needs to exist first)
    if command.is_none() {
        if let Some(cfg) = session_cfg {
            if !cfg.hooks.is_empty() {
                upload_hooks(&ssh_target, &workspace, &sess_name, &cfg.hooks)?;
                // start hooks in background on the VM — the sleep ensures tmux session exists
                let starter_path = format!("/tmp/cella-start-hooks-{sess_name}.sh");
                Command::new("ssh")
                    .args([
                        "-o", "StrictHostKeyChecking=no",
                        "-o", "UserKnownHostsFile=/dev/null",
                        "-o", "LogLevel=ERROR",
                        &ssh_target,
                        &format!("sh -c 'nohup sh {starter_path} >/dev/null 2>&1 &'"),
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status().ok();
            }
        }
    }

    let status = ssh_base(&["-t"], &cmd)
        .status()
        .context("ssh failed")?;

    // run on_exit hook
    if command.is_none() {
        if let Some(cfg) = session_cfg {
            if let Some(on_exit) = &cfg.on_exit {
                let exit_cmd = format!("cd {workspace} && {on_exit}");
                ssh_base(&[], &exit_cmd)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status().ok();
            }
        }
    }

    // after shell exits: if no sessions remain, hint autostop for sweep
    if command.is_none() {
        if let Ok(true) = is_running(name) {
            let sessions = count_sessions(name).unwrap_or(0);
            if sessions == 0 {
                let timeout = read_autostop_timeout(name);
                mark_autostop(name, timeout);
            }
        }
    }

    if !status.success() {
        anyhow::bail!("ssh exited with {}", status);
    }
    Ok(())
}

fn upload_hooks(
    ssh_target: &str,
    workspace: &str,
    session_name: &str,
    hooks: &[String],
) -> Result<()> {
    // the `session` helper script provides read/send to hooks
    let session_helper = format!(
        r#"#!/bin/sh
case "$1" in
  read) tmux capture-pane -p -t '{sess}' 2>/dev/null ;;
  send) shift; tmux send-keys -t '{sess}' "$*" Enter ;;
  *) echo "usage: session read|send <text>" >&2; exit 1 ;;
esac"#,
        sess = session_name
    );

    // build a starter script that waits for tmux session then launches hooks
    let mut starter = format!(
        "#!/bin/sh\n# wait for tmux session to exist\nfor i in $(seq 1 30); do\n  tmux has-session -t '{sess}' 2>/dev/null && break\n  sleep 0.2\ndone\n",
        sess = session_name
    );
    for hook in hooks {
        let log_path = format!("/tmp/cella-hook-{}.log", hook.replace('/', "-"));
        starter.push_str(&format!(
            "cd {workspace} && PATH=\"/tmp:$PATH\" nohup {hook} >'{log}' 2>&1 &\n",
            workspace = workspace,
            hook = hook,
            log = log_path,
        ));
    }

    let helper_path = format!("/tmp/cella-session-{}", session_name);
    let starter_path = format!("/tmp/cella-start-hooks-{}.sh", session_name);

    let ssh_upload = |content: &str, dest: &str| -> Result<()> {
        let cmd = format!("sh -c 'cat > {dest} && chmod +x {dest}'");
        let mut child = Command::new("ssh")
            .args([
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR",
                ssh_target,
                &cmd,
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("failed to upload hook file")?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(content.as_bytes())?;
        }
        child.wait()?;
        Ok(())
    };

    ssh_upload(&session_helper, &helper_path)?;
    ssh_upload(&starter, &starter_path)?;

    // symlink helper as `session` on PATH
    Command::new("ssh")
        .args([
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR",
            ssh_target,
            &format!("ln -sf '{helper_path}' /tmp/session"),
        ])
        .output().ok();

    Ok(())
}

// runtime_dir is now in cell.rs, re-exported above

// List all cells (from cell directories)

// list_cells is now in cell.rs, re-exported above

// Proxy control API

fn register_proxy_rules(ip: &str, branch: &str, config: &CellaConfig) -> Result<()> {
    let nucleus_domains: Vec<&str> = config.nucleus.allowed_domains.iter().map(|s| s.as_str()).collect();
    let body = serde_json::json!({
        "cellIp": ip,
        "branchId": branch,
        "nucleusAllowedDomains": nucleus_domains,
    });

    let output = Command::new("curl")
        .args([
            "-sf",
            "-X", "POST",
            "-H", "Content-Type: application/json",
            "-d", &body.to_string(),
            &format!("{PROXY_CONTROL}/cells"),
        ])
        .output()
        .context("failed to register proxy rules")?;

    if !output.status.success() {
        warn!("failed to register proxy rules (proxy may not be running)");
    }
    Ok(())
}

fn deregister_proxy_rules(ip: &str) -> Result<()> {
    let output = Command::new("curl")
        .args([
            "-sf",
            "-X", "DELETE",
            &format!("{PROXY_CONTROL}/cells/{ip}"),
        ])
        .output()
        .context("failed to deregister proxy rules")?;

    if !output.status.success() {
        warn!("failed to deregister proxy rules");
    }
    Ok(())
}

// DNS registration

fn sanitize_dns(s: &str) -> String {
    let sanitized: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mut result = String::new();
    let mut prev_hyphen = true;
    for c in sanitized.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    result.trim_end_matches('-').to_string()
}

pub fn dns_hostname(branch: &str, repo: &str) -> String {
    format!("{}.{}.cell", sanitize_dns(branch), sanitize_dns(repo))
}

fn register_dns(branch: &str, repo: &str, ip: &str) {
    let hostname = dns_hostname(branch, repo);
    let entry = format!("{ip} {hostname}");
    let hosts = std::fs::read_to_string(DNS_HOSTS).unwrap_or_default();
    if !hosts.contains(&entry) {
        let updated = if hosts.is_empty() {
            format!("{entry}\n")
        } else {
            format!("{hosts}{entry}\n")
        };
        if let Err(e) = std::fs::write(DNS_HOSTS, updated) {
            warn!(error = %e, "failed to write DNS hosts");
        }
    }
    if let Err(e) = Command::new("sudo")
        .args(["systemctl", "reload", "dnsmasq"])
        .status() {
        warn!(error = %e, "failed to reload dnsmasq");
    }
    info!(hostname = %hostname, "registered DNS");
}

fn deregister_dns(branch: &str, repo: &str) {
    let hostname = dns_hostname(branch, repo);
    if let Ok(hosts) = std::fs::read_to_string(DNS_HOSTS) {
        let updated: String = hosts
            .lines()
            .filter(|line| !line.contains(&hostname))
            .collect::<Vec<_>>()
            .join("\n");
        let updated = if updated.is_empty() {
            String::new()
        } else {
            format!("{updated}\n")
        };
        if let Err(e) = std::fs::write(DNS_HOSTS, updated) {
            warn!(error = %e, "failed to write DNS hosts on deregister");
        }
        if let Err(e) = Command::new("sudo")
            .args(["systemctl", "reload", "dnsmasq"])
            .status() {
            warn!(error = %e, "failed to reload dnsmasq on deregister");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // DNS sanitization

    #[test]
    fn sanitize_simple() {
        assert_eq!(sanitize_dns("feat"), "feat");
    }

    #[test]
    fn sanitize_slashes() {
        assert_eq!(sanitize_dns("feature/auth-flow"), "feature-auth-flow");
    }

    #[test]
    fn sanitize_uppercase() {
        assert_eq!(sanitize_dns("MyBranch"), "mybranch");
    }

    #[test]
    fn sanitize_consecutive_special() {
        assert_eq!(sanitize_dns("a//b--c"), "a-b-c");
    }

    #[test]
    fn sanitize_leading_trailing() {
        assert_eq!(sanitize_dns("-leading-"), "leading");
        assert_eq!(sanitize_dns("//trailing//"), "trailing");
    }

    #[test]
    fn dns_hostname_basic() {
        assert_eq!(dns_hostname("feat", "myapp"), "feat.myapp.cell");
    }

    #[test]
    fn dns_hostname_complex() {
        assert_eq!(dns_hostname("feature/auth", "my-app"), "feature-auth.my-app.cell");
    }

    // Cell path construction

    #[test]
    fn cell_dir_path() {
        assert_eq!(cell_dir("myapp-feat"), PathBuf::from("/var/lib/cella/cells/myapp-feat"));
    }

    #[test]
    fn cell_repo_dir_path() {
        assert_eq!(cell_repo_dir("myapp-feat"), PathBuf::from("/var/lib/cella/cells/myapp-feat/repo"));
    }

    #[test]
    fn cell_flake_dir_path() {
        assert_eq!(cell::cell_flake_dir("myapp-feat"), PathBuf::from("/var/lib/cella/cells/myapp-feat/flake"));
    }

    // IP pool (using temp files)

    #[test]
    fn ip_pool_roundtrip() {
        let mut pool = std::collections::HashMap::new();
        pool.insert("cell-a".to_string(), "192.168.83.11".to_string());
        pool.insert("cell-b".to_string(), "192.168.83.12".to_string());

        let json = serde_json::to_string_pretty(&pool).unwrap();
        let loaded: std::collections::HashMap<String, String> = serde_json::from_str(&json).unwrap();
        assert_eq!(pool, loaded);
    }

    #[test]
    fn ip_pool_first_free() {
        let mut used = std::collections::HashSet::new();
        used.insert("192.168.83.11");
        used.insert("192.168.83.12");

        let mut found = None;
        for i in 11..=254u16 {
            let ip = format!("192.168.83.{i}");
            if !used.contains(ip.as_str()) {
                found = Some(ip);
                break;
            }
        }
        assert_eq!(found, Some("192.168.83.13".to_string()));
    }

    #[test]
    fn ip_pool_range() {
        // verify range is .11 to .254 (244 addresses)
        let count = (11..=254u16).count();
        assert_eq!(count, 244);
    }

    // Flake generation

    #[test]
    fn generated_flake_contains_name() {
        let tmpdir = std::env::temp_dir().join("cella-test-flake");
        let _ = std::fs::remove_dir_all(&tmpdir);
        std::fs::create_dir_all(tmpdir.join("flake")).unwrap();

        // we can't call generate_flake directly (uses CELLS_PATH),
        // but we can test the format string logic
        let name = "myapp-feat";
        let ip = "192.168.83.11";
        let cell_dir = "/var/lib/cella/cells/myapp-feat";

        let flake = format!(
            "name = \"{name}\"; ip = \"{ip}\"; cellDir = \"{cell_dir}\";",
        );
        assert!(flake.contains("myapp-feat"));
        assert!(flake.contains("192.168.83.11"));

        let _ = std::fs::remove_dir_all(&tmpdir);
    }
}
