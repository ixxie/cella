use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

const SERVICES_DIR: &str = "/tmp/cellx/services";

fn services_dir() -> PathBuf {
    PathBuf::from(SERVICES_DIR)
}

fn pid_path(name: &str) -> PathBuf {
    services_dir().join(format!("{name}.pid"))
}

fn cmd_path(name: &str) -> PathBuf {
    services_dir().join(format!("{name}.cmd"))
}

fn log_path(name: &str) -> PathBuf {
    services_dir().join(format!("{name}.log"))
}

fn read_pid(name: &str) -> Result<u32> {
    let content = std::fs::read_to_string(pid_path(name))
        .context(format!("service '{name}' not found"))?;
    content.trim().parse().context("invalid PID")
}

fn is_running(name: &str) -> bool {
    read_pid(name)
        .map(|pid| unsafe { libc::kill(pid as i32, 0) } == 0)
        .unwrap_or(false)
}

pub fn start(name: &str, cmd: &str) -> Result<()> {
    if cmd.is_empty() {
        anyhow::bail!("no command specified");
    }
    if is_running(name) {
        anyhow::bail!("service '{name}' already running (use restart)");
    }

    std::fs::create_dir_all(services_dir())?;

    let log = log_path(name);
    let log_file = std::fs::File::create(&log)?;

    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("failed to start service")?;

    std::fs::write(pid_path(name), child.id().to_string())?;
    std::fs::write(cmd_path(name), cmd)?;
    println!("started {name} (pid {})", child.id());
    Ok(())
}

pub fn stop(name: &str) -> Result<()> {
    let pid = read_pid(name)?;
    if !is_running(name) {
        std::fs::remove_file(pid_path(name)).ok();
        println!("{name} not running");
        return Ok(());
    }

    unsafe { libc::kill(pid as i32, libc::SIGTERM); }

    // wait briefly for exit
    for _ in 0..20 {
        if unsafe { libc::kill(pid as i32, 0) } != 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // force kill if still alive
    if unsafe { libc::kill(pid as i32, 0) } == 0 {
        unsafe { libc::kill(pid as i32, libc::SIGKILL); }
    }

    std::fs::remove_file(pid_path(name)).ok();
    println!("stopped {name}");
    Ok(())
}

pub fn restart(name: &str, cmd: Option<&str>) -> Result<()> {
    let actual_cmd = match cmd {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => std::fs::read_to_string(cmd_path(name))
            .context(format!("no previous command for service '{name}'"))?,
    };
    if is_running(name) {
        stop(name)?;
    }
    start(name, &actual_cmd)
}

pub fn logs(name: &str, follow: bool) -> Result<()> {
    let log = log_path(name);
    if !log.exists() {
        anyhow::bail!("no logs for service '{name}'");
    }

    let log_str = log.to_string_lossy().to_string();
    let mut args = vec!["-100"];
    if follow {
        args.push("-f");
    }
    args.push(&log_str);

    Command::new("tail")
        .args(&args)
        .status()
        .context("tail failed")?;
    Ok(())
}

pub fn list() -> Result<()> {
    let dir = services_dir();
    if !dir.exists() {
        println!("no services");
        return Ok(());
    }

    let mut found = false;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pid") {
            continue;
        }
        let name = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?");

        let status = if is_running(name) { "running" } else { "stopped" };
        let cmd = std::fs::read_to_string(cmd_path(name)).unwrap_or_default();
        println!("  {} [{}] {}", name, status, cmd.trim());
        found = true;
    }

    if !found {
        println!("no services");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(pid_path("dev"), PathBuf::from("/tmp/cellx/services/dev.pid"));
        assert_eq!(cmd_path("dev"), PathBuf::from("/tmp/cellx/services/dev.cmd"));
        assert_eq!(log_path("dev"), PathBuf::from("/tmp/cellx/services/dev.log"));
    }
}
