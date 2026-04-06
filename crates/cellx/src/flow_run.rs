use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::info;

use flow::{
    Decision, FlowConfig, FlowReport, FlowState, FlowStatus, Lifecycle, NetRule,
    Op, Rule, Signal, check_signal, flow_state_dir, load_flow, load_op, now_secs,
    op_allows, op_workspace, read_result_from, resolve_timeout,
};

const BRIDGE_ADDR: &str = "192.168.83.1";
const CONTROL_PORT: u16 = 8082;

fn get_cell_ip() -> Option<String> {
    let output = Command::new("ip")
        .args(["-4", "-o", "addr", "show"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for part in stdout.split_whitespace() {
        if part.contains("192.168.83.") {
            return part.split('/').next().map(|s| s.to_string());
        }
    }
    None
}

fn load_cell_rules(repo_root: &Path) -> HashMap<String, Rule> {
    #[derive(Deserialize)]
    struct CellConfig {
        #[serde(default)]
        rules: HashMap<String, Rule>,
    }

    let path = repo_root.join(".cella/config.toml");
    if !path.exists() {
        return HashMap::new();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str::<CellConfig>(&s).ok())
        .map(|c| c.rules)
        .unwrap_or_default()
}

fn lookup_rule<'a>(
    reference: &str,
    cell_rules: &'a HashMap<String, Rule>,
    flow_rules: &'a HashMap<String, Rule>,
) -> Option<&'a Rule> {
    if let Some(name) = reference.strip_prefix("cell:") {
        cell_rules.get(name)
    } else if let Some(name) = reference.strip_prefix("flow:") {
        flow_rules.get(name)
    } else {
        flow_rules.get(reference).or_else(|| cell_rules.get(reference))
    }
}

fn expand_refs(
    values: &[String],
    field: &str,
    direction: &str,
    cell_rules: &HashMap<String, Rule>,
    flow_rules: &HashMap<String, Rule>,
) -> Vec<String> {
    let mut result = Vec::new();
    for val in values {
        if val.contains(':') && !val.contains("*.") && !val.contains("://") {
            if let Some(rule) = lookup_rule(val, cell_rules, flow_rules) {
                let entries = match direction {
                    "reads" => rule.reads.as_ref().and_then(|n| {
                        if field == "allowed" { n.allowed.as_ref() } else { n.denied.as_ref() }
                    }),
                    "writes" => rule.writes.as_ref().and_then(|n| {
                        if field == "allowed" { n.allowed.as_ref() } else { n.denied.as_ref() }
                    }),
                    "files" => rule.files.as_ref().map(|f| &f.denied),
                    _ => None,
                };
                if let Some(entries) = entries {
                    result.extend(entries.iter().cloned());
                }
            }
        } else {
            result.push(val.clone());
        }
    }
    result
}

#[derive(Debug, Default)]
struct ResolvedRule {
    extends_server: bool,
    reads_allowed: Option<Vec<String>>,
    reads_denied: Option<Vec<String>>,
    writes_allowed: Option<Vec<String>>,
    writes_denied: Option<Vec<String>>,
    files_denied: Vec<String>,
}

