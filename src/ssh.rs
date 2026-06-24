use crate::config::{Auth, Server};
use std::path::PathBuf;
use std::process::Command;

/// Shared ssh options applied to every connection.
fn base_opts(port: u16) -> Vec<String> {
    vec![
        "-p".into(),
        port.to_string(),
        "-o".into(),
        "StrictHostKeyChecking=no".into(),
        "-o".into(),
        "ServerAliveInterval=60".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
    ]
}

/// Force password auth so the PTY runner always sees a prompt (no silent
/// fallback to a key in the agent / ~/.ssh).
pub(crate) fn force_password_opts(args: &mut Vec<String>) {
    args.push("-o".into());
    args.push("PubkeyAuthentication=no".into());
    args.push("-o".into());
    args.push("PreferredAuthentications=password,keyboard-interactive".into());
    args.push("-o".into());
    args.push("NumberOfPasswordPrompts=1".into());
}

/// Run a program with an inherited terminal and do not return.
///
/// On Unix we `exec()` so ssh fully replaces this process (cleanest signal and
/// terminal ownership). Windows has no `exec`, so we spawn, wait, and forward
/// the child's exit code.
pub(crate) fn exec_or_status(program: &str, args: &[String]) -> ! {
    let mut cmd = Command::new(program);
    cmd.args(args);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        eprintln!("Failed to exec {}: {}", program, err);
        std::process::exit(1);
    }

    #[cfg(windows)]
    {
        match cmd.status() {
            Ok(s) => std::process::exit(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("Failed to run {}: {}", program, e);
                std::process::exit(1);
            }
        }
    }
}

/// Connect to a server via SSH. Does not return on success.
pub fn connect(server: &Server) -> ! {
    let mut args = base_opts(server.port);
    apply_connect_options(&mut args);

    match &server.auth {
        Some(Auth::Password(password)) => {
            force_password_opts(&mut args);
            args.push(format!("{}@{}", server.user, server.host));
            let code = crate::pty::run("ssh", &args, password, !no_tty_requested());
            std::process::exit(code);
        }
        Some(Auth::Key(key_path)) => {
            args.push("-i".into());
            args.push(expand_path(key_path).display().to_string());
        }
        None => {}
    }

    args.push(format!("{}@{}", server.user, server.host));
    exec_or_status("ssh", &args);
}

/// Run a single command on the server and return its exit code.
/// stdout and stderr are inherited (passed through to the caller).
/// `command` is passed as a single argument to ssh, matching `ssh host "cmd"` semantics.
pub fn run_command(server: &Server, command: &str) -> i32 {
    let mut args = base_opts(server.port);
    // Do not read local stdin for script/AI-tool exec calls. This prevents ssh
    // from hanging after the remote command exits in non-interactive contexts.
    args.push("-n".into());
    apply_connect_options(&mut args);

    match &server.auth {
        Some(Auth::Password(password)) => {
            force_password_opts(&mut args);
            args.push(format!("{}@{}", server.user, server.host));
            args.push(command.to_string());
            return crate::pty::run("ssh", &args, password, false);
        }
        Some(Auth::Key(key_path)) => {
            args.push("-o".into());
            args.push("BatchMode=yes".into());
            args.push("-i".into());
            args.push(expand_path(key_path).display().to_string());
        }
        None => {
            args.push("-o".into());
            args.push("BatchMode=yes".into());
        }
    }

    args.push(format!("{}@{}", server.user, server.host));
    args.push(command.to_string());

    match Command::new("ssh").args(&args).status() {
        Ok(s) => s.code().unwrap_or(255),
        Err(e) => {
            eprintln!("Failed to spawn ssh: {}", e);
            255
        }
    }
}

fn apply_connect_options(args: &mut Vec<String>) {
    if no_tty_requested() {
        args.push("-T".into());
    }
}

fn no_tty_requested() -> bool {
    match std::env::var("SGO_NO_TTY") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

/// Print the equivalent SSH command to stdout (for shell integration with Warp, etc.)
///
/// Password auth cannot be embedded safely, so the printed command will prompt
/// for the password interactively.
pub fn print_command(server: &Server) {
    let mut parts = vec!["ssh".to_string()];
    parts.extend(base_opts(server.port));

    match &server.auth {
        Some(Auth::Key(key_path)) => {
            parts.push("-i".to_string());
            parts.push(shell_escape(&expand_path(key_path).display().to_string()));
        }
        Some(Auth::Password(_)) => {
            eprintln!("Note: password auth — ssh will prompt for the password.");
        }
        None => {}
    }

    parts.push(shell_escape(&format!("{}@{}", server.user, server.host)));
    println!("{}", parts.join(" "));
}

fn shell_escape(s: &str) -> String {
    if is_shell_safe(s) {
        return s.to_string();
    }

    #[cfg(windows)]
    {
        format!("'{}'", s.replace('\'', "''"))
    }

    #[cfg(not(windows))]
    {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(windows)]
fn is_shell_safe(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@=+,%\\".contains(c))
}

#[cfg(not(windows))]
fn is_shell_safe(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@=+,%".contains(c))
}

fn expand_path(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(path));
    }

    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return dirs::home_dir()
            .map(|home| join_path_segments(home, rest))
            .unwrap_or_else(|| PathBuf::from(path));
    }

    PathBuf::from(path)
}

fn join_path_segments(mut base: PathBuf, rest: &str) -> PathBuf {
    for part in rest.split(['/', '\\']).filter(|part| !part.is_empty()) {
        base.push(part);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_tilde_to_home_directory() {
        let home = dirs::home_dir().expect("home directory should exist for tests");

        assert_eq!(expand_path("~"), home);
        assert_eq!(
            expand_path("~/.ssh/id_rsa"),
            dirs::home_dir().unwrap().join(".ssh").join("id_rsa")
        );
        assert_eq!(
            expand_path("~\\.ssh\\id_rsa"),
            dirs::home_dir().unwrap().join(".ssh").join("id_rsa")
        );
    }

    #[test]
    fn shell_escape_quotes_values_with_spaces_or_quotes() {
        assert_eq!(shell_escape("simple-value_1"), "simple-value_1");

        #[cfg(windows)]
        assert_eq!(shell_escape("a b'c"), "'a b''c'");

        #[cfg(not(windows))]
        assert_eq!(shell_escape("a b'c"), "'a b'\\''c'");
    }

    #[test]
    fn parses_no_tty_environment_flag() {
        std::env::remove_var("SGO_NO_TTY");
        assert!(!no_tty_requested());

        std::env::set_var("SGO_NO_TTY", "true");
        assert!(no_tty_requested());

        std::env::set_var("SGO_NO_TTY", "0");
        assert!(!no_tty_requested());

        std::env::remove_var("SGO_NO_TTY");
    }
}
