use crate::config::{Auth, Server};
use colored::Colorize;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum TunnelMode {
    /// -L local:host:remote
    Local {
        local_port: u16,
        remote_host: String,
        remote_port: u16,
    },
    /// -R remote:host:local
    Reverse {
        remote_port: u16,
        local_host: String,
        local_port: u16,
    },
    /// -D port
    Dynamic { port: u16 },
}

impl TunnelMode {
    pub fn describe(&self) -> String {
        match self {
            TunnelMode::Local {
                local_port,
                remote_host,
                remote_port,
            } => format!(
                "localhost:{} -> {}:{} (local forward)",
                local_port, remote_host, remote_port
            ),
            TunnelMode::Reverse {
                remote_port,
                local_host,
                local_port,
            } => format!(
                "server:{} -> {}:{} (reverse forward)",
                remote_port, local_host, local_port
            ),
            TunnelMode::Dynamic { port } => {
                format!("SOCKS5 proxy on localhost:{}", port)
            }
        }
    }

    fn apply(&self, args: &mut Vec<String>) {
        match self {
            TunnelMode::Local {
                local_port,
                remote_host,
                remote_port,
            } => {
                args.push("-L".into());
                args.push(format!("{}:{}:{}", local_port, remote_host, remote_port));
            }
            TunnelMode::Reverse {
                remote_port,
                local_host,
                local_port,
            } => {
                args.push("-R".into());
                args.push(format!("{}:{}:{}", remote_port, local_host, local_port));
            }
            TunnelMode::Dynamic { port } => {
                args.push("-D".into());
                args.push(port.to_string());
            }
        }
    }
}

/// Parse a port spec into a TunnelMode::Local.
/// Formats: "8080", "8080:9090", "8080:host:9090"
pub fn parse_local_spec(spec: &str) -> Result<TunnelMode, String> {
    let parts: Vec<&str> = spec.split(':').collect();
    match parts.as_slice() {
        [p] => {
            let port = p.parse().map_err(|_| format!("invalid port: {}", p))?;
            Ok(TunnelMode::Local {
                local_port: port,
                remote_host: "localhost".to_string(),
                remote_port: port,
            })
        }
        [l, r] => {
            let local = l.parse().map_err(|_| format!("invalid port: {}", l))?;
            let remote = r.parse().map_err(|_| format!("invalid port: {}", r))?;
            Ok(TunnelMode::Local {
                local_port: local,
                remote_host: "localhost".to_string(),
                remote_port: remote,
            })
        }
        [l, h, r] => {
            let local = l.parse().map_err(|_| format!("invalid port: {}", l))?;
            let remote = r.parse().map_err(|_| format!("invalid port: {}", r))?;
            Ok(TunnelMode::Local {
                local_port: local,
                remote_host: h.to_string(),
                remote_port: remote,
            })
        }
        _ => Err(format!(
            "invalid port spec \"{}\" — expected PORT, LOCAL:REMOTE, or LOCAL:HOST:REMOTE",
            spec
        )),
    }
}

/// Verbosity level for ssh itself: 0 = off, 1 = -v, 2 = -vv, 3 = -vvv.
pub type Verbosity = u8;

/// Open an SSH tunnel and exit with ssh's exit code when it closes.
pub fn open(server: &Server, mode: &TunnelMode, verbose: Verbosity) -> ! {
    let mut args = tunnel_base(server, verbose);
    mode.apply(&mut args);

    match &server.auth {
        Some(Auth::Password(password)) => {
            crate::ssh::force_password_opts(&mut args);
            args.push(format!("{}@{}", server.user, server.host));
            log_command(&args, server);
            eprintln!("{}", "Executing ssh (Ctrl+C to close tunnel)...".dimmed());
            // Not interactive: a tunnel has no shell, and we want Ctrl+C to
            // raise a signal that tears the tunnel down. It has no use for
            // stdin either — `-N` means ssh never opens a session channel.
            let code = crate::pty::run(
                "ssh",
                &args,
                password,
                crate::pty::Opts {
                    mode: crate::pty::Mode::Tunnel,
                    forward_stdin: false,
                },
            );
            std::process::exit(code);
        }
        Some(Auth::Key(key_path)) => {
            args.push("-i".into());
            args.push(expand_path(key_path).display().to_string());
            args.push(format!("{}@{}", server.user, server.host));
        }
        None => {
            args.push(format!("{}@{}", server.user, server.host));
        }
    }

    log_command(&args, server);
    eprintln!("{}", "Executing ssh (Ctrl+C to close tunnel)...".dimmed());
    crate::ssh::exec_or_status("ssh", &args);
}

fn tunnel_base(server: &Server, verbose: Verbosity) -> Vec<String> {
    let mut args = vec![
        "-N".into(),
        "-p".into(),
        server.port.to_string(),
        "-o".into(),
        "StrictHostKeyChecking=no".into(),
        "-o".into(),
        "ServerAliveInterval=60".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
    ];

    match verbose {
        0 => {}
        1 => args.push("-v".into()),
        2 => args.push("-vv".into()),
        _ => args.push("-vvv".into()),
    }
    args
}

/// Print the ssh command to stderr. The password is fed via the PTY runner and
/// never appears in the argument list.
fn log_command(args: &[String], server: &Server) {
    let line = std::iter::once("ssh".to_string())
        .chain(args.iter().map(|a| shell_escape(a)))
        .collect::<Vec<_>>()
        .join(" ");

    eprintln!(
        "{} {}@{} via port {}",
        "Server:".green().bold(),
        server.user,
        server.host,
        server.port
    );
    eprintln!("{} {}", "Command:".green().bold(), line.white());
}

fn shell_escape(s: &str) -> String {
    if is_shell_safe(s) {
        s.to_string()
    } else {
        #[cfg(windows)]
        {
            format!("'{}'", s.replace('\'', "''"))
        }

        #[cfg(not(windows))]
        {
            format!("'{}'", s.replace('\'', "'\\''"))
        }
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
    fn parses_local_tunnel_specs() {
        assert_eq!(
            parse_local_spec("8080").unwrap(),
            TunnelMode::Local {
                local_port: 8080,
                remote_host: "localhost".to_string(),
                remote_port: 8080,
            }
        );
        assert_eq!(
            parse_local_spec("8080:9090").unwrap(),
            TunnelMode::Local {
                local_port: 8080,
                remote_host: "localhost".to_string(),
                remote_port: 9090,
            }
        );
        assert_eq!(
            parse_local_spec("8080:db.internal:5432").unwrap(),
            TunnelMode::Local {
                local_port: 8080,
                remote_host: "db.internal".to_string(),
                remote_port: 5432,
            }
        );
    }

    #[test]
    fn rejects_invalid_local_tunnel_specs() {
        assert!(parse_local_spec("not-a-port").is_err());
        assert!(parse_local_spec("8080:9090:too:many").is_err());
    }
}