fn resolve_op_rules(
    op: &Op,
    flow_rules: &HashMap<String, Rule>,
    cell_rules: &HashMap<String, Rule>,
) -> ResolvedRule {
    let mut resolved = ResolvedRule::default();

    if op.config.rules.is_empty() {
        resolved.extends_server = true;
        return resolved;
    }

    let mut active_rules: Vec<Rule> = Vec::new();
    for name in &op.config.rules {
        if let Some(rule) = lookup_rule(name, cell_rules, flow_rules) {
            active_rules.push(rule.clone());
        }
    }

    for rule in &active_rules {
        for ext in &rule.extends {
            if ext == "server:default" {
                resolved.extends_server = true;
            } else if let Some(base) = lookup_rule(ext, cell_rules, flow_rules) {
                merge_net_into(&mut resolved.reads_allowed, &base.reads, "allowed");
                merge_net_into(&mut resolved.reads_denied, &base.reads, "denied");
                merge_net_into(&mut resolved.writes_allowed, &base.writes, "allowed");
                merge_net_into(&mut resolved.writes_denied, &base.writes, "denied");
                if let Some(ref f) = base.files {
                    resolved.files_denied.extend(f.denied.iter().cloned());
                }
            }
        }
    }

    for rule in &active_rules {
        if let Some(ref net) = rule.reads {
            if let Some(ref vals) = net.allowed {
                let expanded = expand_refs(vals, "allowed", "reads", cell_rules, flow_rules);
                resolved.reads_allowed = Some(expanded);
            }
            if let Some(ref vals) = net.denied {
                let expanded = expand_refs(vals, "denied", "reads", cell_rules, flow_rules);
                resolved.reads_denied = Some(expanded);
            }
        }
        if let Some(ref net) = rule.writes {
            if let Some(ref vals) = net.allowed {
                let expanded = expand_refs(vals, "allowed", "writes", cell_rules, flow_rules);
                resolved.writes_allowed = Some(expanded);
            }
            if let Some(ref vals) = net.denied {
                let expanded = expand_refs(vals, "denied", "writes", cell_rules, flow_rules);
                resolved.writes_denied = Some(expanded);
            }
        }
        if let Some(ref f) = rule.files {
            let expanded = expand_refs(&f.denied, "denied", "files", cell_rules, flow_rules);
            resolved.files_denied = expanded;
        }
    }

    resolved
}

fn merge_net_into(target: &mut Option<Vec<String>>, source: &Option<NetRule>, field: &str) {
    if let Some(net) = source {
        let vals = if field == "allowed" { &net.allowed } else { &net.denied };
        if let Some(v) = vals {
            target.get_or_insert_with(Vec::new).extend(v.iter().cloned());
        }
    }
}

fn apply_rules(resolved: &ResolvedRule, cell_ip: &str) {
    let has_net = resolved.reads_allowed.is_some() || resolved.reads_denied.is_some()
        || resolved.writes_allowed.is_some() || resolved.writes_denied.is_some();

    if !has_net {
        return;
    }

    let egress = serde_json::json!({
        "additive": resolved.extends_server,
        "reads": {
            "allowed": resolved.reads_allowed,
            "denied": resolved.reads_denied,
        },
        "writes": {
            "allowed": resolved.writes_allowed,
            "denied": resolved.writes_denied,
        },
    });

    let body = serde_json::json!({
        "cellIp": cell_ip,
        "branchId": "flow",
        "egress": egress,
    });

    let url = format!("http://{}:{}/cells", BRIDGE_ADDR, CONTROL_PORT);
    if let Err(e) = Command::new("curl")
        .args(["-sf", "-X", "POST", "-H", "Content-Type: application/json", "-d", &body.to_string(), &url])
        .output()
    {
        eprintln!("warning: failed to update network rules: {e}");
    }
}

fn clear_network_rules(cell_ip: &str) {
    let body = serde_json::json!({
        "cellIp": cell_ip,
        "branchId": "flow",
    });
    let url = format!("http://{}:{}/cells", BRIDGE_ADDR, CONTROL_PORT);
    Command::new("curl")
        .args(["-sf", "-X", "POST", "-H", "Content-Type: application/json", "-d", &body.to_string(), &url])
        .output()
        .ok();
}

fn apply_file_rules(resolved: &ResolvedRule, repo_root: &Path) {
    for pattern in &resolved.files_denied {
        let target = repo_root.join(pattern.trim_end_matches("/**"));
        if target.exists() {
            Command::new("chmod")
                .args(["-R", "a-w", &target.to_string_lossy()])
                .output()
                .ok();
            info!(path = %target.display(), "applied file deny rule");
        }
    }
}

fn clear_file_rules(resolved: &ResolvedRule, repo_root: &Path) {
    for pattern in &resolved.files_denied {
        let target = repo_root.join(pattern.trim_end_matches("/**"));
        if target.exists() {
            Command::new("chmod")
                .args(["-R", "u+w", &target.to_string_lossy()])
                .output()
                .ok();
        }
    }
}

