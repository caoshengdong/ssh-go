use crate::config::{Auth, Server};
use colored::Colorize;
use std::os::unix::process::CommandExt;
use std::process::Command;

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

    fn apply(&self, cmd: &mut Command) {
        match self {
            TunnelMode::Local {
                local_port,
                remote_host,
                remote_port,
            } => {
                cmd.arg("-L")
                    .arg(format!("{}:{}:{}", local_port, remote_host, remote_port));
            }
            TunnelMode::Reverse {
                remote_port,
                local_host,
                local_port,
            } => {
                cmd.arg("-R")
                    .arg(format!("{}:{}:{}", remote_port, local_host, local_port));
            }
            TunnelMode::Dynamic { port } => {
                cmd.arg("-D").arg(port.to_string());
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

/// Open an SSH tunnel. Does not return on success (exec replaces the process).
pub fn open(server: &Server, mode: &TunnelMode, verbose: Verbosity) -> ! {
    let mut cmd = tunnel_base(server, verbose);
    mode.apply(&mut cmd);

    match &server.auth {
        Some(Auth::Password(password)) => {
            let exe = std::env::current_exe().expect("Cannot determine sgo executable path");
            cmd.env("SGO_PASS", password)
                .env("SSH_ASKPASS", &exe)
                .env("SSH_ASKPASS_REQUIRE", "force")
                .arg(format!("{}@{}", server.user, server.host));
        }
        Some(Auth::Key(key_path)) => {
            cmd.arg("-i")
                .arg(key_path)
                .arg(format!("{}@{}", server.user, server.host));
        }
        None => {
            cmd.arg(format!("{}@{}", server.user, server.host));
        }
    };

    log_command(&cmd, server);
    eprintln!("{}", "Executing ssh (Ctrl+C to close tunnel)...".dimmed());

    let err = cmd.exec();
    eprintln!("{} failed to exec ssh: {}", "Error:".red().bold(), err);
    std::process::exit(1);
}

fn tunnel_base(server: &Server, verbose: Verbosity) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.arg("-N")
        .arg("-p")
        .arg(server.port.to_string())
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("ServerAliveInterval=60")
        .arg("-o")
        .arg("ServerAliveCountMax=3")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes");

    match verbose {
        0 => {}
        1 => {
            cmd.arg("-v");
        }
        2 => {
            cmd.arg("-vv");
        }
        _ => {
            cmd.arg("-vvv");
        }
    }
    cmd
}

/// Print the ssh command to stderr, with passwords redacted.
fn log_command(cmd: &Command, server: &Server) {
    let program = cmd.get_program().to_string_lossy();
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| shell_escape(&a.to_string_lossy()))
        .collect();

    let env_prefix: Vec<String> = cmd
        .get_envs()
        .filter_map(|(k, v)| {
            let key = k.to_string_lossy().into_owned();
            let val = v?.to_string_lossy().into_owned();
            let display = if key == "SGO_PASS" {
                "<redacted>".to_string()
            } else {
                shell_escape(&val)
            };
            Some(format!("{}={}", key, display))
        })
        .collect();

    let mut line = String::new();
    if !env_prefix.is_empty() {
        line.push_str(&env_prefix.join(" "));
        line.push(' ');
    }
    line.push_str(&program);
    if !args.is_empty() {
        line.push(' ');
        line.push_str(&args.join(" "));
    }

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
