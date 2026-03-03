use crate::config::{Auth, Server};
use std::os::unix::process::CommandExt;
use std::process::Command;

/// Connect to a server via SSH.
/// This function does not return on success (exec replaces the process).
pub fn connect(server: &Server) -> ! {
    match &server.auth {
        Some(Auth::Password(password)) => connect_with_password(server, password),
        Some(Auth::Key(key_path)) => connect_with_key(server, key_path),
        None => connect_plain(server),
    }
}

fn ssh_base(server: &Server) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.arg("-p")
        .arg(server.port.to_string())
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("ServerAliveInterval=60")
        .arg("-o")
        .arg("ServerAliveCountMax=3");
    cmd
}

fn connect_with_key(server: &Server, key_path: &str) -> ! {
    let err = ssh_base(server)
        .arg("-i")
        .arg(key_path)
        .arg(format!("{}@{}", server.user, server.host))
        .exec();

    eprintln!("Failed to exec ssh: {}", err);
    std::process::exit(1);
}

fn connect_plain(server: &Server) -> ! {
    let err = ssh_base(server)
        .arg(format!("{}@{}", server.user, server.host))
        .exec();

    eprintln!("Failed to exec ssh: {}", err);
    std::process::exit(1);
}

/// Print the equivalent SSH command to stdout (for shell integration with Warp, etc.)
pub fn print_command(server: &Server) {
    let base = format!(
        "ssh -p {} -o StrictHostKeyChecking=no -o ServerAliveInterval=60 -o ServerAliveCountMax=3",
        server.port
    );

    match &server.auth {
        Some(Auth::Password(password)) => {
            let exe = std::env::current_exe().expect("Cannot determine sgo executable path");
            println!(
                "SGO_PASS={} SSH_ASKPASS={} SSH_ASKPASS_REQUIRE=force {} {}@{}",
                shell_escape(password),
                shell_escape(&exe.display().to_string()),
                base,
                server.user,
                server.host
            );
        }
        Some(Auth::Key(key_path)) => {
            println!(
                "{} -i {} {}@{}",
                base,
                shell_escape(key_path),
                server.user,
                server.host
            );
        }
        None => {
            println!("{} {}@{}", base, server.user, server.host);
        }
    }
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn connect_with_password(server: &Server, password: &str) -> ! {
    // Use SSH_ASKPASS mechanism: ssh calls our own binary to get the password.
    // No intermediate pty, no external dependencies.
    //
    // How it works:
    //   1. We set SGO_PASS=<password> in the environment
    //   2. We set SSH_ASKPASS=<path to our own binary>
    //   3. We set SSH_ASKPASS_REQUIRE=force (OpenSSH 8.4+)
    //   4. We exec ssh
    //   5. When ssh needs a password, it spawns our binary
    //   6. Our binary sees SGO_PASS, prints it, and exits
    //   7. ssh reads the password from stdout — done
    //
    // The ssh process directly owns the terminal. No pty wrapper, no freeze.

    let exe = std::env::current_exe().expect("Cannot determine sgo executable path");

    let err = ssh_base(server)
        .env("SGO_PASS", password)
        .env("SSH_ASKPASS", &exe)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .arg(format!("{}@{}", server.user, server.host))
        .exec();

    eprintln!("Failed to exec ssh: {}", err);
    std::process::exit(1);
}