fn clear_rules(resolved: &ResolvedRule, cell_ip: &Option<String>, repo_root: &Path) {
    if let Some(ip) = cell_ip {
        clear_network_rules(ip);
    }
    clear_file_rules(resolved, repo_root);
}

// Hook resolution and execution

struct ResolvedHooks {
    pre: Vec<PathBuf>,
    handle: PathBuf,
    post: Vec<PathBuf>,
}

fn resolve_hooks(flow_dir: &Path, op_name: &str) -> Result<ResolvedHooks> {
    let op_dir = flow_dir.join("ops").join(op_name);

    // handle: op overrides flow (required)
    let op_handle = op_dir.join("handle.sh");
    let flow_handle = flow_dir.join("handle.sh");
    let handle = if op_handle.exists() {
        op_handle
    } else if flow_handle.exists() {
        flow_handle
    } else {
        anyhow::bail!("no handle.sh found (checked {} and {})",
            op_handle.display(), flow_handle.display());
    };

    // pre: compose (flow wraps op)
    let mut pre = Vec::new();
    let flow_pre = flow_dir.join("pre.sh");
    if flow_pre.exists() {
        pre.push(flow_pre);
    }
    let op_pre = op_dir.join("pre.sh");
    if op_pre.exists() {
        pre.push(op_pre);
    }

    // post: compose (op then flow)
    let mut post = Vec::new();
    let op_post = op_dir.join("post.sh");
    if op_post.exists() {
        post.push(op_post);
    }
    let flow_post = flow_dir.join("post.sh");
    if flow_post.exists() {
        post.push(flow_post);
    }

    Ok(ResolvedHooks { pre, handle, post })
}

fn run_hook(
    path: &Path,
    op_name: &str,
    flow_name: &str,
    flow_dir: &Path,
    flow_state_dir: &Path,
    workspace: &Path,
    prompt: &str,
    params: &HashMap<String, String>,
    transition_params: &Option<serde_json::Value>,
    exit_code: Option<i32>,
    duration: u64,
) -> Result<i32> {
    let mut cmd = Command::new(path);
    cmd.env("FLOW_NAME", flow_name)
        .env("FLOW_DIR", flow_dir)
        .env("FLOW_STATE_DIR", flow_state_dir)
        .env("OP_NAME", op_name)
        .env("OP_WORKSPACE", workspace)
        .env("OP_PROMPT", prompt)
        .env("OP_DURATION", duration.to_string());

    // inject frontmatter params as PARAM_* env vars
    for (k, v) in params {
        cmd.env(format!("PARAM_{}", k.to_uppercase()), v);
    }

    // inject transition params from previous op
    if let Some(tp) = transition_params {
        cmd.env("OP_PARAMS", tp.to_string());
    }

    if let Some(code) = exit_code {
        cmd.env("OP_EXIT_CODE", code.to_string());
    }

    let status = cmd.status()
        .context(format!("executing hook {}", path.display()))?;
    Ok(status.code().unwrap_or(1))
}

fn run_onion(
    hooks: &ResolvedHooks,
    op_name: &str,
    flow_name: &str,
    flow_dir: &Path,
    state_dir: &Path,
    workspace: &Path,
    prompt: &str,
    params: &HashMap<String, String>,
    transition_params: &Option<serde_json::Value>,
    duration: u64,
) -> Result<i32> {
    // pre hooks (flow → op)
    for hook in &hooks.pre {
        let code = run_hook(hook, op_name, flow_name, flow_dir, state_dir,
            workspace, prompt, params, transition_params, None, duration)?;
        if code != 0 {
            info!(hook = %hook.display(), code, "pre hook failed");
            return Ok(code);
        }
    }

    // handle
    let handle_code = run_hook(&hooks.handle, op_name, flow_name, flow_dir,
        state_dir, workspace, prompt, params, transition_params, None, duration)?;

    // post hooks (op → flow), receive handle exit code
    for hook in &hooks.post {
        let code = run_hook(hook, op_name, flow_name, flow_dir, state_dir,
            workspace, prompt, params, transition_params, Some(handle_code), duration)?;
        if code != 0 {
            info!(hook = %hook.display(), code, "post hook failed");
            return Ok(code);
        }
    }

    Ok(handle_code)
}

