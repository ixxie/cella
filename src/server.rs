use anyhow::{Context, Result};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

const REGISTRY_FILE: &str = ".config/cella/servers.toml";
const CLIENT_CONFIG: &str = ".config/cella/config.toml";
const ACTIVE_DIR: &str = ".config/cella/active";

// Client config

#[derive(Debug, Default, serde::Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub sync: Vec<String>,
}

pub fn load_client_config() -> ClientConfig {
    let path = home_dir().join(CLIENT_CONFIG);
    if !path.exists() {
        return ClientConfig::default();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub enum ActiveServer {
    Localhost,
    Remote { name: String },
}

impl ActiveServer {
    pub fn target(&self) -> Result<Option<String>> {
        match self {
            ActiveServer::Localhost => Ok(None),
            ActiveServer::Remote { name } => {
                let registry = load_registry()?;
                let entry = registry.get(name.as_str())
                    .ok_or_else(|| anyhow::anyhow!("server '{name}' not in registry"))?;
                Ok(Some(entry.target.clone()))
            }
        }
    }

    pub fn is_server(&self) -> bool {
        matches!(self, ActiveServer::Remote { .. })
    }

    pub fn remote_url(&self) -> Result<String> {
        match self.target()? {
            Some(t) => Ok(format!("cella://{t}")),
            None => Ok("cella://localhost".to_string()),
        }
    }
}

// Server registry

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct ServerEntry {
    pub target: String,
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".to_string()))
}

fn registry_path() -> PathBuf {
    home_dir().join(REGISTRY_FILE)
}

pub fn load_registry() -> Result<HashMap<String, ServerEntry>> {
    let path = registry_path();
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = std::fs::read_to_string(&path)
        .context("reading server registry")?;
    toml::from_str(&content).context("parsing server registry")
}

fn save_registry(registry: &HashMap<String, ServerEntry>) -> Result<()> {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(registry)
        .context("serializing server registry")?;
    std::fs::write(&path, content)?;
    Ok(())
}

pub fn add(name: &str, target: &str) -> Result<()> {
    let mut registry = load_registry()?;
    registry.insert(name.to_string(), ServerEntry { target: target.to_string() });
    save_registry(&registry)
}

pub fn remove(name: &str) -> Result<()> {
    let mut registry = load_registry()?;
    if registry.remove(name).is_none() {
        anyhow::bail!("server '{name}' not in registry");
    }
    save_registry(&registry)
}

pub fn list() -> Result<Vec<(String, String)>> {
    let registry = load_registry()?;
    let mut entries: Vec<(String, String)> = registry
        .into_iter()
        .map(|(name, entry)| (name, entry.target))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

// Active server tracking (per-repo)

fn repo_hash(repo_root: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    repo_root.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn active_file(repo_root: &Path) -> PathBuf {
    home_dir().join(ACTIVE_DIR).join(repo_hash(repo_root))
}

pub fn active(repo_root: &Path) -> Result<ActiveServer> {
    let path = active_file(repo_root);
    if !path.exists() {
        return Ok(ActiveServer::Localhost);
    }

    let content = std::fs::read_to_string(&path)
        .context("reading active server")?
        .trim()
        .to_string();

    if content.is_empty() || content == "localhost" {
        return Ok(ActiveServer::Localhost);
    }

    // support old format (system:name, repo:name) and new format (just name)
    let name = content
        .strip_prefix("system:")
        .or_else(|| content.strip_prefix("repo:"))
        .unwrap_or(&content);

    Ok(ActiveServer::Remote { name: name.to_string() })
}

pub fn write_active(repo_root: &Path, server: &ActiveServer) -> Result<()> {
    let path = active_file(repo_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = match server {
        ActiveServer::Localhost => "localhost",
        ActiveServer::Remote { name } => name,
    };

    std::fs::write(&path, format!("{content}\n"))?;
    Ok(())
}

pub fn resolve(name: &str) -> Result<ActiveServer> {
    if name == "localhost" {
        return Ok(ActiveServer::Localhost);
    }

    let registry = load_registry()?;
    if registry.contains_key(name) {
        Ok(ActiveServer::Remote { name: name.to_string() })
    } else {
        anyhow::bail!("server '{name}' not in registry. Add it with: cella server add {name} <target>")
    }
}
