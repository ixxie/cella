use anyhow::{Context, Result};
use std::io::Write;
use tracing::{warn, instrument};

use crate::config::CellaConfig;
use crate::ssh;

const CONTROL_PORT: u32 = 8082;

pub struct Client {
    session: ssh::Session,
    rt: tokio::runtime::Runtime,
    user_host: String,
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
pub struct UpResponse {
    pub ok: bool,
    pub ip: Option<String>,
    pub error: Option<String>,
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
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
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to create tokio runtime")?;

        let session = rt.block_on(async {
            ssh::Session::connect(user_host).await
        }).context("SSH connection failed")?;

        Ok(Self {
            session,
            rt,
            user_host: user_host.to_string(),
        })
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<String> {
        self.rt.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(300),
                self.session.http_request("127.0.0.1", CONTROL_PORT, method, path, body),
            )
            .await
            .context("control API request timed out")?
        })
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
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let mut seen_lines = 0usize;

        loop {
            let content = self.flow_logs(name, 500)?;
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();

            if total > seen_lines {
                for line in &lines[seen_lines..] {
                    writeln!(out, "{line}").ok();
                }
                out.flush().ok();
                seen_lines = total;
            }

            // Check if flow is still running
            let status = self.list()?;
            let cell = status.iter().find(|c| c.name == name);
            match cell {
                Some(c) => {
                    if let Some(ref f) = c.flow {
                        if f.state != "running" && f.state != "paused" {
                            // Print remaining lines
                            let content = self.flow_logs(name, 500)?;
                            let lines: Vec<&str> = content.lines().collect();
                            for line in &lines[seen_lines..] {
                                writeln!(out, "{line}").ok();
                            }
                            break;
                        }
                    } else {
                        break; // no flow running
                    }
                }
                None => break,
            }

            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        Ok(())
    }

    /// SSH hop for shell or command execution (still uses subprocess SSH for TTY support)
    pub fn shell(&self, name: &str, command: Option<&str>) -> Result<()> {
        let target = &self.user_host;
        let remote_cmd = match command {
            Some(cmd) => crate::exec::cella_hop(name, cmd),
            None => format!("cella shell --server {}", name),
        };
        let status = std::process::Command::new("ssh")
            .args([
                "-t", "-A",
                "-o", "StrictHostKeyChecking=no",
                "-o", "UserKnownHostsFile=/dev/null",
                "-o", "ServerAliveInterval=30",
                "-o", "ServerAliveCountMax=3",
                target,
                &remote_cmd,
            ])
            .status()
            .context("ssh shell failed")?;
        if !status.success() {
            anyhow::bail!("shell exited with {}", status);
        }
        Ok(())
    }

    /// Sync local files to the cell on the server
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

            let rel = if path.starts_with("~/") {
                &path[2..]
            } else if let Ok(stripped) = local.strip_prefix(&home) {
                stripped.to_str().unwrap_or(path)
            } else {
                continue;
            };

            let dest = format!("/var/lib/cella/cells/{name}/sync/{rel}");

            // Use SSH exec to create dir and write file content
            self.rt.block_on(async {
                if let Some(parent) = std::path::Path::new(&dest).parent() {
                    self.session.exec(&format!("mkdir -p '{}'", parent.display())).await.ok();
                }

                if local.is_file() {
                    let content = std::fs::read(&expanded).unwrap_or_default();
                    let cmd = format!("cat > '{dest}'");
                    if let Err(e) = self.session.exec_detached(&cmd, Some(&content)).await {
                        warn!(error = %e, path = %expanded, "file sync failed");
                    }
                }
                // TODO: directory sync via tar pipe
            });
        }

        // fix ownership
        self.rt.block_on(async {
            self.session.exec(&format!(
                "chown -R 1000:users /var/lib/cella/cells/{name}/sync/ 2>/dev/null"
            )).await.ok();
        });

        Ok(())
    }
}
