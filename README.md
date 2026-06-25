# ssh-go

A fast, minimal SSH server manager with fuzzy matching. Written in Rust.

Save your servers once, then connect with a short query — no more remembering IPs, ports, or passwords.

`sgo` is cross-platform. It uses the system OpenSSH client (`ssh`) on Linux, macOS, and Windows.

## Install

Linux/macOS:

```bash
cargo install --path .
```

Windows PowerShell:

```powershell
cargo install --path .
```

This installs the `sgo` binary.

## OpenSSH Client

`sgo` launches `ssh` from your `PATH`.

Linux/macOS:

```bash
ssh -V
```

Windows PowerShell:

```powershell
ssh -V
```

On Linux and macOS, OpenSSH is usually already installed or available through your package manager.

On Windows, if `ssh` is not found, install the Windows OpenSSH Client from **Settings > Optional features**,
or run PowerShell as Administrator:

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0
```

## Usage

The CLI commands are the same on every platform. The main differences are shell syntax for paths,
environment variables, and quoting.

### Linux/macOS

```bash
# Show all saved servers
sgo
sgo list

# Connect to a server by fuzzy query
sgo myserver
sgo 192.168.1
sgo prod

# Print the underlying ssh command instead of running it
sgo --print-ssh prod

# Add a new server interactively
sgo add

# Edit a server
sgo edit myserver

# Remove a server
sgo remove myserver

# Run a remote command non-interactively
sgo exec prod "uptime"
sgo exec prod 'df -h | grep /var'

# Open SSH tunnels
sgo tunnel prod 8080
sgo tunnel prod 8080:9090
sgo tunnel prod 8080:db.internal:5432
sgo tunnel prod -d 1080
sgo tunnel prod -r 8080
```

### Windows PowerShell

```powershell
# Show all saved servers
sgo
sgo list

# Connect to a server by fuzzy query
sgo myserver
sgo 192.168.1
sgo prod

# Print the underlying ssh command instead of running it
sgo --print-ssh prod

# Add a new server interactively
sgo add

# Edit a server
sgo edit myserver

# Remove a server
sgo remove myserver

# Run a remote command non-interactively
sgo exec prod "uptime"
sgo exec prod 'df -h | grep /var'

# Open SSH tunnels
sgo tunnel prod 8080
sgo tunnel prod 8080:9090
sgo tunnel prod 8080:db.internal:5432
sgo tunnel prod -d 1080
sgo tunnel prod -r 8080
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

- **Password** — stored locally, auto-filled through a PTY-backed OpenSSH session
- **SSH Key** — connects with `-i <keyfile>`
- **None** — plain `ssh` connection

## Configuration

Server configs are stored in the current user's home directory:

- Linux/macOS: `~/.ssh-go/servers.json`
- Windows: `C:\Users\<you>\.ssh-go\servers.json`

The directory and file are created automatically. On Unix-like systems, `sgo` also applies restricted
permissions (`700` / `600`); on Windows it relies on your user profile ACLs.

For tests or scripted runs, set `SGO_CONFIG_DIR` to use a temporary config directory:

Linux/macOS:

```bash
export SGO_CONFIG_DIR="$PWD/.tmp-sgo-config"
sgo list
unset SGO_CONFIG_DIR
```

Windows PowerShell:

```powershell
$env:SGO_CONFIG_DIR = "$PWD\.tmp-sgo-config"
sgo list
Remove-Item Env:\SGO_CONFIG_DIR
```

## Requirements

- Rust toolchain with Cargo
- OpenSSH client (`ssh`) in `PATH`

## License

MIT
