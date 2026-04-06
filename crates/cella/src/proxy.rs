use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::Path;

// Config types (shared with mitmproxy addon via JSON)

#[allow(dead_code)] // fields read by mitmproxy addon via JSON
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProxyConfig {
    pub cells: Vec<CellRules>,
    pub egress: EgressConfig,
    #[serde(rename = "httpPort")]
    pub http_port: u16,
    #[serde(rename = "gitCredentialPort")]
    pub git_credential_port: u16,
    #[serde(rename = "controlPort")]
    pub control_port: u16,
    #[serde(rename = "logFile")]
    pub log_file: String,
    #[serde(rename = "bindAddress")]
    pub bind_address: String,
    /// Server-side sweep timeout in seconds (default: 6h = 21600).
    /// Stops VMs where the current op has been running longer than this.
    #[serde(rename = "sweepTimeout", default = "default_sweep_timeout")]
    pub sweep_timeout: u64,
    /// How often to run the sweep, in seconds (default: 5m = 300).
    #[serde(rename = "sweepInterval", default = "default_sweep_interval")]
    pub sweep_interval: u64,
}

fn default_sweep_timeout() -> u64 { 6 * 3600 }
fn default_sweep_interval() -> u64 { 300 }

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct EgressConfig {
    pub reads: EgressRules,
    pub writes: EgressRules,
    pub credentials: Vec<CredentialRule>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct EgressRules {
    pub methods: Vec<String>,
    pub allowed: serde_json::Value,
    pub denied: serde_json::Value,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CellRules {
    #[serde(rename = "cellIp")]
    pub cell_ip: String,
    #[serde(rename = "branchId")]
    pub branch_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<CellEgress>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CellEgress {
    #[serde(default)]
    pub additive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reads: Option<CellEgressRules>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writes: Option<CellEgressRules>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CellEgressRules {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied: Option<Vec<String>>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CredentialRule {
    pub host: String,
    pub header: String,
    #[serde(rename = "envVar")]
    pub env_var: String,
}

const DYNAMIC_CELLS: &str = "/var/lib/cella/cells.json";

// Dynamic cell state — written to file for mitmproxy addon to read

fn load_cells() -> Vec<CellRules> {
    std::fs::read_to_string(DYNAMIC_CELLS)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cells(cells: &[CellRules]) {
    let json = serde_json::to_string_pretty(cells).unwrap_or_default();
    if let Err(e) = std::fs::write(DYNAMIC_CELLS, json) {
        eprintln!("warning: failed to save cells.json: {e}");
    }
}

// Git credential handler (simple TCP)

async fn serve_git_credentials(listener: tokio::net::TcpListener) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => { eprintln!("git accept error: {e}"); continue; }
        };

        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = match stream.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => return,
            };
            let body = String::from_utf8_lossy(&buf[..n]);

            if !body.contains("POST") || !body.contains("/git-credential") {
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\n\r\n").await;
                return;
            }

            let mut host = String::new();
            if let Some(body_start) = body.find("\r\n\r\n") {
                for line in body[body_start + 4..].lines() {
                    if let Some((k, v)) = line.split_once('=') {
                        if k.trim() == "host" {
                            host = v.trim().to_string();
                        }
                    }
                }
            }

            let (username, env_var) = match host.as_str() {
                "github.com" => ("x-access-token", "GITHUB_TOKEN"),
                "gitlab.com" => ("oauth2", "GITLAB_TOKEN"),
                "bitbucket.org" => ("x-token-auth", "BITBUCKET_TOKEN"),
                _ => {
                    let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
                    return;
                }
            };

            let token = match std::env::var(env_var) {
                Ok(t) => t,
                Err(_) => {
                    let _ = stream.write_all(b"HTTP/1.1 500 Error\r\n\r\n").await;
                    return;
                }
            };

            let response_body = format!("username={username}\npassword={token}\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n{response_body}",
                response_body.len(),
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

// VM lifecycle API types

#[allow(dead_code)] // fields consumed by serde
#[derive(serde::Deserialize)]
struct UpRequest {
    name: String,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    create: bool,
    #[serde(default)]
    config: crate::config::CellaConfig,
}

#[derive(serde::Serialize)]
struct UpResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct NameRequest {
    name: String,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct FlowStartRequest {
    name: String,
    flow: String,
    #[serde(default)]
    params: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct FlowLogsRequest {
    name: String,
    #[serde(default)]
    follow: bool,
    #[serde(default = "default_lines")]
    lines: u32,
}

fn default_lines() -> u32 { 100 }

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct FlowInfo {
    flow_name: String,
    current_op: String,
    state: String,
    started_at: u64,
    op_started_at: u64,
}

#[derive(serde::Serialize)]
struct CellStatus {
    name: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flow: Option<FlowInfo>,
}

async fn handle_prepare(req: &str) -> (&'static str, String) {
    let body = match req.find("\r\n\r\n") {
        Some(i) => &req[i + 4..],
        None => return ("400 Bad Request", "missing body".to_string()),
    };
    let up: UpRequest = match serde_json::from_str(body) {
        Ok(u) => u,
        Err(e) => return ("400 Bad Request", format!("bad request: {e}")),
    };

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        crate::git::init_clone_server(&up.name, &up.config)?;
        Ok(())
    }).await;

    match result {
        Ok(Ok(())) => ("200 OK", r#"{"ok":true}"#.to_string()),
        Ok(Err(e)) => {
            let resp = format!("{{\"ok\":false,\"error\":\"{}\"}}", e);
            ("500 Internal Server Error", resp)
        }
        Err(e) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
    }
}

async fn handle_up(req: &str) -> (&'static str, String) {
    let body = match req.find("\r\n\r\n") {
        Some(i) => &req[i + 4..],
        None => return ("400 Bad Request", "missing body".to_string()),
    };
    let up: UpRequest = match serde_json::from_str(body) {
        Ok(u) => u,
        Err(e) => return ("400 Bad Request", format!("bad request: {e}")),
    };

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        crate::git::init_clone_server(&up.name, &up.config)?;
        if !crate::vm::is_running(&up.name)? {
            let repo_name = up.repo.as_deref().unwrap_or("unknown");
            crate::vm::start(&up.name, repo_name, &up.config)?;
        }
        let rt = crate::vm::runtime_dir(&up.name);
        let ip = std::fs::read_to_string(rt.join("ip")).unwrap_or_default().trim().to_string();
        Ok(ip)
    }).await;

    match result {
        Ok(Ok(ip)) => {
            let resp = UpResponse { ok: true, ip: Some(ip), error: None };
            ("200 OK", serde_json::to_string(&resp).unwrap())
        }
        Ok(Err(e)) => {
            let resp = UpResponse { ok: false, ip: None, error: Some(e.to_string()) };
            ("500 Internal Server Error", serde_json::to_string(&resp).unwrap())
        }
        Err(e) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
    }
}

async fn handle_down(req: &str) -> (&'static str, String) {
    let body = match req.find("\r\n\r\n") {
        Some(i) => &req[i + 4..],
        None => return ("400 Bad Request", "missing body".to_string()),
    };
    let nr: NameRequest = match serde_json::from_str(body) {
        Ok(n) => n,
        Err(e) => return ("400 Bad Request", format!("bad request: {e}")),
    };

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        if crate::vm::is_running(&nr.name)? {
            crate::vm::stop(&nr.name)?;
        }
        Ok(())
    }).await;

    match result {
        Ok(Ok(())) => ("200 OK", r#"{"ok":true}"#.to_string()),
        Ok(Err(e)) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
        Err(e) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
    }
}

async fn handle_delete(req: &str) -> (&'static str, String) {
    let body = match req.find("\r\n\r\n") {
        Some(i) => &req[i + 4..],
        None => return ("400 Bad Request", "missing body".to_string()),
    };
    let nr: NameRequest = match serde_json::from_str(body) {
        Ok(n) => n,
        Err(e) => return ("400 Bad Request", format!("bad request: {e}")),
    };

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        crate::vm::delete(&nr.name)?;
        Ok(())
    }).await;

    match result {
        Ok(Ok(())) => ("200 OK", r#"{"ok":true}"#.to_string()),
        Ok(Err(e)) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
        Err(e) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
    }
}

async fn handle_list() -> (&'static str, String) {
    let result = tokio::task::spawn_blocking(|| -> anyhow::Result<Vec<CellStatus>> {
        let clones = crate::vm::list_cells()?;
        let mut cells = Vec::new();
        for name in clones {
            let running = crate::vm::is_running(&name).unwrap_or(false);
            let rt = crate::vm::runtime_dir(&name);
            let ip = if running {
                std::fs::read_to_string(rt.join("ip")).ok().map(|s| s.trim().to_string())
            } else {
                None
            };
            let repo = std::fs::read_to_string(rt.join("repo"))
                .ok().map(|s| s.trim().to_string());
            let flow = {
                let path = crate::cell::cell_dir(&name).join("flow-status.json");
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<FlowInfo>(&s).ok())
            };
            cells.push(CellStatus {
                name,
                status: if running { "running" } else { "stopped" }.to_string(),
                ip,
                repo,
                flow,
            });
        }
        Ok(cells)
    }).await;

    match result {
        Ok(Ok(cells)) => ("200 OK", serde_json::to_string(&cells).unwrap()),
        Ok(Err(e)) => ("500 Internal Server Error", format!("{{\"error\":\"{e}\"}}")),
        Err(e) => ("500 Internal Server Error", format!("{{\"error\":\"{e}\"}}")),
    }
}

async fn handle_flow_start(req: &str) -> (&'static str, String) {
    let body = match req.find("\r\n\r\n") {
        Some(i) => &req[i + 4..],
        None => return ("400 Bad Request", "missing body".to_string()),
    };
    let fr: FlowStartRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return ("400 Bad Request", format!("bad request: {e}")),
    };

    let name = fr.name;
    let flow = fr.flow;
    let params = fr.params;
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let (_, target) = crate::vm::ssh_target(&name)?;
        let repo_name = std::fs::read_to_string(crate::vm::runtime_dir(&name).join("repo"))
            .unwrap_or_else(|_| "cell".to_string())
            .trim().to_string();
        let workspace = format!("/{repo_name}");
        let inner_cmd = match params {
            Some(ref p) => format!("cellx flow run {flow} --params {}", crate::exec::shell_escape(p)),
            None => format!("cellx flow run {flow}"),
        };
        let detached = crate::exec::detached(&inner_cmd, "/tmp/cellx/flow.log");
        let script = format!("cd {} && {}", crate::exec::shell_escape(&workspace), detached);

        eprintln!("flow-start: target={target} cmd={script}");

        // fire-and-forget: spawn SSH, don't wait for exit
        // SSH already runs the command through a remote shell, so no sh -c wrapper needed
        std::process::Command::new("ssh")
            .args([
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR",
                &target,
                &script,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to start flow: {e}"))?;

        Ok(())
    }).await;

    match result {
        Ok(Ok(())) => ("200 OK", r#"{"ok":true}"#.to_string()),
        Ok(Err(e)) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
        Err(e) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
    }
}

async fn handle_flow_stop(req: &str) -> (&'static str, String) {
    let body = match req.find("\r\n\r\n") {
        Some(i) => &req[i + 4..],
        None => return ("400 Bad Request", "missing body".to_string()),
    };
    let nr: NameRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return ("400 Bad Request", format!("bad request: {e}")),
    };

    let name = nr.name;
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let (_, target) = crate::vm::ssh_target(&name)?;
        // fire-and-forget: cellx flow done writes a result file, quick operation
        std::process::Command::new("ssh")
            .args([
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR",
                "-o", "ConnectTimeout=5",
                &target,
                "cellx flow done",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .status()
            .ok();
        Ok(())
    }).await;

    match result {
        Ok(Ok(())) => ("200 OK", r#"{"ok":true}"#.to_string()),
        Ok(Err(e)) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
        Err(e) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
    }
}

async fn handle_flow_pause(req: &str) -> (&'static str, String) {
    let body = match req.find("\r\n\r\n") {
        Some(i) => &req[i + 4..],
        None => return ("400 Bad Request", "missing body".to_string()),
    };
    let nr: NameRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return ("400 Bad Request", format!("bad request: {e}")),
    };

    let name = nr.name;
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let (_, target) = crate::vm::ssh_target(&name)?;
        std::process::Command::new("ssh")
            .args([
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR",
                "-o", "ConnectTimeout=5",
                &target,
                "cellx flow pause",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .status()
            .ok();
        Ok(())
    }).await;

    match result {
        Ok(Ok(())) => ("200 OK", r#"{"ok":true}"#.to_string()),
        Ok(Err(e)) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
        Err(e) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
    }
}

async fn handle_flow_logs(req: &str) -> (&'static str, String) {
    let body = match req.find("\r\n\r\n") {
        Some(i) => &req[i + 4..],
        None => return ("400 Bad Request", "missing body".to_string()),
    };
    let lr: FlowLogsRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return ("400 Bad Request", format!("bad request: {e}")),
    };

    let name = lr.name;
    let lines = lr.lines;
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let (_, target) = crate::vm::ssh_target(&name)?;
        let output = std::process::Command::new("ssh")
            .args([
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR",
                &target,
                &format!("tail -{lines} /tmp/cellx/flow.log 2>/dev/null"),
            ])
            .output()
            .map_err(|e| anyhow::anyhow!("ssh failed: {e}"))?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }).await;

    match result {
        Ok(Ok(content)) => {
            let resp = serde_json::json!({"ok": true, "content": content});
            ("200 OK", resp.to_string())
        }
        Ok(Err(e)) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
        Err(e) => ("500 Internal Server Error", format!("{{\"ok\":false,\"error\":\"{e}\"}}")),
    }
}

