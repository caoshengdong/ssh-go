//! Cross-platform PTY runner for password authentication.
//!
//! Windows' built-in OpenSSH ignores `SSH_ASKPASS` (see Win32-OpenSSH #2115),
//! so we cannot auto-fill passwords via an askpass helper. Instead we spawn
//! `ssh` inside a pseudo-terminal (ConPTY on Windows, a real PTY on Unix),
//! watch its output for the password prompt, type the stored password, and
//! then proxy the interactive session between the user's terminal and the PTY.
//!
//! This path is only used for password auth. Key/none auth runs `ssh` directly
//! with an inherited terminal (see `ssh::exec_or_status`).

use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size as term_size};
use crossterm::tty::IsTty;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// Run `ssh` (or any program) inside a PTY, feeding `password` when a password
/// prompt appears, then proxy the session. Returns the child's exit code.
///
/// `raw` should be true only for a full interactive shell, where keystrokes
/// (including Ctrl+C) must pass straight through to the remote. For tunnels and
/// non-interactive `exec`, keep it false so Ctrl+C still raises a local signal.
pub fn run(program: &str, args: &[String], password: &str, raw: bool) -> i32 {
    let stdin_is_tty = std::io::stdin().is_tty();
    let (cols, rows) = if std::io::stdout().is_tty() {
        term_size().unwrap_or((80, 24))
    } else {
        (80, 24)
    };

    let pty_system = native_pty_system();
    let pair = match pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sgo: failed to open pty: {}", e);
            return 255;
        }
    };

    let mut cmd = CommandBuilder::new(program);
    for a in args {
        cmd.arg(a);
    }
    // Inherit the parent environment so ssh finds PATH, HOME, known_hosts, etc.
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sgo: failed to spawn {}: {}", program, e);
            return 255;
        }
    };
    // Drop the slave so EOF propagates once the child exits.
    drop(pair.slave);

    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("sgo: failed to read from pty: {}", e);
            return 255;
        }
    };
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("sgo: failed to write to pty: {}", e);
            return 255;
        }
    };

    // --- Password phase -----------------------------------------------------
    // Suppress ssh's pre-auth chatter (the prompt, host-key warnings) and feed
    // the password as soon as we recognize the prompt. We force password auth
    // upstream, so a prompt is guaranteed unless the connection itself fails.
    let mut acc = String::new();
    let mut buf = [0u8; 4096];
    let mut authed = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // child exited before prompting (e.g. connection refused)
            Ok(n) => {
                acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                if acc.to_lowercase().contains("password:") {
                    let _ = writer.write_all(password.as_bytes());
                    let _ = writer.write_all(b"\n");
                    let _ = writer.flush();
                    authed = true;
                    break;
                }
                // Bound the buffer; prompts arrive early and small.
                if acc.len() > 16384 {
                    acc.drain(..8192);
                }
            }
            Err(_) => break,
        }
    }

    if !authed {
        // Never reached the prompt — surface whatever ssh printed and exit.
        eprint!("{}", acc);
        let _ = std::io::stderr().flush();
        let status = child.wait();
        return status.map(|s| s.exit_code() as i32).unwrap_or(255);
    }

    // --- Proxy phase --------------------------------------------------------
    let raw_enabled = raw && stdin_is_tty && enable_raw_mode().is_ok();

    // stdin -> pty
    thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if writer.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
                Err(_) => break,
            }
        }
        // Dropping the writer here closes the pty input so the remote sees EOF.
    });

    // Keep the master alive for the duration of the session. When in raw mode
    // we also poll for terminal resizes and forward them to the PTY.
    let mut master_keep = Some(pair.master);
    if raw_enabled {
        let m = master_keep.take().unwrap();
        let mut last = (cols, rows);
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(300));
            if let Ok(sz) = term_size() {
                if sz != last {
                    last = sz;
                    let _ = m.resize(PtySize {
                        rows: sz.1,
                        cols: sz.0,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
            }
        });
    }

    // pty -> stdout
    let mut stdout = std::io::stdout();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let _ = stdout.write_all(&buf[..n]);
                let _ = stdout.flush();
            }
            Err(_) => break,
        }
    }

    let status = child.wait();
    if raw_enabled {
        let _ = disable_raw_mode();
    }
    drop(master_keep);

    status.map(|s| s.exit_code() as i32).unwrap_or(255)
}
