use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use tracing::{warn, instrument};

use crate::config::CellaConfig;

const CONTROL_PORT: u16 = 8082;

pub struct Client {
    user_host: String,
    local_port: u16,
    control_socket: String,
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
pub struct UpResponse {
    pub ok: bool,
    pub ip: Option<String>,
    pub error: Option<String>,
}

#[derive(serde::Deserialize)]
#[allow(dead_code)] // all fields are part of the API contract
pub struct FlowInfo {
    pub flow_name: String,
    pub current_op: String,
    pub state: String,
    pub started_at: u64,
    pub op_started_at: u64,
}

#[derive(serde::Deserialize)]
pub struct CellStatus {
    pub name: String,
    pub status: String,
    pub ip: Option<String>,
    pub repo: Option<String>,
    pub flow: Option<FlowInfo>,
}

impl Client {
    pub fn user_host(&self) -> &str {
        &self.user_host
    }

    #[instrument]
    pub fn connect(user_host: &str) -> Result<Self> {
        let local_port = 18082u16;
        let control_socket = format!(
            "/tmp/cella-ssh-{}",
            user_host.replace('@', "-").replace('.', "-")
        );

        // check if master connection already exists
        let check = Command::new("ssh")
            .args(["-O", "check", "-S", &control_socket, user_host])
            .output();
        let master_exists = check.map(|o| o.status.success()).unwrap_or(false);

        if !master_exists {
            // open master connection with tunnel
            let status = Command::new("ssh")
                .args([
                    "-f", "-N", "-A",
                    "-M", "-S", &control_socket,
                    "-o", "ControlPersist=10m",
                    "-o", "ServerAliveInterval=30",
                    "-o", "ServerAliveCountMax=3",
                    "-L", &format!("{local_port}:127.0.0.1:{CONTROL_PORT}"),
                    user_host,
                ])
                .status()
                .context("failed to open SSH tunnel")?;
            if !status.success() {
                anyhow::bail!("failed to open SSH tunnel to {user_host}");
            }

            // wait for tunnel to be ready
            for _ in 0..20 {
                if TcpStream::connect(format!("127.0.0.1:{local_port}")).is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }

        Ok(Self {
            user_host: user_host.to_string(),
            local_port,
            control_socket,
        })
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<String> {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", self.local_port))
            .context("failed to connect to control API (is the tunnel open?)")?;

        let body_bytes = body.unwrap_or("");
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body_bytes}",
            body_bytes.len(),
        );
        stream.write_all(req.as_bytes())?;

        let mut response = String::new();
        stream.read_to_string(&mut response)?;

        // extract body after headers
        if let Some(i) = response.find("\r\n\r\n") {
            Ok(response[i + 4..].to_string())
        } else {
            Ok(response)
        }
    }

    pub fn prepare(&self, name: &str, repo: &str, config: &CellaConfig) -> Result<()> {
        let body = serde_json::json!({
            "name": name,
            "repo": repo,
            "config": config,
        });
        let resp = self.request("POST", "/prepare", Some(&body.to_string()))?;
        if !resp.contains("\"ok\":true") {
            anyhow::bail!("server error: {resp}");
        }
        Ok(())
    }

    pub fn up(&self, name: &str, repo: &str, create: bool, config: &CellaConfig) -> Result<UpResponse> {
        let body = serde_json::json!({
            "name": name,
            "repo": repo,
            "create": create,
            "config": config,
        });
        let resp = self.request("POST", "/up", Some(&body.to_string()))?;
        let up: UpResponse = serde_json::from_str(&resp)
            .context(format!("bad response from server: {resp}"))?;
        if !up.ok {
            anyhow::bail!("server error: {}", up.error.unwrap_or_default());
        }
        Ok(up)
    }

