//! Password authentication without putting the password on a command line.
//!
//! Windows' built-in OpenSSH ignores `SSH_ASKPASS` (see Win32-OpenSSH #2115),
//! so we cannot auto-fill passwords via an askpass helper. Instead we give ssh
//! a pseudo-terminal, watch it for the password prompt, and type the stored
//! password into it.
//!
//! There are two ways to hand ssh a pty, and which one is right depends on what
//! the session is carrying:
//!
//!   - `Mode::Command` on Unix runs ssh with the real stdin/stdout/stderr and
//!     uses the pty *only* as its controlling terminal (see `ctty`). ssh asks
//!     for the password on `/dev/tty`, which is the pty; the session's data
//!     never goes near it. This is what keeps `sgo exec` byte-exact.
//!   - Everything else runs ssh fully inside the pty. An interactive shell
//!     wants that — it is how keystrokes and terminal modes reach the remote —
//!     and a tunnel carries no stdio data to corrupt.
//!
//! This path is only used for password auth. Key/none auth runs `ssh` directly
//! with an inherited terminal (see `ssh::exec_or_status`).

use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size as term_size};
use crossterm::tty::IsTty;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

/// What the session is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// An interactive remote shell. Keystrokes (including Ctrl+C) pass through,
    /// and the pty keeps the terminal modes ssh negotiates for the remote tty.
    Shell,
    /// A port forward (`ssh -N`): no session data in either direction.
    Tunnel,
    /// A one-shot remote command whose stdin, stdout and stderr carry data that
    /// must arrive unaltered.
    Command,
}

pub struct Opts {
    pub mode: Mode,
    /// Copy this process's stdin into the session. Ignored when stdin is handed
    /// to ssh directly, which is the case for `Mode::Command` on Unix.
    pub forward_stdin: bool,
}

/// Run `ssh` (or any program) under a pty, feeding `password` when the prompt
/// appears. Returns the child's exit code.
pub fn run(program: &str, args: &[String], password: &str, opts: Opts) -> i32 {
    #[cfg(unix)]
    if opts.mode == Mode::Command {
        return ctty::run(program, args, password);
    }

    run_inside_pty(program, args, password, opts)
}

/// Run the child with its stdio wired to a pty, then proxy that pty.
fn run_inside_pty(program: &str, args: &[String], password: &str, opts: Opts) -> i32 {
    let interactive = opts.mode == Mode::Shell;
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

    // Left cooked, the line discipline rewrites every `\n` the child prints as
    // `\r\n` and echoes back whatever is typed into it. An interactive shell
    // wants those modes (ssh negotiates them with the remote tty); a tunnel's
    // log output does not.
    if !interactive {
        make_pty_raw(&*pair.master);
    }

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
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let mut authed = false;
    // Bytes the child wrote after the prompt in the same read. They are session
    // output, not pre-auth chatter, so they must survive into the proxy phase.
    let mut leftover: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // child exited before prompting (e.g. connection refused)
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                if let Some(end) = find_password_prompt(&acc) {
                    let _ = writer.write_all(password.as_bytes());
                    let _ = writer.write_all(b"\n");
                    let _ = writer.flush();
                    leftover = acc[end..].to_vec();
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
        eprint!("{}", String::from_utf8_lossy(&acc));
        let _ = std::io::stderr().flush();
        let status = child.wait();
        return status.map(|s| s.exit_code() as i32).unwrap_or(255);
    }

    // --- Proxy phase --------------------------------------------------------
    let raw_enabled = interactive && stdin_is_tty && enable_raw_mode().is_ok();

    let mut stdout = std::io::stdout();
    if !leftover.is_empty() {
        let _ = stdout.write_all(&leftover);
        let _ = stdout.flush();
    }

    // stdin -> pty
    let mut writer_keep = None;
    if opts.forward_stdin {
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
    } else {
        // ssh was given `-n` and ignores stdin. Hold the writer open anyway:
        // dropping it would signal EOF on a session that is still running.
        writer_keep = Some(writer);
    }

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
    drop(writer_keep);
    drop(master_keep);

    status.map(|s| s.exit_code() as i32).unwrap_or(255)
}

