use crate::config::{Auth, Server};
use colored::Colorize;

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

/// Open an SSH tunnel. Does not return on success.
pub fn open(server: &Server, mode: &TunnelMode, verbose: Verbosity) -> ! {
    let mut args = tunnel_base(server, verbose);
    mode.apply(&mut args);

    match &server.auth {
        Some(Auth::Password(password)) => {
            crate::ssh::force_password_opts(&mut args);
            args.push(format!("{}@{}", server.user, server.host));
            log_command(&args, server);
            eprintln!("{}", "Executing ssh (Ctrl+C to close tunnel)...".dimmed());
            // raw = false: a tunnel has no interactive shell, and we want Ctrl+C
            // to raise a signal that tears the tunnel down.
            let code = crate::pty::run("ssh", &args, password, false);
            std::process::exit(code);
        }
        Some(Auth::Key(key_path)) => {
            args.push("-i".into());
            args.push(key_path.clone());
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
    if s.chars().all(|c| c.is_alphanumeric() || "-_./:@=".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
