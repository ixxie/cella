use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

const CELLS_PATH: &str = "/var/lib/cella/cells";
const IP_POOL: &str = "/var/lib/cella/ip-pool.json";

// Cell state

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cell {
    pub name: String,
    pub state: CellState,
    pub ip: Option<String>,
    pub repo: String,
    pub user: String,
    pub autostop: AutostopState,
    #[serde(default)]
    pub started_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CellState {
    Created,
    Running,
    Stopped,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AutostopState {
    pub timeout: u64,
    pub stop_at: Option<u64>,
}

impl Cell {
    pub fn new(name: &str, repo: &str) -> Self {
        Self {
            name: name.to_string(),
            state: CellState::Created,
            ip: None,
            repo: repo.to_string(),
            user: String::new(),
            autostop: AutostopState::default(),
            started_at: None,
        }
    }

    // Persistence

    fn state_path(name: &str) -> PathBuf {
        PathBuf::from(CELLS_PATH).join(name).join("cell.json")
    }

    pub fn load(name: &str) -> Result<Self> {
        let path = Self::state_path(name);
        let content = std::fs::read_to_string(&path)
            .context("reading cell state")?;
        serde_json::from_str(&content).context("parsing cell state")
    }

    pub fn load_or_new(name: &str, repo: &str) -> Self {
        Self::load(name).unwrap_or_else(|_| Self::new(name, repo))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::state_path(&self.name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // atomic write: tmp file + rename
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &path)?;

        // backward compat: write runtime dir files
        self.write_runtime_compat();

        Ok(())
    }

    fn write_runtime_compat(&self) {
        let rt = runtime_dir(&self.name);
        std::fs::create_dir_all(&rt).ok();
        if let Some(ip) = &self.ip {
            std::fs::write(rt.join("ip"), ip).ok();
        }
        if !self.repo.is_empty() {
            std::fs::write(rt.join("repo"), &self.repo).ok();
        }
        if !self.user.is_empty() {
            std::fs::write(rt.join("user"), &self.user).ok();
        }
        std::fs::write(
            rt.join("autostop.timeout"),
            self.autostop.timeout.to_string(),
        ).ok();
        if let Some(stop_at) = self.autostop.stop_at {
            std::fs::write(rt.join("autostop.at"), stop_at.to_string()).ok();
        } else {
            std::fs::remove_file(rt.join("autostop.at")).ok();
        }
    }

    // State transitions

    pub fn mark_running(&mut self, ip: &str, user: &str) {
        self.state = CellState::Running;
        self.ip = Some(ip.to_string());
        self.user = user.to_string();
        self.started_at = now_secs();
    }

    pub fn mark_stopped(&mut self) {
        self.state = CellState::Stopped;
        self.autostop.stop_at = None;
        // keep IP allocated until delete
    }

    pub fn mark_deleted(&mut self) {
        self.state = CellState::Stopped;
        self.ip = None;
        self.autostop.stop_at = None;
    }

    // Autostop

    pub fn set_autostop_timeout(&mut self, timeout: u64) {
        self.autostop.timeout = timeout;
    }

    pub fn schedule_autostop(&mut self) {
        if let Some(now) = now_secs() {
            self.autostop.stop_at = Some(now + self.autostop.timeout);
        }
    }

    pub fn clear_autostop(&mut self) {
        self.autostop.stop_at = None;
    }

    pub fn autostop_remaining(&self) -> Option<u64> {
        let stop_at = self.autostop.stop_at?;
        let now = now_secs()?;
        if stop_at > now {
            Some(stop_at - now)
        } else {
            Some(0)
        }
    }

    pub fn is_autostop_expired(&self) -> bool {
        self.autostop_remaining() == Some(0)
    }
}

// IP pool

pub fn allocate_ip(name: &str) -> Result<String> {
    let mut pool = load_ip_pool();

    if let Some(ip) = pool.get(name) {
        return Ok(ip.clone());
    }

    let used: std::collections::HashSet<&str> = pool.values().map(|s| s.as_str()).collect();
    for i in 11..=254u16 {
        let ip = format!("192.168.83.{i}");
        if !used.contains(ip.as_str()) {
            pool.insert(name.to_string(), ip.clone());
            save_ip_pool(&pool);
            info!(ip = %ip, cell = %name, "allocated IP");
            return Ok(ip);
        }
    }

    anyhow::bail!("IP pool exhausted (244 cells max)")
}

pub fn release_ip(name: &str) {
    let mut pool = load_ip_pool();
    pool.remove(name);
    save_ip_pool(&pool);
}

fn load_ip_pool() -> HashMap<String, String> {
    std::fs::read_to_string(IP_POOL)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_ip_pool(pool: &HashMap<String, String>) {
    let json = serde_json::to_string_pretty(pool).unwrap_or_default();
    if let Err(e) = std::fs::write(IP_POOL, json) {
        warn!(error = %e, "failed to save IP pool");
    }
}

// Path helpers

pub fn cell_dir(name: &str) -> PathBuf {
    PathBuf::from(CELLS_PATH).join(name)
}

pub fn cell_repo_dir(name: &str) -> PathBuf {
    cell_dir(name).join("repo")
}

pub fn cell_flake_dir(name: &str) -> PathBuf {
    cell_dir(name).join("flake")
}

pub fn runtime_dir(name: &str) -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| "/tmp".to_string());
    let dir = Path::new(&base).join("cella").join(name);
    if !dir.join("ip").exists() {
        let fallback = Path::new("/tmp").join("cella").join(name);
        if fallback.join("ip").exists() {
            return fallback;
        }
    }
    dir
}

pub fn list_cells() -> Result<Vec<String>> {
    let dir = PathBuf::from(CELLS_PATH);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(s) = entry.file_name().to_str() {
                names.push(s.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

// Utilities

fn now_secs() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}