/// Stream flow logs (follow mode) — writes directly to the TCP stream
async fn handle_flow_logs_follow(
    stream: &mut tokio::net::TcpStream,
    req: &str,
) {
    use tokio::io::AsyncWriteExt;

    let body = match req.find("\r\n\r\n") {
        Some(i) => &req[i + 4..],
        None => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 12\r\n\r\nmissing body").await;
            return;
        }
    };
    let lr: FlowLogsRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 11\r\n\r\nbad request").await;
            return;
        }
    };

    let name = lr.name.clone();
    let target = match tokio::task::spawn_blocking(move || crate::vm::ssh_target(&name)).await {
        Ok(Ok((_, t))) => t,
        _ => {
            let _ = stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 16\r\n\r\ncannot reach cell").await;
            return;
        }
    };

    // send chunked transfer header
    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n").await;

    // spawn tail -f and pipe output
    let mut child = match tokio::process::Command::new("ssh")
        .args([
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR",
            &target,
            "tail -f /tmp/cellx/flow.log 2>/dev/null & TAIL=$!; while kill -0 $TAIL 2>/dev/null; do if [ ! -f /tmp/cellx/flow.json ]; then kill $TAIL 2>/dev/null; break; fi; sleep 1; done; wait $TAIL 2>/dev/null",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    if let Some(mut stdout) = child.stdout.take() {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 4096];
        loop {
            let n = match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = format!("{:x}\r\n", n);
            if stream.write_all(chunk.as_bytes()).await.is_err() { break; }
            if stream.write_all(&buf[..n]).await.is_err() { break; }
            if stream.write_all(b"\r\n").await.is_err() { break; }
        }
    }

    let _ = stream.write_all(b"0\r\n\r\n").await;
    child.kill().await.ok();
}

