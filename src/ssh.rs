use crate::config::{Auth, Server};
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

    match &server.auth {
        Some(Auth::Password(password)) => {
            force_password_opts(&mut args);
            args.push(format!("{}@{}", server.user, server.host));
            let code = crate::pty::run("ssh", &args, password, true);
            std::process::exit(code);
        }
        Some(Auth::Key(key_path)) => {
            args.push("-i".into());
            args.push(key_path.clone());
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

    match &server.auth {
        Some(Auth::Password(password)) => {
            // Password auth must go through the PTY runner; BatchMode would
            // suppress the prompt, so we never set it here.
            force_password_opts(&mut args);
            args.push(format!("{}@{}", server.user, server.host));
            args.push(command.to_string());
            return crate::pty::run("ssh", &args, password, false);
        }
        Some(Auth::Key(key_path)) => {
            // BatchMode lets ssh fail fast on missing keys.
            args.push("-o".into());
            args.push("BatchMode=yes".into());
            args.push("-i".into());
            args.push(key_path.clone());
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
            parts.push(shell_escape(key_path));
        }
        Some(Auth::Password(_)) => {
            eprintln!("Note: password auth — ssh will prompt for the password.");
        }
        None => {}
    }

    parts.push(format!("{}@{}", server.user, server.host));
    println!("{}", parts.join(" "));
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
