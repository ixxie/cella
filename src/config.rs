use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct CellaConfig {
    #[serde(default = "default_memory")]
    pub memory: String,
    #[serde(default = "default_vcpu")]
    pub vcpu: u32,
    #[serde(default = "default_timeout")]
    pub shell_timeout: u64,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub post_push: Option<String>,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub proxy: Vec<ProxyRule>,
    #[serde(default)]
    pub nucleus: NucleusConfig,
    #[serde(default)]
    pub secrets: SecretsConfig,
    // legacy — use post_push instead
    #[serde(default)]
    pub hooks: HooksConfig,
}

impl CellaConfig {
    pub fn post_push(&self) -> Option<&str> {
        self.post_push.as_deref()
            .or(self.hooks.post_push.as_deref())
    }
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct SessionConfig {
    pub command: Option<String>,
    pub on_exit: Option<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct HooksConfig {
    pub post_push: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct SecretsConfig {
    pub command: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct NucleusConfig {
    pub command: Option<String>,
    pub proxy_port: Option<u16>,
    #[serde(default, rename = "allowedDomains")]
    pub allowed_domains: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProxyRule {
    pub domain: String,
    pub mode: ProxyMode,
    pub secret: Option<String>,
    pub prefix: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    Live,
    Record,
    Replay,
}

fn default_memory() -> String { "2G".to_string() }
fn default_vcpu() -> u32 { 2 }
fn default_timeout() -> u64 { 300 }

impl Default for CellaConfig {
    fn default() -> Self {
        Self {
            memory: default_memory(),
            vcpu: default_vcpu(),
            shell_timeout: default_timeout(),
            ports: Vec::new(),
            post_push: None,
            session: SessionConfig::default(),
            proxy: Vec::new(),
            nucleus: NucleusConfig::default(),
            secrets: SecretsConfig::default(),
            hooks: HooksConfig::default(),
        }
    }
}

pub fn load(repo_root: &Path) -> Result<CellaConfig> {
    let new_path = repo_root.join(".cella/config.toml");
    let legacy_path = repo_root.join("cella.toml");
    let config_path = if new_path.exists() { new_path } else { legacy_path };
    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .context("reading config")?;
        toml::from_str(&content).context("parsing config")
    } else {
        Ok(CellaConfig::default())
    }
}