// Status reporting to host

fn report_status(cell_ip: &str, status: &FlowStatus) {
    let report = FlowReport {
        cell_ip: cell_ip.to_string(),
        flow_name: status.flow_name.clone(),
        current_op: status.current_op.clone(),
        state: status.state.clone(),
        started_at: status.started_at,
        op_started_at: status.op_started_at,
    };
    let body = serde_json::to_string(&report).unwrap_or_default();
    let url = format!("http://{}:{}/flow-status", BRIDGE_ADDR, CONTROL_PORT);
    Command::new("curl")
        .args(["-sf", "-X", "POST", "-H", "Content-Type: application/json", "-d", &body, &url])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok();
}

// Run loop

#[derive(Debug, PartialEq)]
enum FlowOutcome {
    Success,
    Failure(String),
    Stopped,
    Paused,
}

pub fn run(flow_name: &str, repo_root: &Path, initial_params: Option<serde_json::Value>) -> Result<()> {
    let flow_dir = repo_root.join(".cella/flows").join(flow_name);
    let flow_config = load_flow(repo_root, flow_name)?;
    let mut current_op = flow_config.flow.start.clone();
    let now = now_secs();

    // Resume from paused state if applicable
    let state_dir = if let Ok(status) = FlowStatus::load() {
        if status.state == FlowState::Paused && status.flow_name == flow_name {
            current_op = status.current_op;
            status.state_dir
        } else {
            flow_state_dir(flow_config.flow.state.as_deref(), flow_name, now)
        }
    } else {
        flow_state_dir(flow_config.flow.state.as_deref(), flow_name, now)
    };
    std::fs::create_dir_all(&state_dir)?;

    let cell_ip = get_cell_ip();
    let cell_rules = load_cell_rules(repo_root);

    if cell_ip.is_none() && (!flow_config.rules.is_empty() || !cell_rules.is_empty()) {
        eprintln!("warning: could not detect cell IP — network rules will not be enforced");
    }

    let mut status = FlowStatus {
        flow_name: flow_name.to_string(),
        current_op: current_op.clone(),
        state: FlowState::Running,
        started_at: now,
        op_started_at: now,
        state_dir: state_dir.clone(),
    };
    status.save()?;
    if let Some(ref ip) = cell_ip {
        report_status(ip, &status);
    }

    info!(flow = flow_name, op = %current_op, "flow started");

    let outcome = run_loop(
        &mut current_op, &mut status, &flow_dir, &flow_config,
        &cell_rules, &cell_ip, repo_root, initial_params,
    );

    if outcome != FlowOutcome::Paused {
        run_lifecycle_ops(
            &outcome, flow_name, &state_dir, &flow_dir, &flow_config,
            &cell_rules, &cell_ip, repo_root,
        );
    }

    match outcome {
        FlowOutcome::Success | FlowOutcome::Stopped => {
            status.state = FlowState::Done;
            status.save()?;
            FlowStatus::remove();
        }
        FlowOutcome::Failure(_) => {
            status.state = FlowState::Failed;
            status.save()?;
            FlowStatus::remove();
        }
        FlowOutcome::Paused => {
            status.state = FlowState::Paused;
            status.save()?;
        }
    }
    if let Some(ref ip) = cell_ip {
        report_status(ip, &status);
    }

    if let FlowOutcome::Failure(ref msg) = outcome {
        anyhow::bail!("flow failed: {msg}");
    }
    Ok(())
}

