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
  pty.rs      - Cross-platform PTY runner used for password auth
```

## Key Design Decisions

- Config lives in `~/.ssh-go/servers.json`, NOT in `~/.ssh/config`
- Directory permissions: 700, file permissions: 600 (gated behind `#[cfg(unix)]`)
- **Cross-platform** (Linux/macOS/Windows). Relies on the system `ssh` (Windows
  10/11 ship OpenSSH's `ssh.exe`).
- Key/none auth runs `ssh` with an inherited terminal: Unix `exec()`s (replaces
  the process); Windows spawns + waits and forwards the exit code (no `exec` on
  Windows). See `ssh::exec_or_status`.
- Password auth goes through `pty.rs`: spawn `ssh` in a PTY (ConPTY on Windows),
  detect the `password:` prompt, type the password, then proxy the session.
  `-o PubkeyAuthentication=no -o PreferredAuthentications=password,keyboard-interactive`
  forces a prompt so feeding is deterministic.
  - We do NOT use `SSH_ASKPASS` — Windows' OpenSSH ignores it on Win11
    (Win32-OpenSSH #2115).
  - `pty::run(.., raw)` — `raw=true` only for the interactive shell so Ctrl+C
    passes to the remote; `false` for tunnels/exec so Ctrl+C raises a signal.
- Matching is deterministic priority-based, not fuzzy/scoring

## Dependencies

- clap 4 (CLI parsing with derive)
- serde + serde_json (config serialization)
- dialoguer (interactive prompts: Input, Password, Select)
- colored (terminal colors)
- dirs (home directory detection)
- portable-pty (PTY/ConPTY for password auth)
- crossterm (raw mode + terminal size, cross-platform)
