use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const CELLX_DIR: &str = "/tmp/cellx";
const DEFAULT_STATE_DIR: &str = "/var/lib/cellx/state";

// flow.toml config

#[derive(Debug, Deserialize)]
pub struct FlowConfig {
    pub flow: FlowMeta,
    #[serde(default)]
    pub rules: HashMap<String, Rule>,
}

fn default_max_retries() -> u32 { 10 }
fn default_timeout() -> String { "2h".to_string() }

#[derive(Debug, Deserialize)]
pub struct FlowMeta {
    pub start: String,
    pub state: Option<String>,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Default op timeout for this flow (e.g. "2h", "30m"). Ops can override.
    #[serde(default = "default_timeout")]
    pub timeout: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Rule {
    #[serde(default)]
    pub extends: Vec<String>,
    pub files: Option<FileRule>,
    pub reads: Option<NetRule>,
    pub writes: Option<NetRule>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FileRule {
    pub denied: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NetRule {
    pub allowed: Option<Vec<String>>,
    pub denied: Option<Vec<String>>,
}

// op.md config

#[derive(Debug, Deserialize)]
pub struct OpFrontmatter {
    pub name: String,
    #[serde(default)]
    pub rules: Vec<String>,
    #[serde(default)]
    pub next: Vec<String>,
    pub on: Option<Lifecycle>,
    #[serde(default)]
    pub params: HashMap<String, String>,
    /// Op-level timeout override (e.g. "4h", "30m"). Falls back to flow timeout.
    pub timeout: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    Success,
    Failure,
    Finish,
}

pub struct Op {
    pub config: OpFrontmatter,
    pub prompt: String,
}

// Flow state

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FlowState {
    Running,
    Paused,
    Done,
    Failed,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlowStatus {
    pub flow_name: String,
    pub current_op: String,
    pub state: FlowState,
    pub started_at: u64,
    pub op_started_at: u64,
    pub state_dir: PathBuf,
}

impl FlowStatus {
    fn pointer_path() -> PathBuf {
        PathBuf::from(CELLX_DIR).join("flow.json")
    }

    fn flow_json_path(&self) -> PathBuf {
        self.state_dir.join("flow.json")
    }

    pub fn load() -> Result<Self> {
        let pointer = Self::pointer_path();
        let content = std::fs::read_to_string(&pointer)
            .context("no active flow")?;
        serde_json::from_str(&content).context("parsing flow state")
    }

    pub fn save(&self) -> Result<()> {
        // Write to state_dir
        std::fs::create_dir_all(&self.state_dir)?;
        let state_path = self.flow_json_path();
        let tmp = state_path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &state_path)?;

        // Write pointer in /tmp/cellx/ for quick lookup
        std::fs::create_dir_all(CELLX_DIR)?;
        let pointer = Self::pointer_path();
        std::fs::write(&pointer, &json)?;
        Ok(())
    }

    pub fn remove() {
        std::fs::remove_file(Self::pointer_path()).ok();
    }
}

// Host-side status report (cellx → control API → host file)

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FlowReport {
    pub cell_ip: String,
    pub flow_name: String,
    pub current_op: String,
    pub state: FlowState,
    pub started_at: u64,
    pub op_started_at: u64,
}

// Runner result

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Decision {
    To {
        op: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<serde_json::Value>,
    },
    Retry { after: Option<u64> },
    Pause,
    Done,
}

// Result file I/O (workspace-based)

// Signal file (external pause/stop)

fn signal_path() -> PathBuf {
    PathBuf::from(CELLX_DIR).join("flow-signal")
}

pub enum Signal {
    Pause,
    Done,
}

pub fn check_signal() -> Option<Signal> {
    let path = signal_path();
    let content = std::fs::read_to_string(&path).ok()?;
    std::fs::remove_file(&path).ok();
    match content.trim() {
        "pause" => Some(Signal::Pause),
        "done" => Some(Signal::Done),
        _ => None,
    }
}

pub fn write_signal(sig: &str) -> Result<()> {
    std::fs::create_dir_all(CELLX_DIR)?;
    std::fs::write(signal_path(), sig)?;
    Ok(())
}

// Parsing

pub fn load_flow(repo_root: &Path, name: &str) -> Result<FlowConfig> {
    let path = repo_root.join(".cella/flows").join(name).join("flow.toml");
    let content = std::fs::read_to_string(&path)
        .context(format!("reading {}", path.display()))?;
    toml::from_str(&content).context("parsing flow.toml")
}

pub fn load_op(flow_dir: &Path, op_name: &str) -> Result<Op> {
    let path = flow_dir.join("ops").join(op_name).join("op.md");
    let content = std::fs::read_to_string(&path)
        .context(format!("reading {}", path.display()))?;
    parse_op_md(&content)
}

pub fn parse_op_md(content: &str) -> Result<Op> {
    let trimmed = content.trim();
    if !trimmed.starts_with("---") {
        anyhow::bail!("op.md must start with --- frontmatter delimiter");
    }

    let after_first = &trimmed[3..];
    let end = after_first.find("---")
        .ok_or_else(|| anyhow::anyhow!("missing closing --- in op.md frontmatter"))?;

    let frontmatter = &after_first[..end].trim();
    let body = after_first[end + 3..].trim();

    let config: OpFrontmatter = serde_yaml::from_str(frontmatter)
        .context("parsing op.md frontmatter")?;

    Ok(Op {
        config,
        prompt: body.to_string(),
    })
}

// Duration parsing

pub fn parse_duration(s: &str) -> Option<u64> {
    if let Ok(n) = s.parse::<u64>() {
        return Some(n);
    }
    let (num_str, suffix) = if s.ends_with('m') {
        (&s[..s.len() - 1], 'm')
    } else if s.ends_with('h') {
        (&s[..s.len() - 1], 'h')
    } else if s.ends_with('s') {
        (&s[..s.len() - 1], 's')
    } else {
        return None;
    };
    let n: u64 = num_str.parse().ok()?;
    match suffix {
        's' => Some(n),
        'm' => Some(n * 60),
        'h' => Some(n * 3600),
        _ => None,
    }
}

// Helpers

pub fn op_allows(op: &Op, target: &str) -> bool {
    op.config.next.iter().any(|n| n == target)
}

/// Resolve effective timeout in seconds: op override > flow default.
pub fn resolve_timeout(op: &OpFrontmatter, flow: &FlowMeta) -> Option<u64> {
    let raw = op.timeout.as_deref().unwrap_or(&flow.timeout);
    parse_duration(raw)
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Workspace paths

pub fn flow_state_dir(base: Option<&str>, flow_name: &str, timestamp: u64) -> PathBuf {
    let base = base.unwrap_or(DEFAULT_STATE_DIR);
    PathBuf::from(base).join(format!("{flow_name}-{timestamp}"))
}

pub fn op_workspace(flow_state: &Path, op_name: &str, timestamp: u64) -> PathBuf {
    flow_state.join(format!("{op_name}-{timestamp}"))
}

pub fn write_result_to(workspace: &Path, decision: &Decision) -> Result<()> {
    std::fs::create_dir_all(workspace)?;
    let json = serde_json::to_string(decision)?;
    std::fs::write(workspace.join("result.json"), &json)?;
    Ok(())
}

pub fn read_result_from(workspace: &Path) -> Result<Decision> {
    let path = workspace.join("result.json");
    let content = std::fs::read_to_string(&path)
        .context("no result (missing result.json in workspace)")?;
    serde_json::from_str(&content).context("parsing result")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_op_md_basic() {
        let content = "---\nname: implement\nrules:\n  - no-test-write\nnext:\n  - validate\n  - implement\n---\n\nImplement the feature.\n";
        let op = parse_op_md(content).unwrap();
        assert_eq!(op.config.name, "implement");
        assert_eq!(op.config.rules, vec!["no-test-write"]);
        assert_eq!(op.config.next, vec!["validate", "implement"]);
        assert_eq!(op.prompt, "Implement the feature.");
    }

    #[test]
    fn parse_op_md_minimal() {
        let content = "---\nname: eval\nnext:\n  - implement\n---\n\nEvaluate results.\n";
        let op = parse_op_md(content).unwrap();
        assert_eq!(op.config.name, "eval");
        assert_eq!(op.config.next, vec!["implement"]);
        assert_eq!(op.prompt, "Evaluate results.");
    }

    #[test]
    fn parse_op_md_no_frontmatter() {
        let content = "Just some text without frontmatter.";
        assert!(parse_op_md(content).is_err());
    }

    #[test]
    fn parse_duration_secs() {
        assert_eq!(parse_duration("30"), Some(30));
        assert_eq!(parse_duration("30s"), Some(30));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration("5m"), Some(300));
        assert_eq!(parse_duration("30m"), Some(1800));
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("1h"), Some(3600));
    }

    #[test]
    fn parse_duration_invalid() {
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn decision_roundtrip() {
        let cases = vec![
            Decision::To { op: "impl".to_string(), params: None },
            Decision::Retry { after: Some(60) },
            Decision::Retry { after: None },
            Decision::Pause,
            Decision::Done,
        ];
        for d in cases {
            let json = serde_json::to_string(&d).unwrap();
            let parsed: Decision = serde_json::from_str(&json).unwrap();
            match (&d, &parsed) {
                (Decision::To { op: a, .. }, Decision::To { op: b, .. }) => assert_eq!(a, b),
                (Decision::Retry { after: a }, Decision::Retry { after: b }) => assert_eq!(a, b),
                (Decision::Pause, Decision::Pause) => {}
                (Decision::Done, Decision::Done) => {}
                _ => panic!("mismatch"),
            }
        }
    }

    #[test]
    fn op_allows_valid() {
        let op = Op {
            config: OpFrontmatter {
                name: "implement".to_string(),
                rules: vec![],
                next: vec!["validate".to_string(), "implement".to_string()],
                on: None,
                params: HashMap::new(),
                timeout: None,
            },
            prompt: String::new(),
        };
        assert!(op_allows(&op, "validate"));
        assert!(op_allows(&op, "implement"));
        assert!(!op_allows(&op, "unknown"));
    }

    #[test]
    fn parse_lifecycle_op() {
        let content = "---\nname: cleanup\non: finish\n---\n\nClean up resources.\n";
        let op = parse_op_md(content).unwrap();
        assert_eq!(op.config.on, Some(Lifecycle::Finish));
        assert_eq!(op.config.name, "cleanup");
        assert!(op.config.next.is_empty());
    }

    #[test]
    fn parse_lifecycle_variants() {
        let success = parse_op_md("---\nname: a\non: success\n---\n\nok\n").unwrap();
        assert_eq!(success.config.on, Some(Lifecycle::Success));

        let failure = parse_op_md("---\nname: b\non: failure\n---\n\nfail\n").unwrap();
        assert_eq!(failure.config.on, Some(Lifecycle::Failure));

        let finish = parse_op_md("---\nname: c\non: finish\n---\n\ndone\n").unwrap();
        assert_eq!(finish.config.on, Some(Lifecycle::Finish));
    }

    #[test]
    fn regular_op_has_no_lifecycle() {
        let content = "---\nname: implement\nnext:\n  - validate\n---\n\nDo stuff.\n";
        let op = parse_op_md(content).unwrap();
        assert_eq!(op.config.on, None);
    }
}