fn run_loop(
    current_op: &mut String,
    status: &mut FlowStatus,
    flow_dir: &Path,
    flow_config: &FlowConfig,
    cell_rules: &HashMap<String, Rule>,
    cell_ip: &Option<String>,
    repo_root: &Path,
    initial_params: Option<serde_json::Value>,
) -> FlowOutcome {
    let flow_name = &status.flow_name;
    let state_dir = &status.state_dir.clone();
    let max_retries = flow_config.flow.max_retries;
    let mut retry_count: u32 = 0;
    let mut transition_params: Option<serde_json::Value> = initial_params;

    loop {
        if let Some(sig) = check_signal() {
            match sig {
                Signal::Pause => {
                    info!(op = %current_op, "flow paused (signal)");
                    return FlowOutcome::Paused;
                }
                Signal::Done => {
                    info!("flow stopped (signal)");
                    return FlowOutcome::Stopped;
                }
            }
        }

        let op = match load_op(flow_dir, current_op) {
            Ok(op) => op,
            Err(e) => return FlowOutcome::Failure(format!("loading op {current_op}: {e}")),
        };

        if op.config.on.is_some() {
            return FlowOutcome::Failure(format!("op '{current_op}' is a lifecycle op, not a regular op"));
        }

        // Resolve and apply rules
        let resolved = resolve_op_rules(&op, &flow_config.rules, cell_rules);
        if let Some(ip) = cell_ip {
            apply_rules(&resolved, ip);
        }
        apply_file_rules(&resolved, repo_root);

        // Resolve hooks
        let hooks = match resolve_hooks(flow_dir, current_op) {
            Ok(h) => h,
            Err(e) => {
                clear_rules(&resolved, cell_ip, repo_root);
                return FlowOutcome::Failure(format!("resolving hooks for {current_op}: {e}"));
            }
        };

        // Create op workspace
        let workspace = op_workspace(state_dir, current_op, now_secs());
        if let Err(e) = std::fs::create_dir_all(&workspace) {
            clear_rules(&resolved, cell_ip, repo_root);
            return FlowOutcome::Failure(format!("creating workspace: {e}"));
        }

        status.current_op = current_op.clone();
        status.op_started_at = now_secs();
        if let Err(e) = status.save() {
            return FlowOutcome::Failure(format!("saving state: {e}"));
        }
        if let Some(ip) = cell_ip {
            report_status(ip, status);
        }

        info!(op = %current_op, "running op");

        // Run the middleware onion
        let onion_result = run_onion(
            &hooks, current_op, flow_name, flow_dir,
            state_dir, &workspace, &op.prompt, &op.config.params,
            &transition_params, 0,
        );

        let duration = now_secs() - status.op_started_at;
        clear_rules(&resolved, cell_ip, repo_root);

        if let Err(e) = onion_result {
            return FlowOutcome::Failure(format!("hook execution: {e}"));
        }

        // check op timeout
        if let Some(timeout) = resolve_timeout(&op.config, &flow_config.flow) {
            if duration > timeout {
                info!(op = %current_op, duration, timeout, "op timed out");
                return FlowOutcome::Failure(
                    format!("op '{}' timed out ({}s > {}s)", current_op, duration, timeout)
                );
            }
        }

        info!(op = %current_op, duration, "op finished");

        // Read decision from workspace
        let decision = match read_result_from(&workspace) {
            Ok(d) => d,
            Err(_) => {
                return FlowOutcome::Failure(
                    format!("no transition decision for op '{current_op}' (missing result.json in workspace)")
                );
            }
        };

        match decision {
            Decision::To { ref op, ref params } => {
                match load_op(flow_dir, current_op) {
                    Ok(current) if !op_allows(&current, op) => {
                        return FlowOutcome::Failure(
                            format!("invalid transition: {} -> {} (not in next list)", current_op, op)
                        );
                    }
                    Err(e) => return FlowOutcome::Failure(format!("loading op: {e}")),
                    _ => {}
                }
                info!(from = %current_op, to = %op, "transitioning");
                *current_op = op.clone();
                transition_params = params.clone();
                retry_count = 0;
            }
            Decision::Retry { after } => {
                retry_count += 1;
                if retry_count > max_retries {
                    return FlowOutcome::Failure(
                        format!("op '{}' exceeded max retries ({})", current_op, max_retries)
                    );
                }
                if let Some(secs) = after {
                    info!(op = %current_op, delay = secs, retry = retry_count, "retrying after delay");
                    std::thread::sleep(std::time::Duration::from_secs(secs));
                } else {
                    info!(op = %current_op, retry = retry_count, "retrying");
                }
            }
            Decision::Pause => {
                info!(op = %current_op, "flow paused");
                return FlowOutcome::Paused;
            }
            Decision::Done => {
                info!("flow done");
                return FlowOutcome::Success;
            }
        }
    }
}

