use crate::config::{Auth, Server};
use std::io::IsTerminal;
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
            let code = crate::pty::run(
                "ssh",
                &args,
                password,
                crate::pty::Opts {
                    // With `-T` there is no remote tty and the session is a
                    // plain data pipe, which is what `Command` describes.
                    mode: if no_tty_requested() {
                        crate::pty::Mode::Command
                    } else {
                        crate::pty::Mode::Shell
                    },
                    forward_stdin: true,
                },
            );
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
/// The command reaches the remote as one string, matching `ssh host "cmd"`
/// semantics — see `build_remote_command` for how `command_parts` becomes it.
///
/// Local stdin is forwarded unless it is a terminal, so `sgo exec host 'bash -s'
/// <<'EOF'` feeds the remote the way the same heredoc would feed `ssh`.
pub fn run_command(server: &Server, command_parts: &[String]) -> i32 {
    let command = build_remote_command(command_parts);
    let forward_stdin = !std::io::stdin().is_terminal();

    let mut args = base_opts(server.port);
    // A terminal's keystrokes belong to the user, not to this command: keep ssh
    // off stdin unless something was actually piped or redirected in. `-n` also
    // stops ssh from blocking on a stdin nobody will ever close.
    if !forward_stdin {
        args.push("-n".into());
    }
    apply_connect_options(&mut args);

    match &server.auth {
        Some(Auth::Password(password)) => {
            force_password_opts(&mut args);
            args.push(format!("{}@{}", server.user, server.host));
            args.push(command);
            return crate::pty::run(
                "ssh",
                &args,
                password,
                crate::pty::Opts {
                    mode: crate::pty::Mode::Command,
                    forward_stdin,
                },
            );
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
    args.push(command);

    // stdin is inherited, so a heredoc or pipe reaches the remote command.
    match Command::new("ssh").args(&args).status() {
        Ok(s) => s.code().unwrap_or(255),
        Err(e) => {
            eprintln!("Failed to spawn ssh: {}", e);
            255
        }
    }
}

/// Build the single command string ssh hands to the remote shell.
///
/// One part is passed through verbatim, so `sgo exec host "a | b"` keeps
/// behaving like `ssh host "a | b"` — pipes, redirects and globs are the remote
/// shell's to interpret. Several parts are quoted individually, which is the
/// point of the `--` form: what the local shell handed to sgo is exactly what
/// the remote command receives, with no second round of word splitting or glob
/// expansion to lose quotes and spaces in.
fn build_remote_command(parts: &[String]) -> String {
    match parts {
        [single] => single.clone(),
        _ => parts
            .iter()
            .map(|p| posix_quote(p))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Quote a single argument for the remote shell. Always POSIX: the shell on the
/// far end is the server's, whatever the client happens to run.
fn posix_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@=+,%".contains(c));

    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
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
    fn single_command_part_reaches_the_remote_shell_untouched() {
        let parts = vec!["df -h | grep /var".to_string()];
        assert_eq!(build_remote_command(&parts), "df -h | grep /var");
    }

    #[test]
    fn multiple_command_parts_are_quoted_individually() {
        let parts = vec![
            "grep".to_string(),
            "hello world".to_string(),
            "/var/log/app.log".to_string(),
        ];
        assert_eq!(
            build_remote_command(&parts),
            "grep 'hello world' /var/log/app.log"
        );
    }

    #[test]
    fn quoting_survives_the_characters_a_shell_would_eat() {
        // Each of these would be expanded, split or swallowed by the remote
        // shell if it were pasted in unquoted.
        assert_eq!(posix_quote("$HOME"), "'$HOME'");
        assert_eq!(posix_quote("*.mp4"), "'*.mp4'");
        assert_eq!(posix_quote("a'b"), "'a'\\''b'");
        assert_eq!(posix_quote("`id`"), "'`id`'");
        assert_eq!(posix_quote(""), "''");
        assert_eq!(posix_quote("plain-value_1"), "plain-value_1");
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
