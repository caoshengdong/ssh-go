# ssh-go (sgo)

A CLI SSH server manager with fuzzy matching, written in Rust.

## Build & Run

```bash
cargo build              # debug build
cargo build --release    # release build
cargo run -- <args>      # run with arguments, e.g. cargo run -- add
```

The binary name is `sgo`. After `cargo install --path .`, use `sgo` directly.

## Project Structure

```
src/
  main.rs     - CLI entry point (clap), subcommands: add/list/edit/remove, default is connect
  config.rs   - Server model (Server, Auth), JSON load/save from ~/.ssh-go/servers.json
  matcher.rs  - Fuzzy matching logic (exact alias > IP suffix > alias substring > IP substring)
  ssh.rs      - SSH connect/exec/print-command; routes auth to the right runner
  tunnel.rs   - SSH port forwarding (-L / -R / -D)
  pty.rs      - Password auth: PTY as ssh's terminal (shell/tunnel) or as its
                controlling terminal only (exec, Unix — see pty::ctty)
tests/
  exec_stdin.rs - End-to-end `sgo exec` tests against a fake ssh (Unix)
scripts/
  e2e-windows.ps1 - Manual Windows smoke test
```

## Key Design Decisions

- Config lives in `~/.ssh-go/servers.json`, NOT in `~/.ssh/config`
- Directory permissions: 700, file permissions: 600 (gated behind `#[cfg(unix)]`)
- **Cross-platform** (Linux/macOS/Windows). Relies on the system `ssh` (Windows
  10/11 ship OpenSSH's `ssh.exe`).
- Key/none auth runs `ssh` with an inherited terminal: Unix `exec()`s (replaces
  the process); Windows spawns + waits and forwards the exit code (no `exec` on
  Windows). See `ssh::exec_or_status`.
- Password auth goes through `pty.rs`: give `ssh` a PTY, detect the `password:`
  prompt, type the password.
  `-o PubkeyAuthentication=no -o PreferredAuthentications=password,keyboard-interactive`
  forces a prompt so feeding is deterministic.
  - We do NOT use `SSH_ASKPASS` — Windows' OpenSSH ignores it on Win11
    (Win32-OpenSSH #2115).
  - **`pty::Mode` decides how the PTY is used, and this matters:**
    - `Mode::Command` (exec) on Unix — the PTY is ssh's **controlling terminal
      only** (`pty::ctty`, the sshpass approach): setsid + TIOCSCTTY, while
      stdin/stdout/stderr stay the real ones. ssh prompts on `/dev/tty`, so the
      session's data never touches a line discipline. This is what makes `sgo
      exec` byte-exact and heredocs work. Do not "simplify" it back to running
      ssh inside the PTY — see the failure modes below.
    - `Mode::Shell` — ssh runs inside the PTY, cooked, local terminal in raw
      mode. An interactive shell needs that: it is how keystrokes and terminal
      modes reach the remote.
    - `Mode::Tunnel` — ssh runs inside the PTY, PTY set raw (no data to carry,
      but it keeps `-v` logs from being CRLF-mangled).
    - Windows has no controlling-terminal split, so `Mode::Command` runs inside
      ConPTY there and its output still gets `\n` → `\r\n`. Key auth is the
      byte-exact route on Windows.
  - Putting the data path through a PTY caused four separate bugs (all covered
    by `tests/exec_stdin.rs`): `\n` rewritten to `\r\n`, the payload echoed back
    into the output, input lines capped at the canonical buffer size, and a
    0x04 byte in a binary payload read as end-of-file. Raw mode fixes the first
    two but then removes VEOF, so there is no way left to signal EOF — which is
    why exec does not use that shape at all.
- `sgo exec` forwards local stdin unless it is a terminal (then ssh gets `-n`),
  so heredocs and pipes reach the remote command.
- `sgo exec host -- cmd args...` quotes each argument for the remote shell
  (`ssh::build_remote_command`); a single argument is still passed through
  verbatim so pipes and redirects keep working.
- Matching is deterministic priority-based, not fuzzy/scoring

## Dependencies

- clap 4 (CLI parsing with derive)
- serde + serde_json (config serialization)
- dialoguer (interactive prompts: Input, Password, Select)
- colored (terminal colors)
- dirs (home directory detection)
- portable-pty (PTY/ConPTY for password auth)
- crossterm (raw mode + terminal size, cross-platform)
