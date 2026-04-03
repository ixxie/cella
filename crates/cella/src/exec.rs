/// Shell-safe command construction for multi-hop SSH execution.
///
/// Each function returns a script string suitable for one specific context.
/// The caller (vm::shell) handles SSH transport wrapping.

/// Escape a string for safe embedding in a POSIX shell command.
/// Result is always wrapped in single quotes.
pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build a detached command that survives SSH disconnect.
/// Uses setsid to create a new session, redirects I/O to log file.
/// Does NOT include cd — vm::shell handles workdir.
pub fn detached(cmd: &str, log_path: &str) -> String {
    let log = shell_escape(log_path);
    format!(
        "mkdir -p $(dirname {log}) && setsid sh -c {} > {log} 2>&1 < /dev/null &",
        shell_escape(cmd),
    )
}

/// Wrap a command for the client→server SSH hop.
/// Produces: cella shell --server <cell> -c <escaped_cmd>
pub fn cella_hop(cell: &str, cmd: &str) -> String {
    format!("cella shell --server {} -c {}", cell, shell_escape(cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_simple() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn escape_single_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn escape_empty() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn escape_special_chars() {
        assert_eq!(shell_escape("a > b & c"), "'a > b & c'");
    }

    #[test]
    fn detached_structure() {
        let s = detached("cellx flow run qa", "/tmp/cellx/flow.log");
        assert!(s.contains("setsid sh -c 'cellx flow run qa'"));
        assert!(s.contains("> '/tmp/cellx/flow.log'"));
        assert!(s.contains("< /dev/null &"));
        assert!(s.starts_with("mkdir -p"));
    }

    #[test]
    fn detached_escapes_cmd() {
        let s = detached("echo 'hello'", "/tmp/out.log");
        assert!(s.contains("setsid sh -c 'echo '\\''hello'\\'''"));
    }

    #[test]
    fn cella_hop_basic() {
        let s = cella_hop("my-cell", "cellx flow done");
        assert_eq!(s, "cella shell --server my-cell -c 'cellx flow done'");
    }

    #[test]
    fn cella_hop_escapes_quotes() {
        let s = cella_hop("cell", "echo 'hi'");
        assert!(s.contains("-c 'echo '\\''hi'\\'''"));
    }

    #[test]
    fn cella_hop_escapes_redirects() {
        let s = cella_hop("cell", "ls > /tmp/out");
        assert_eq!(s, "cella shell --server cell -c 'ls > /tmp/out'");
    }

    #[test]
    fn double_hop_composition() {
        let user_cmd = "tail -f /tmp/cellx/flow.log 2>/dev/null || echo 'no log'";
        let server_cmd = cella_hop("feat-a", user_cmd);
        assert!(server_cmd.contains("-c '"));

        let workspace = "/cella-test";
        let script = format!("cd {} && {}", shell_escape(workspace), user_cmd);
        let ssh_arg = format!("sh -c {}", shell_escape(&script));
        assert!(ssh_arg.contains("2>/dev/null"));
        assert!(ssh_arg.contains("no log"));
    }

}
