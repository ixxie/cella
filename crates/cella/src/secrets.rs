use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::CellaConfig;

fn secrets_age_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".cella/secrets.age")
}

fn secrets_env_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".cella/secrets.env")
}

fn ssh_key_paths() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let mut paths = vec![
        PathBuf::from(&home).join(".ssh/id_ed25519"),
        PathBuf::from(&home).join(".ssh/id_rsa"),
    ];
    // server-side key
    paths.push(PathBuf::from("/var/lib/cella/ssh/id_ed25519"));
    paths.retain(|p| p.exists());
    paths
}

fn decrypt(age_file: &Path) -> Result<String> {
    let keys = ssh_key_paths();
    if keys.is_empty() {
        anyhow::bail!("no SSH keys found for decryption");
    }

    for key in &keys {
        let output = Command::new("age")
            .args(["-d", "-i"])
            .arg(key)
            .arg(age_file)
            .output()
            .context("failed to run age — is it installed?")?;
        if output.status.success() {
            return Ok(String::from_utf8(output.stdout)?);
        }
    }

    anyhow::bail!("age decryption failed — no key matched any recipient")
}

/// Resolve secrets for a cell.
pub fn resolve(_name: &str, repo_root: &Path, config: &CellaConfig) -> Result<Option<PathBuf>> {
    let env_content = if let Some(ref cmd) = config.secrets.command {
        let output = Command::new("sh")
            .args(["-c", cmd])
            .current_dir(repo_root)
            .output()
            .context("secrets command failed")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("secrets command failed: {stderr}");
        }
        Some(String::from_utf8(output.stdout)?)
    } else {
        let age_file = secrets_age_path(repo_root);
        if age_file.exists() {
            Some(decrypt(&age_file)?)
        } else {
            None
        }
    };

    match env_content {
        Some(content) => {
            let out = PathBuf::from("/var/lib/cella/secrets.env");
            std::fs::write(&out, &content)
                .context("writing secrets.env")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(Some(out))
        }
        None => Ok(None),
    }
}

/// Encrypt .cella/secrets.env → .cella/secrets.age for all keys.
pub fn encrypt(repo_root: &Path, keys: &[String]) -> Result<()> {
    if keys.is_empty() {
        anyhow::bail!("no keys configured — add secrets.keys in .cella/config.toml");
    }

    let env_file = secrets_env_path(repo_root);
    if !env_file.exists() {
        anyhow::bail!(".cella/secrets.env not found — create it first");
    }

    let age_file = secrets_age_path(repo_root);
    let mut cmd = Command::new("age");
    for key in keys {
        cmd.args(["-r", key]);
    }
    cmd.arg("-o").arg(&age_file).arg(&env_file);

    let output = cmd.output().context("failed to run age — is it installed?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("age encryption failed: {stderr}");
    }
    println!("encrypted → {}", age_file.display());
    Ok(())
}

/// Decrypt .cella/secrets.age, open $EDITOR, re-encrypt on save.
pub fn edit(repo_root: &Path, keys: &[String]) -> Result<()> {
    if keys.is_empty() {
        anyhow::bail!("no keys configured — add secrets.keys in .cella/config.toml");
    }

    let age_file = secrets_age_path(repo_root);
    let env_file = secrets_env_path(repo_root);

    let content = if age_file.exists() {
        decrypt(&age_file)?
    } else {
        String::new()
    };

    std::fs::create_dir_all(repo_root.join(".cella"))?;
    std::fs::write(&env_file, &content)?;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = Command::new(&editor)
        .arg(&env_file)
        .status()
        .context("failed to open editor")?;
    if !status.success() {
        anyhow::bail!("editor exited with {}", status);
    }

    encrypt(repo_root, keys)?;

    std::fs::remove_file(&env_file).ok();
    Ok(())
}