/// Byte offset just past a case-insensitive `password:` in `haystack`.
fn find_password_prompt(haystack: &[u8]) -> Option<usize> {
    const NEEDLE: &[u8] = b"password:";
    haystack
        .windows(NEEDLE.len())
        .position(|w| w.eq_ignore_ascii_case(NEEDLE))
        .map(|start| start + NEEDLE.len())
}

/// Put the pty in raw mode: no output post-processing, no echo, no canonical
/// line editing. Failures are not fatal — the session still works, it just
/// mangles `\n` and echoes its input.
#[cfg(unix)]
fn make_pty_raw(master: &dyn MasterPty) {
    let fd = match master.as_raw_fd() {
        Some(fd) => fd,
        None => return,
    };

    unsafe {
        let mut tio: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut tio) != 0 {
            return;
        }
        libc::cfmakeraw(&mut tio);
        libc::tcsetattr(fd, libc::TCSANOW, &tio);
    }
}

/// ConPTY renders a terminal rather than exposing a line discipline, so there
/// is nothing to switch off.
#[cfg(windows)]
fn make_pty_raw(_master: &dyn MasterPty) {}

/// Give ssh a pty as its controlling terminal and nothing more.
///
/// ssh asks for a password on `/dev/tty`, not on stdin, so answering it needs a
/// terminal — but nothing says the session's data has to travel through that
/// terminal as well. Here ssh inherits the real stdin, stdout and stderr and
/// only its `/dev/tty` is ours, which is what makes `sgo exec` byte-exact:
/// there is no line discipline in the data path to rewrite `\n` as `\r\n`, echo
/// a payload back into the output, cap an input line at the canonical buffer
/// size, or read a stray 0x04 in a binary stream as end-of-file. EOF, exit
/// codes and Ctrl-C all behave the way they do for plain `ssh`.
#[cfg(unix)]
mod ctty {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::thread;