    pub fn down(&self, name: &str) -> Result<()> {
        let body = serde_json::json!({ "name": name });
        let resp = self.request("POST", "/down", Some(&body.to_string()))?;
        if !resp.contains("\"ok\":true") {
            anyhow::bail!("server error: {resp}");
        }
        Ok(())
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let body = serde_json::json!({ "name": name });
        let resp = self.request("POST", "/delete", Some(&body.to_string()))?;
        if !resp.contains("\"ok\":true") {
            anyhow::bail!("server error: {resp}");
        }
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<CellStatus>> {
        let resp = self.request("GET", "/list", None)?;
        serde_json::from_str(&resp).context(format!("bad response: {resp}"))
    }

    // Flow management

    pub fn flow_start(&self, name: &str, flow: &str, params: Option<&str>) -> Result<()> {
        let mut body = serde_json::json!({ "name": name, "flow": flow });
        if let Some(p) = params {
            body["params"] = serde_json::Value::String(p.to_string());
        }
        let resp = self.request("POST", "/flow/start", Some(&body.to_string()))?;
        if !resp.contains("\"ok\":true") {
            anyhow::bail!("flow start failed: {resp}");
        }
        Ok(())
    }

    pub fn flow_stop(&self, name: &str) -> Result<()> {
        let body = serde_json::json!({ "name": name });
        let resp = self.request("POST", "/flow/stop", Some(&body.to_string()))?;
        if !resp.contains("\"ok\":true") {
            anyhow::bail!("flow stop failed: {resp}");
        }
        Ok(())
    }

    pub fn flow_pause(&self, name: &str) -> Result<()> {
        let body = serde_json::json!({ "name": name });
        let resp = self.request("POST", "/flow/pause", Some(&body.to_string()))?;
        if !resp.contains("\"ok\":true") {
            anyhow::bail!("flow pause failed: {resp}");
        }
        Ok(())
    }

    pub fn flow_logs(&self, name: &str, lines: u32) -> Result<String> {
        let body = serde_json::json!({ "name": name, "follow": false, "lines": lines });
        let resp = self.request("POST", "/flow/logs", Some(&body.to_string()))?;
        let v: serde_json::Value = serde_json::from_str(&resp)
            .context("bad flow logs response")?;
        Ok(v["content"].as_str().unwrap_or("").to_string())
    }

    pub fn flow_logs_follow(&self, name: &str) -> Result<()> {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", self.local_port))
            .context("failed to connect to control API")?;

        let body = serde_json::json!({ "name": name, "follow": true });
        let body_str = body.to_string();
        let req = format!(
            "POST /flow/logs HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body_str}",
            body_str.len(),
        );
        stream.write_all(req.as_bytes())?;

        // skip HTTP headers
        let mut header_buf = Vec::new();
        let mut b = [0u8; 1];
        loop {
            if stream.read(&mut b)? == 0 { return Ok(()); }
            header_buf.push(b[0]);
            if header_buf.ends_with(b"\r\n\r\n") { break; }
        }

        // parse chunked transfer encoding
        use std::io::{BufRead, BufReader, Write};
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let mut reader = BufReader::new(stream);

        loop {
            // read chunk size line
            let mut size_line = String::new();
            if reader.read_line(&mut size_line).unwrap_or(0) == 0 {
                break;
            }
            let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
            if size == 0 {
                break;
            }

            // read chunk data
            let mut chunk = vec![0u8; size];
            if reader.read_exact(&mut chunk).is_err() {
                break;
            }
            out.write_all(&chunk).ok();
            out.flush().ok();

            // consume trailing \r\n
            let mut crlf = [0u8; 2];
            reader.read_exact(&mut crlf).ok();
        }
        Ok(())
    }

    /// SSH hop for shell or command execution
    pub fn shell(&self, name: &str, command: Option<&str>) -> Result<()> {
        let remote_cmd = match command {
            Some(cmd) => crate::exec::cella_hop(name, cmd),
            None => format!("cella shell --server {}", name),
        };
        let mut args = vec![
            "-t".to_string(),
        ];
        args.extend([
            "-A".to_string(),
            "-o".to_string(), "ServerAliveInterval=30".to_string(),
            "-o".to_string(), "ServerAliveCountMax=3".to_string(),
            "-S".to_string(), self.control_socket.clone(),
            self.user_host.clone(),
            remote_cmd,
        ]);
        let status = Command::new("ssh")
            .args(&args)
            .status()
            .context("ssh shell failed")?;
        if !status.success() {
            anyhow::bail!("shell exited with {}", status);
        }
        Ok(())
    }

    /// Sync local files to the cell's home directory on the server VM
    #[instrument(skip(self, paths))]
    pub fn sync_files(&self, name: &str, paths: &[String]) -> Result<()> {
        let home = std::env::var("HOME").unwrap_or_default();

        for path in paths {
            let expanded = if path.starts_with("~/") {
                format!("{}/{}", home, &path[2..])
            } else {
                path.clone()
            };

            let local = std::path::Path::new(&expanded);
            if !local.exists() {
                continue;
            }

            // preserve relative path from home
            let rel = if path.starts_with("~/") {
                &path[2..]
            } else if let Ok(stripped) = local.strip_prefix(&home) {
                stripped.to_str().unwrap_or(path)
            } else {
                continue;
            };

            let dest = format!("/var/lib/cella/cells/{name}/sync/{rel}");

            // ensure parent dir exists on server
            if let Some(parent) = std::path::Path::new(&dest).parent() {
                Command::new("ssh")
                    .args([
                        "-S", &self.control_socket,
                        &self.user_host,
                        &format!("mkdir -p '{}'", parent.display()),
                    ])
                    .output()
                    .map_err(|e| warn!(error = %e, "failed to create dir on server"))
                    .ok();
            }

            // scp through the master connection
            let src = if local.is_dir() {
                format!("{}/", expanded) // trailing slash for rsync-like behavior
            } else {
                expanded.clone()
            };

            let mut cmd = Command::new("scp");
            cmd.args(["-o", &format!("ControlPath={}", self.control_socket)]);
            if local.is_dir() {
                cmd.arg("-r");
            }
            cmd.arg(&src);
            cmd.arg(&format!("{}:{}", self.user_host, dest));
            let scp_result = cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            if let Err(e) = scp_result {
                warn!(error = %e, path = %expanded, "scp failed");
            }
        }

        // fix ownership so the VM user can read synced files
        let chown_cmd = format!("chown -R 1000:users /var/lib/cella/cells/{name}/sync/ 2>/dev/null");
        if let Err(e) = Command::new("ssh")
            .args([
                "-S", &self.control_socket,
                &self.user_host,
                &chown_cmd,
            ])
            .output() {
            warn!(error = %e, "failed to fix sync file ownership");
        }
        Ok(())
    }
}
