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
    #[serde(rename = "nucleusEnabled", default)]
    pub nucleus_enabled: bool,
}

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
    #[serde(rename = "nucleusAllowedDomains", default)]
    pub nucleus_allowed_domains: Vec<String>,
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

#[derive(serde::Serialize)]
struct CellStatus {
    name: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip: Option<String>,
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
            let ip = if running {
                let rt = crate::vm::runtime_dir(&name);
                std::fs::read_to_string(rt.join("ip")).ok().map(|s| s.trim().to_string())
            } else {
                None
            };
            cells.push(CellStatus {
                name,
                status: if running { "running" } else { "stopped" }.to_string(),
                ip,
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

            let (status, body) = if req.starts_with("POST /cells") {
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

    // Control API (localhost only)
    let ctrl_addr: SocketAddr = format!("127.0.0.1:{}", config.control_port).parse()?;
    let ctrl_listener = tokio::net::TcpListener::bind(ctrl_addr).await?;
    eprintln!("Control API listening on {ctrl_addr}");
    tokio::spawn(serve_control_api(ctrl_listener));

    eprintln!("Cella services started");
    eprintln!("  Git credentials: {git_addr}");
    eprintln!("  Control API: {ctrl_addr}");

    // keep running
    tokio::signal::ctrl_c().await?;
    Ok(())
}