    pub fn run(program: &str, args: &[String], password: &str) -> i32 {
        let (master, slave_name) = match open_master() {
            Some(pair) => pair,
            None => {
                eprintln!("sgo: failed to open pty: {}", io::Error::last_os_error());
                return 255;
            }
        };

        // Hold the slave open here, for as long as the session lasts. ssh closes
        // the descriptors it inherits on startup, so a slave left open only in
        // the child disappears the moment it execs — and the master then reports
        // end-of-file before ssh has even opened /dev/tty to ask for a password.
        // Close-on-exec so it stays out of ssh's own descriptor table.
        let _slave = match open_slave(&slave_name) {
            Some(fd) => fd,
            None => {
                eprintln!("sgo: failed to open pty: {}", io::Error::last_os_error());
                return 255;
            }
        };

        let master_fd = master.as_raw_fd();

        let mut cmd = Command::new(program);
        cmd.args(args);
        // stdin/stdout/stderr are inherited by default — that is the point.
        unsafe {
            cmd.pre_exec(move || {
                // Leave the caller's session, so `/dev/tty` in the child
                // resolves to the pty below rather than to the user's terminal.
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }

                let fd = libc::open(slave_name.as_ptr(), libc::O_RDWR);
                if fd == -1 {
                    return Err(io::Error::last_os_error());
                }

                // Claim it explicitly rather than relying on open() conferring a
                // controlling terminal, which varies between systems.
                libc::ioctl(fd, libc::TIOCSCTTY as _, 0);

                // The association outlives the descriptor, and the parent holds
                // the slave open, so there is no reason to hand ssh a stray fd.
                libc::close(fd);
                Ok(())
            });
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("sgo: failed to run {}: {}", program, e);
                return 255;
            }
        };

        let mut tty = File::from(master);
        let mut acc: Vec<u8> = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            // The pty never reports end-of-file while this process holds the
            // slave, so ssh giving up has to be noticed rather than waited for.
            if !wait_readable(master_fd, 100) {
                match child.try_wait() {
                    // Gone without ever asking: it already said why on the
                    // stderr it inherited, so there is nothing to add.
                    Ok(Some(_)) => break,
                    Ok(None) => continue,
                    Err(_) => break,
                }
            }

            match tty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    acc.extend_from_slice(&buf[..n]);
                    if super::find_password_prompt(&acc).is_some() {
                        // OpenSSH turns echo off itself before it prompts, and
                        // restores the modes it found afterwards. Setting it
                        // here, with the prompt on screen, covers anything that
                        // does not, so the password is never typed back at us.
                        silence_echo(master_fd);
                        let _ = tty.write_all(password.as_bytes());
                        let _ = tty.write_all(b"\n");
                        let _ = tty.flush();
                        break;
                    }
                    if acc.len() > 16384 {
                        acc.drain(..8192);
                    }
                }
                Err(_) => break,
            }
        }

        // Whatever else ssh says on /dev/tty — a re-prompt, a host key question
        // — belongs in front of the user, and draining keeps a full pty buffer
        // from blocking ssh.
        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            // ssh ends the hidden password with a newline of its own, to move
            // the cursor off the prompt line. There is no prompt on the user's
            // screen to move off, so it would just be a blank line in front of
            // every command's output.
            let mut leading_blanks = true;
            while let Ok(n) = tty.read(&mut buf) {
                if n == 0 {
                    break;
                }

                let mut chunk = &buf[..n];
                if leading_blanks {
                    let start = chunk
                        .iter()
                        .position(|b| *b != b'\r' && *b != b'\n')
                        .unwrap_or(chunk.len());
                    chunk = &chunk[start..];
                    leading_blanks = chunk.is_empty();
                }

                if !chunk.is_empty() {
                    let _ = io::stderr().write_all(chunk);
                    let _ = io::stderr().flush();
                }
            }
        });

        match child.wait() {
            Ok(s) => s.code().unwrap_or(255),
            Err(e) => {
                eprintln!("sgo: {} did not run to completion: {}", program, e);
                255
            }
        }
    }

    /// Wait up to `timeout_ms` for the pty to have something to read.
    fn wait_readable(fd: RawFd, timeout_ms: i32) -> bool {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut pfd, 1, timeout_ms) > 0 }
    }

    /// Open the slave end, close-on-exec so ssh does not inherit it.
    fn open_slave(name: &CString) -> Option<OwnedFd> {
        unsafe {
            let fd = libc::open(
                name.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            );
            if fd < 0 {
                return None;
            }
            Some(OwnedFd::from_raw_fd(fd))
        }
    }

    /// Open a pty master and return it with the path of its slave.
    fn open_master() -> Option<(OwnedFd, CString)> {
        unsafe {
            let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if fd < 0 {
                return None;
            }

            // Owned from here on, so an early return still closes it.
            let master = OwnedFd::from_raw_fd(fd);
            if libc::grantpt(fd) != 0 || libc::unlockpt(fd) != 0 {
                return None;
            }

            // Static storage, so copy it before anything else can call ptsname.
            let name = libc::ptsname(fd);
            if name.is_null() {
                return None;
            }

            Some((master, std::ffi::CStr::from_ptr(name).to_owned()))
        }
    }

    /// Turn off echo on the pty. OpenSSH does this itself before writing the
    /// prompt; doing it up front covers anything that does not.
    fn silence_echo(fd: RawFd) {
        unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut tio) != 0 {
                return;
            }
            tio.c_lflag &= !(libc::ECHO | libc::ECHONL);
            libc::tcsetattr(fd, libc::TCSANOW, &tio);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_password_prompt_case_insensitively() {
        assert_eq!(find_password_prompt(b"user@host's password:"), Some(21));
        assert_eq!(find_password_prompt(b"Password:"), Some(9));
        assert_eq!(find_password_prompt(b"no prompt here"), None);
    }

    #[test]
    fn keeps_bytes_that_follow_the_prompt() {
        let acc = b"password:hello";
        let end = find_password_prompt(acc).unwrap();
        assert_eq!(&acc[end..], b"hello");
    }
}
