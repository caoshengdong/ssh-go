//! End-to-end tests for `sgo exec` over the password/PTY path.
//!
//! These stand a fake `ssh` up on PATH and point sgo at a throwaway config, so
//! the whole runner is exercised — prompt detection, password feeding, the pty's
//! terminal modes, stdin forwarding and EOF — without needing a real server.
//!
//! What they are guarding against, all of it observed in the wild:
//!   - a heredoc or pipe arriving empty, because ssh was given `-n`
//!   - the payload picking up stray bytes on the way through the pty
//!   - the remote's `\n` coming back as `\r\n`, which corrupts byte-exact output
//!   - output that lands in the same read as the password prompt being dropped
//!
//! Unix only: the fake ssh is a shell script, and the terminal modes under test
//! do not exist on Windows.
#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A fake `ssh` that reports what it was actually handed.
///
/// It prompts the way OpenSSH does: on `/dev/tty`, not on stdin. That is the
/// whole reason a pty is involved, and reproducing it here is what makes these
/// tests meaningful — a fake that read the password from stdin would pass no
/// matter how the session's data was routed.
///
/// stdout carries a marker before the prompt and a trailer after the payload,
/// so a test can tell whether any part of the session's output was swallowed.
const FAKE_SSH: &str = r#"#!/bin/sh
for a in "$@"; do printf '%s\n' "$a" >> "$SGO_TEST_OUT/argv"; done
printf 'HEAD_MARKER\n'
printf "fake@host's password:" > /dev/tty
IFS= read -r pw < /dev/tty
printf 'PW=%s\n' "$pw"
case " $* " in
  *" -n "*) printf 'NO_STDIN\n' ;;
  *) cat > "$SGO_TEST_OUT/stdin" ;;
esac
printf 'TRAILER\n'
"#;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sgo-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::create_dir_all(dir.join("home/.ssh-go")).unwrap();
        fs::create_dir_all(dir.join("out")).unwrap();

        let ssh = dir.join("bin/ssh");
        fs::write(&ssh, FAKE_SSH).unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();

        fs::write(
            dir.join("home/.ssh-go/servers.json"),
            r#"[{"alias":"testpw","host":"127.0.0.1","port":22,"user":"tester",
                 "auth":{"type":"password","value":"dummy-pw"}}]"#,
        )
        .unwrap();

        Fixture { dir }
    }

    /// Run `sgo exec` with `payload` on stdin, returning what sgo wrote to
    /// stdout and stderr.
    fn exec(&self, args: &[&str], payload: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let mut child = Command::new(env!("CARGO_BIN_EXE_sgo"))
            .arg("exec")
            .args(args)
            .env("HOME", self.dir.join("home"))
            .env("PATH", path)
            .env("SGO_TEST_OUT", self.dir.join("out"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        child.stdin.take().unwrap().write_all(payload).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.stdout, out.stderr)
    }

    fn captured(&self, name: &str) -> Vec<u8> {
        fs::read(self.dir.join("out").join(name)).unwrap_or_default()
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.path());
    }
}

#[test]
fn forwards_piped_stdin_to_the_remote_command() {
    let fx = Fixture::new("pipe");
    let payload = b"line one\nline two with 'quotes' and $vars\n";

    fx.exec(&["testpw", "cat"], payload);

    assert_eq!(
        fx.captured("stdin"),
        payload,
        "a heredoc or pipe must reach the remote command unchanged"
    );
}

#[test]
fn forwards_stdin_that_does_not_end_in_a_newline() {
    let fx = Fixture::new("nonl");
    let payload = b"no-trailing-newline";

    fx.exec(&["testpw", "cat"], payload);

    // The EOF is delivered out-of-band, so nothing may be appended to close the
    // final line.
    assert_eq!(fx.captured("stdin"), payload);
}

#[test]
fn forwards_binary_stdin_byte_for_byte() {
    let fx = Fixture::new("binary");
    // Every byte value, so the ones a cooked pty would act on rather than pass
    // through are all covered: NUL, ^C, ^D, ^Z, CR, and XON/XOFF.
    let payload: Vec<u8> = (0..=255u8).cycle().take(64 * 1024).collect();

    fx.exec(&["testpw", "cat > /tmp/x"], &payload);

    assert_eq!(fx.captured("stdin"), payload);
}

#[test]
fn output_is_not_rewritten_on_the_way_out() {
    let fx = Fixture::new("crlf");

    let (out, _) = fx.exec(&["testpw", "true"], b"");

    assert!(
        !out.windows(2).any(|w| w == b"\r\n"),
        "the remote's \\n must not arrive as \\r\\n: {:?}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn keeps_output_written_before_the_password_was_answered() {
    let fx = Fixture::new("head");

    let (out, _) = fx.exec(&["testpw", "true"], b"");
    let text = String::from_utf8_lossy(&out);

    // Suppressing ssh's pre-auth chatter must not cost the session anything the
    // remote itself printed, whenever it printed it.
    assert!(
        text.contains("HEAD_MARKER"),
        "output from before the prompt was swallowed: {:?}",
        text
    );
    assert!(
        text.contains("TRAILER"),
        "output from after the payload was swallowed: {:?}",
        text
    );
}

#[test]
fn the_password_reaches_ssh_and_goes_nowhere_else() {
    let fx = Fixture::new("echo");

    let (out, err) = fx.exec(&["testpw", "true"], b"");
    let text = String::from_utf8_lossy(&out);

    // The fake reads the password from /dev/tty the way ssh does and echoes it
    // back as `PW=...`, so this proves it was actually delivered.
    assert!(
        text.contains("PW=dummy-pw"),
        "ssh never received the password: {:?}",
        text
    );
    assert_eq!(
        text.matches("dummy-pw").count(),
        1,
        "the password must not be echoed into the session: {:?}",
        text
    );
    assert!(
        !String::from_utf8_lossy(&err).contains("dummy-pw"),
        "the password must not leak onto stderr"
    );
}

#[test]
fn passes_several_arguments_through_as_a_quoted_argv() {
    let fx = Fixture::new("argv");

    fx.exec(&["testpw", "--", "grep", "hello world", "*.mp4"], b"");

    let argv = String::from_utf8(fx.captured("argv")).unwrap();
    let remote_command = argv.lines().last().unwrap();
    assert_eq!(remote_command, "grep 'hello world' '*.mp4'");
}

#[test]
fn passes_a_single_argument_to_the_remote_shell_verbatim() {
    let fx = Fixture::new("single");

    fx.exec(&["testpw", "df -h | grep /var"], b"");

    let argv = String::from_utf8(fx.captured("argv")).unwrap();
    let remote_command = argv.lines().last().unwrap();
    assert_eq!(remote_command, "df -h | grep /var");
}