fn discover_lifecycle_ops(flow_dir: &Path) -> Vec<(String, Lifecycle)> {
    let ops_dir = flow_dir.join("ops");
    let mut ops = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&ops_dir) {
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .collect();
        names.sort();
        for name in names {
            if let Ok(op) = load_op(flow_dir, &name) {
                if let Some(lifecycle) = op.config.on {
                    ops.push((name, lifecycle));
                }
            }
        }
    }
    ops
}

fn run_lifecycle_ops(
    outcome: &FlowOutcome,
    flow_name: &str,
    state_dir: &Path,
    flow_dir: &Path,
    flow_config: &FlowConfig,
    cell_rules: &HashMap<String, Rule>,
    cell_ip: &Option<String>,
    repo_root: &Path,
) {
    let ops = discover_lifecycle_ops(flow_dir);

    for (name, lifecycle) in &ops {
        let should_run = match lifecycle {
            Lifecycle::Finish => true,
            Lifecycle::Success => *outcome == FlowOutcome::Success,
            Lifecycle::Failure => matches!(outcome, FlowOutcome::Failure(_)),
        };

        if !should_run {
            continue;
        }

        info!(op = %name, on = ?lifecycle, "running lifecycle op");

        let op = match load_op(flow_dir, &name) {
            Ok(op) => op,
            Err(e) => {
                eprintln!("warning: failed to load lifecycle op {name}: {e}");
                continue;
            }
        };

        let resolved = resolve_op_rules(&op, &flow_config.rules, cell_rules);
        if let Some(ip) = cell_ip {
            apply_rules(&resolved, ip);
        }
        apply_file_rules(&resolved, repo_root);

        let hooks = match resolve_hooks(flow_dir, &name) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("warning: failed to resolve hooks for lifecycle op {name}: {e}");
                clear_rules(&resolved, cell_ip, repo_root);
                continue;
            }
        };

        let workspace = op_workspace(state_dir, &name, now_secs());
        if let Err(e) = std::fs::create_dir_all(&workspace) {
            eprintln!("warning: failed to create workspace for {name}: {e}");
            clear_rules(&resolved, cell_ip, repo_root);
            continue;
        }

        match run_onion(&hooks, &name, flow_name, flow_dir, state_dir, &workspace, &op.prompt, &op.config.params, &None, 0) {
            Ok(_) => info!(op = %name, "lifecycle op finished"),
            Err(e) => eprintln!("warning: lifecycle op {name} failed: {e}"),
        }

        clear_rules(&resolved, cell_ip, repo_root);
    }
}

// Status display

