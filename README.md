# ssh-go

A fast, minimal SSH server manager with fuzzy matching. Written in Rust.

Save your servers once, then connect with a short query — no more remembering IPs, ports, or passwords.

## Install

```bash
cargo install --path .
```

This installs the `sgo` binary.

## Usage

```bash
# Connect to a server by fuzzy query
sgo myserver
sgo 192.168.1
sgo prod

# Add a new server interactively
sgo add

# List all saved servers
sgo list

# Edit a server
sgo edit myserver

# Remove a server
sgo remove myserver
```

## Fuzzy Matching

When connecting, `sgo` matches your query against saved servers with the following priority:

1. **Exact alias match** — alias equals the query
2. **IP suffix match** — host ends with the query (e.g., `.100` matches `192.168.1.100`)
3. **Alias substring match** — alias contains the query (case-insensitive)
4. **IP substring match** — host contains the query

If multiple servers match, you'll be prompted to select one.

## Authentication

Three methods are supported when adding a server:

- **Password** — stored locally, auto-filled via `expect` on connect
- **SSH Key** — connects with `-i <keyfile>`
- **None** — plain `ssh` connection

## Configuration

Server configs are stored in `~/.ssh-go/servers.json`. The directory and file are created automatically with restricted permissions (`700` / `600`).

## Requirements

- Rust 1.56+ (2021 edition)
- `expect` (only needed for password-based authentication)

## License

MIT