async fn handle_flow_status(req: &str) -> (&'static str, String) {
    let body = match req.find("\r\n\r\n") {
        Some(i) => &req[i + 4..],
        None => return ("400 Bad Request", "missing body".to_string()),
    };
    let report: flow::FlowReport = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => return ("400 Bad Request", format!("bad request: {e}")),
    };

    // resolve cell name from IP
    let cells = load_cells();
    let cell_name = cells.iter()
        .find(|c| c.cell_ip == report.cell_ip)
        .map(|c| c.branch_id.clone());

    if let Some(name) = cell_name {
        let info = FlowInfo {
            flow_name: report.flow_name,
            current_op: report.current_op,
            state: serde_json::to_value(&report.state)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown".to_string()),
            started_at: report.started_at,
            op_started_at: report.op_started_at,
        };
        let path = crate::cell::cell_dir(&name).join("flow-status.json");
        let json = serde_json::to_string_pretty(&info).unwrap_or_default();
        if let Err(e) = std::fs::write(&path, json) {
            eprintln!("warning: failed to write flow status for {name}: {e}");
        }
    }

    ("200 OK", r#"{"ok":true}"#.to_string())
}

// Control API — manages dynamic cell registrations, writes to file for mitmproxy

async fn serve_control_api(listener: tokio::net::TcpListener) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => { eprintln!("control accept error: {e}"); continue; }
        };

        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            let n = match stream.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => return,
            };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();

            // streaming endpoint — writes directly to stream, then returns
            if req.starts_with("POST /flow/logs") {
                if let Some(body_start) = req.find("\r\n\r\n") {
                    let body = &req[body_start + 4..];
                    if body.contains("\"follow\":true") || body.contains("\"follow\": true") {
                        handle_flow_logs_follow(&mut stream, &req).await;
                        return;
                    }
                }
            }

            let (status, body) = if req.starts_with("POST /flow/start") {
                handle_flow_start(&req).await
            } else if req.starts_with("POST /flow/stop") {
                handle_flow_stop(&req).await
            } else if req.starts_with("POST /flow/pause") {
                handle_flow_pause(&req).await
            } else if req.starts_with("POST /flow/logs") {
                handle_flow_logs(&req).await
            } else if req.starts_with("POST /cells") {
                if let Some(body_start) = req.find("\r\n\r\n") {
                    let json = &req[body_start + 4..];
                    match serde_json::from_str::<CellRules>(json) {
                        Ok(rules) => {
                            let ip = rules.cell_ip.clone();
                            let branch = rules.branch_id.clone();
                            let mut cells = load_cells();
                            cells.retain(|c| c.cell_ip != ip);
                            cells.push(rules);
                            save_cells(&cells);
                            eprintln!("registered cell {ip} (branch: {branch})");
                            ("200 OK", "ok".to_string())
                        }
                        Err(e) => ("400 Bad Request", format!("bad request: {e}")),
                    }
                } else {
                    ("400 Bad Request", "missing body".to_string())
                }
            } else if req.starts_with("DELETE /cells/") {
                let first_line = req.lines().next().unwrap_or("");
                let path = first_line.split_whitespace().nth(1).unwrap_or("");
                let ip = path.strip_prefix("/cells/").unwrap_or("");
                if ip.is_empty() {
                    ("400 Bad Request", "missing ip".to_string())
                } else {
                    let mut cells = load_cells();
                    cells.retain(|c| c.cell_ip != ip);
                    save_cells(&cells);
                    eprintln!("deregistered cell {ip}");
                    ("200 OK", "ok".to_string())
                }
            } else if req.starts_with("GET /cells") {
                let cells = load_cells();
                let json = serde_json::to_string(&cells).unwrap_or_default();
                ("200 OK", json)
            } else if req.starts_with("POST /prepare") {
                handle_prepare(&req).await
            } else if req.starts_with("POST /up") {
                handle_up(&req).await
            } else if req.starts_with("POST /down") {
                handle_down(&req).await
            } else if req.starts_with("POST /delete") {
                handle_delete(&req).await
            } else if req.starts_with("POST /flow-status") {
                handle_flow_status(&req).await
            } else if req.starts_with("GET /list") {
                handle_list().await
            } else {
                ("404 Not Found", "not found".to_string())
            };

            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

