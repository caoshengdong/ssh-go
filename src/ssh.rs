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

fn connect_with_key(server: &Server, key_path: &str) -> ! {
    let err = Command::new("ssh")
        .arg("-i")
        .arg(key_path)
        .arg("-p")
        .arg(server.port.to_string())
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg(format!("{}@{}", server.user, server.host))
        .exec();

    eprintln!("Failed to exec ssh: {}", err);
    std::process::exit(1);
}

fn connect_plain(server: &Server) -> ! {
    let err = Command::new("ssh")
        .arg("-p")
        .arg(server.port.to_string())
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg(format!("{}@{}", server.user, server.host))
        .exec();

    eprintln!("Failed to exec ssh: {}", err);
    std::process::exit(1);
}

fn connect_with_password(server: &Server, password: &str) -> ! {
    // Use expect to automate password input
    let expect_script = format!(
        r#"set timeout 30
spawn ssh -p {} -o StrictHostKeyChecking=no {}@{}
expect {{
    "*assword*" {{
        send "{}\r"
        interact
    }}
    "*yes/no*" {{
        send "yes\r"
        expect "*assword*"
        send "{}\r"
        interact
    }}
    timeout {{
        puts "Connection timed out"
        exit 1
    }}
}}"#,
        server.port,
        server.user,
        server.host,
        escape_expect(password),
        escape_expect(password),
    );

    let err = Command::new("expect")
        .arg("-c")
        .arg(&expect_script)
        .exec();

    eprintln!("Failed to exec expect: {}", err);
    std::process::exit(1);
}

/// Escape special characters for expect's `send` command
fn escape_expect(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '$' => escaped.push_str("\\$"),
            '[' => escaped.push_str("\\["),
            ']' => escaped.push_str("\\]"),
            '{' => escaped.push_str("\\{"),
            '}' => escaped.push_str("\\}"),
            _ => escaped.push(c),
        }
    }
    escaped
}