pub fn print_status() -> Result<()> {
    match FlowStatus::load() {
        Ok(status) => {
            let elapsed = now_secs().saturating_sub(status.started_at);
            let mins = elapsed / 60;
            println!("flow: {}", status.flow_name);
            println!("state: {:?}", status.state);
            println!("op: {}", status.current_op);
            if mins > 0 {
                println!("elapsed: {}m", mins);
            }
        }
        Err(_) => {
            println!("no active flow");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flow::{FileRule, OpFrontmatter};

    #[test]
    fn resolve_standalone_rule() {
        let mut flow_rules = HashMap::new();
        flow_rules.insert("llm-only".to_string(), Rule {
            extends: vec![],
            files: None,
            reads: None,
            writes: Some(NetRule {
                allowed: Some(vec!["api.anthropic.com".to_string()]),
                denied: Some(vec!["*".to_string()]),
            }),
        });

        let op = Op {
            config: OpFrontmatter {
                name: "review".to_string(),
                rules: vec!["llm-only".to_string()],
                next: vec![],

                on: None,
                params: HashMap::new(),
                timeout: None,
            },
            prompt: String::new(),
        };

        let resolved = resolve_op_rules(&op, &flow_rules, &HashMap::new());
        assert!(!resolved.extends_server);
        assert_eq!(resolved.writes_allowed, Some(vec!["api.anthropic.com".to_string()]));
        assert_eq!(resolved.writes_denied, Some(vec!["*".to_string()]));
    }

    #[test]
    fn resolve_with_extends() {
        let mut flow_rules = HashMap::new();
        flow_rules.insert("review".to_string(), Rule {
            extends: vec!["server:default".to_string()],
            files: Some(FileRule { denied: vec!["src/**".to_string()] }),
            reads: None,
            writes: Some(NetRule {
                allowed: Some(vec!["api.anthropic.com".to_string()]),
                denied: None,
            }),
        });

        let op = Op {
            config: OpFrontmatter {
                name: "review".to_string(),
                rules: vec!["review".to_string()],
                next: vec![],

                on: None,
                params: HashMap::new(),
                timeout: None,
            },
            prompt: String::new(),
        };

        let resolved = resolve_op_rules(&op, &flow_rules, &HashMap::new());
        assert!(resolved.extends_server);
        assert_eq!(resolved.writes_allowed, Some(vec!["api.anthropic.com".to_string()]));
        assert_eq!(resolved.files_denied, vec!["src/**"]);
    }

    #[test]
    fn resolve_with_cell_reference() {
        let mut cell_rules = HashMap::new();
        cell_rules.insert("strict".to_string(), Rule {
            extends: vec![],
            files: None,
            reads: None,
            writes: Some(NetRule {
                allowed: None,
                denied: Some(vec!["evil.com".to_string()]),
            }),
        });

        let mut flow_rules = HashMap::new();
        flow_rules.insert("impl".to_string(), Rule {
            extends: vec!["server:default".to_string()],
            files: None,
            reads: None,
            writes: Some(NetRule {
                allowed: None,
                denied: Some(vec!["cell:strict".to_string(), "*.bad.com".to_string()]),
            }),
        });

        let op = Op {
            config: OpFrontmatter {
                name: "impl".to_string(),
                rules: vec!["impl".to_string()],
                next: vec![],

                on: None,
                params: HashMap::new(),
                timeout: None,
            },
            prompt: String::new(),
        };

        let resolved = resolve_op_rules(&op, &flow_rules, &cell_rules);
        assert!(resolved.extends_server);
        assert_eq!(resolved.writes_denied, Some(vec!["evil.com".to_string(), "*.bad.com".to_string()]));
    }

    #[test]
    fn resolve_no_rules_inherits_server() {
        let op = Op {
            config: OpFrontmatter {
                name: "impl".to_string(),
                rules: vec![],
                next: vec![],

                on: None,
                params: HashMap::new(),
                timeout: None,
            },
            prompt: String::new(),
        };

        let resolved = resolve_op_rules(&op, &HashMap::new(), &HashMap::new());
        assert!(resolved.extends_server);
        assert!(resolved.writes_allowed.is_none());
    }

    #[test]
    fn resolve_missing_ref_ignored() {
        let op = Op {
            config: OpFrontmatter {
                name: "impl".to_string(),
                rules: vec!["nonexistent".to_string()],
                next: vec![],

                on: None,
                params: HashMap::new(),
                timeout: None,
            },
            prompt: String::new(),
        };

        let resolved = resolve_op_rules(&op, &HashMap::new(), &HashMap::new());
        assert!(!resolved.extends_server);
        assert!(resolved.writes_allowed.is_none());
    }
}