// Sweep — stop stale cells where the op has exceeded the server timeout

async fn sweep_stale_cells(timeout: u64) {
    let now = flow::now_secs();
    let cells = match crate::vm::list_cells() {
        Ok(c) => c,
        Err(_) => return,
    };

    for name in cells {
        if !crate::vm::is_running(&name).unwrap_or(false) {
            continue;
        }

        let status_path = crate::cell::cell_dir(&name).join("flow-status.json");
        let info: Option<FlowInfo> = std::fs::read_to_string(&status_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());

        if let Some(info) = info {
            if info.state == "running" && info.op_started_at > 0 {
                let elapsed = now.saturating_sub(info.op_started_at);
                if elapsed > timeout {
                    eprintln!("sweep: stopping stale cell '{}' (op '{}' running for {}s, timeout {}s)",
                        name, info.current_op, elapsed, timeout);
                    crate::vm::stop(&name).ok();
                }
            }
        }
    }
}

async fn run_sweep_loop(interval: u64, timeout: u64) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval));
    ticker.tick().await; // skip first immediate tick
    loop {
        ticker.tick().await;
        sweep_stale_cells(timeout).await;
    }
}

// Main entry point

pub async fn run(config_path: &str) -> Result<()> {
    let config: ProxyConfig = {
        let content = std::fs::read_to_string(config_path)
            .context("reading proxy config")?;
        serde_json::from_str(&content)
            .context("parsing proxy config")?
    };

    let log_dir = Path::new(&config.log_file).parent().unwrap();
    std::fs::create_dir_all(log_dir).ok();

    // initialize cells.json from static config
    save_cells(&config.cells);

    // Git credential server
    let git_addr: SocketAddr = format!("{}:{}", config.bind_address, config.git_credential_port).parse()?;
    let git_listener = tokio::net::TcpListener::bind(git_addr).await?;
    eprintln!("Git credential server listening on {git_addr}");
    tokio::spawn(serve_git_credentials(git_listener));

    // Control API — localhost (for SSH tunnels) + bridge (for VMs)
    let ctrl_local: SocketAddr = format!("127.0.0.1:{}", config.control_port).parse()?;
    let ctrl_bridge: SocketAddr = format!("{}:{}", config.bind_address, config.control_port).parse()?;
    let ctrl_listener_local = tokio::net::TcpListener::bind(ctrl_local).await?;
    let ctrl_listener_bridge = tokio::net::TcpListener::bind(ctrl_bridge).await?;
    eprintln!("Control API listening on {ctrl_local} + {ctrl_bridge}");
    tokio::spawn(serve_control_api(ctrl_listener_local));
    tokio::spawn(serve_control_api(ctrl_listener_bridge));

    // Sweep loop — stops stale cells
    tokio::spawn(run_sweep_loop(config.sweep_interval, config.sweep_timeout));

    eprintln!("Cella services started");
    eprintln!("  Git credentials: {git_addr}");
    eprintln!("  Control API: {ctrl_local} + {ctrl_bridge}");

    // keep running
    tokio::signal::ctrl_c().await?;
    Ok(())
}
