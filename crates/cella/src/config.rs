use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct CellaConfig {
    #[serde(default = "default_memory")]
    pub memory: String,
    #[serde(default = "default_vcpu")]
    pub vcpu: u32,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub post_push: Option<String>,
    #[serde(default)]
    pub proxy: Vec<ProxyRule>,
    #[serde(default)]
    pub secrets: SecretsConfig,
    #[serde(default)]
    pub egress: CellEgressConfig,
    // legacy — use post_push instead
    #[serde(default)]
    pub hooks: HooksConfig,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct HooksConfig {
    pub post_push: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct SecretsConfig {
    pub command: Option<String>,
    pub recipient: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct CellEgressConfig {
    #[serde(default)]
    pub writes: Option<CellEgressRules>,
    #[serde(default)]
    pub reads: Option<CellEgressRules>,
    #[serde(default)]
    pub credentials: Vec<CredentialConfig>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct CellEgressRules {
    pub allowed: Option<Vec<String>>,
    pub denied: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CredentialConfig {
    pub host: String,
    pub header: String,
    pub env_var: String,
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

impl Default for CellaConfig {
    fn default() -> Self {
        Self {
            memory: default_memory(),
            vcpu: default_vcpu(),
            ports: Vec::new(),
            server: None,
            post_push: None,
            proxy: Vec::new(),
            secrets: SecretsConfig::default(),
            egress: CellEgressConfig::default(),
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
