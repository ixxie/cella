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

fn ssh_key_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let ed = PathBuf::from(&home).join(".ssh/id_ed25519");
    if ed.exists() { return ed; }
    PathBuf::from(&home).join(".ssh/id_rsa")
}

fn decrypt(age_file: &Path) -> Result<String> {
    let key = ssh_key_path();
    let output = Command::new("age")
        .args(["-d", "-i"])
        .arg(&key)
        .arg(age_file)
        .output()
        .context("failed to run age — is it installed?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("age decryption failed: {stderr}");
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Resolve secrets for a cell. Returns path to a secrets.env file if secrets are available.
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

/// Encrypt .cella/secrets.env → .cella/secrets.age using a recipient key.
pub fn encrypt(repo_root: &Path, recipient: &str) -> Result<()> {
    let env_file = secrets_env_path(repo_root);
    if !env_file.exists() {
        anyhow::bail!(".cella/secrets.env not found — create it first");
    }

    let age_file = secrets_age_path(repo_root);
    let output = Command::new("age")
        .args(["-r", recipient, "-o"])
        .arg(&age_file)
        .arg(&env_file)
        .output()
        .context("failed to run age — is it installed?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("age encryption failed: {stderr}");
    }
    println!("encrypted → {}", age_file.display());
    Ok(())
}

/// Decrypt .cella/secrets.age to a temp file, open $EDITOR, re-encrypt on save.
pub fn edit(repo_root: &Path, recipient: &str) -> Result<()> {
    let age_file = secrets_age_path(repo_root);
    let env_file = secrets_env_path(repo_root);

    // decrypt existing or start fresh
    let content = if age_file.exists() {
        decrypt(&age_file)?
    } else {
        String::new()
    };

    // write to .cella/secrets.env for editing
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

    // re-encrypt
    encrypt(repo_root, recipient)?;

    // remove plaintext
    std::fs::remove_file(&env_file).ok();
    Ok(())
}
