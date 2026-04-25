mod config;
mod matcher;
mod ssh;
mod tunnel;

use clap::{Parser, Subcommand};
use colored::Colorize;
use config::{Auth, Server};
use dialoguer::{Input, Password, Select};
use tunnel::TunnelMode;

/// When ssh invokes us as SSH_ASKPASS, just print the password and exit.
fn maybe_handle_askpass() {
    if let Ok(pass) = std::env::var("SGO_PASS") {
        print!("{}", pass);
        std::process::exit(0);
    }
}

#[derive(Parser)]
#[command(
    name = "sgo",
    about = "SSH server manager with fuzzy matching",
    long_about = "SSH server manager with fuzzy matching.\n\n\
        With no subcommand, fuzzy-matches QUERY against saved servers and connects.\n\n\
        Examples:\n  \
          sgo                       list all saved servers\n  \
          sgo prod                  connect to server matching \"prod\"\n  \
          sgo add                   add a new server interactively\n  \
          sgo tunnel prod 8080      open a tunnel to prod\n\n\
        Matching priority: exact alias > IP suffix > alias substring > IP substring."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Print the SSH command instead of executing it (for Warp terminal integration)
    #[arg(long)]
    print_ssh: bool,

    /// Query to fuzzy-match a server and connect
    query: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Add a new server interactively
    Add,
    /// List all saved servers
    List,
    /// Edit a server matching the query
    Edit {
        /// Query to match the server
        query: String,
    },
    /// Remove a server matching the query
    Remove {
        /// Query to match the server
        query: String,
    },
    /// Open an SSH tunnel to a server
    #[command(long_about = "Open an SSH tunnel to a server (runs in foreground, Ctrl+C to close).\n\n\
        Examples:\n  \
          sgo tunnel prod 8080                   local:8080 -> prod:8080\n  \
          sgo tunnel prod 8080:9090              local:8080 -> prod:9090\n  \
          sgo tunnel prod 8080:db.internal:5432  local:8080 -> db.internal:5432 (via prod)\n  \
          sgo tunnel prod -d 1080                SOCKS5 proxy on local:1080\n  \
          sgo tunnel prod -r 8080                prod:8080 -> local:8080\n  \
          sgo tunnel prod 8080 -vv               enable ssh verbose logging")]
    Tunnel {
        /// Query to match the server
        query: String,
        /// Port spec: PORT | LOCAL:REMOTE | LOCAL:HOST:REMOTE (or single port with -d/-r)
        port_spec: String,
        /// Dynamic forward (SOCKS5 proxy): port_spec is a single local port
        #[arg(short = 'd', long)]
        dynamic: bool,
        /// Reverse forward: port_spec is REMOTE or REMOTE:LOCAL
        #[arg(short = 'r', long)]
        reverse: bool,
        /// Verbose ssh output (-v, -vv, -vvv). Repeat for more detail.
        #[arg(short = 'v', long, action = clap::ArgAction::Count)]
        verbose: u8,
    },
}

fn main() {
    maybe_handle_askpass();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Add) => cmd_add(),
        Some(Commands::List) => cmd_list(),
        Some(Commands::Edit { query }) => cmd_edit(&query),
        Some(Commands::Remove { query }) => cmd_remove(&query),
        Some(Commands::Tunnel {
            query,
            port_spec,
            dynamic,
            reverse,
            verbose,
        }) => cmd_tunnel(&query, &port_spec, dynamic, reverse, verbose),
        None => {
            if let Some(query) = cli.query {
                if cli.print_ssh {
                    cmd_print_ssh(&query);
                } else {
                    cmd_connect(&query);
                }
            } else {
                // No query and no subcommand — show list
                cmd_list();
            }
        }
    }
}

fn cmd_add() {
    println!("{}", "Add a new SSH server".green().bold());

    let alias: String = Input::new()
        .with_prompt("Alias")
        .interact_text()
        .unwrap();

    let host: String = Input::new()
        .with_prompt("Host (IP or hostname)")
        .interact_text()
        .unwrap();

    let port: u16 = Input::new()
        .with_prompt("Port")
        .default(22)
        .interact_text()
        .unwrap();

    let user: String = Input::new()
        .with_prompt("Username")
        .default("root".to_string())
        .interact_text()
        .unwrap();

    let auth_options = &["Password", "SSH Key", "None"];
    let auth_choice = Select::new()
        .with_prompt("Authentication method")
        .items(auth_options)
        .default(0)
        .interact()
        .unwrap();

    let auth = match auth_choice {
        0 => {
            let pw: String = Password::new()
                .with_prompt("Password")
                .interact()
                .unwrap();
            Some(Auth::Password(pw))
        }
        1 => {
            let key: String = Input::new()
                .with_prompt("Key file path")
                .default("~/.ssh/id_rsa".to_string())
                .interact_text()
                .unwrap();
            Some(Auth::Key(key))
        }
        _ => None,
    };

    let server = Server {
        alias,
        host,
        port,
        user,
        auth,
    };

    let mut servers = config::load_servers().unwrap_or_default();
    println!(
        "{} {}",
        "Added:".green().bold(),
        server.to_string().white()
    );
    servers.push(server);
    config::save_servers(&servers).expect("Failed to save config");
}

fn cmd_list() {
    let servers = config::load_servers().unwrap_or_default();
    if servers.is_empty() {
        println!(
            "{}",
            "No servers configured. Use `sgo add` to add one.".yellow()
        );
        return;
    }

    println!("{}", "Saved servers:".green().bold());
    for (i, s) in servers.iter().enumerate() {
        let auth_str = match &s.auth {
            Some(Auth::Password(_)) => "password".dimmed().to_string(),
            Some(Auth::Key(p)) => format!("key: {}", p).dimmed().to_string(),
            None => "none".dimmed().to_string(),
        };
        println!(
            "  {} {} {}@{}:{} [{}]",
            format!("[{}]", i + 1).dimmed(),
            s.alias.cyan().bold(),
            s.user,
            s.host,
            s.port,
            auth_str,
        );
    }
}

fn cmd_edit(query: &str) {
    let mut servers = config::load_servers().unwrap_or_default();
    let matched = matcher::match_servers(&servers, query);

    let index = match resolve_single(&servers, &matched, query) {
        Some(i) => i,
        None => return,
    };

    let server = &servers[index];
    println!(
        "{} {}",
        "Editing:".green().bold(),
        server.to_string().white()
    );

    let alias: String = Input::new()
        .with_prompt("Alias")
        .default(server.alias.clone())
        .interact_text()
        .unwrap();

    let host: String = Input::new()
        .with_prompt("Host")
        .default(server.host.clone())
        .interact_text()
        .unwrap();

    let port: u16 = Input::new()
        .with_prompt("Port")
        .default(server.port)
        .interact_text()
        .unwrap();

    let user: String = Input::new()
        .with_prompt("Username")
        .default(server.user.clone())
        .interact_text()
        .unwrap();

    let auth_options = &["Password", "SSH Key", "None"];
    let default_auth = match &server.auth {
        Some(Auth::Password(_)) => 0,
        Some(Auth::Key(_)) => 1,
        None => 2,
    };
    let auth_choice = Select::new()
        .with_prompt("Authentication method")
        .items(auth_options)
        .default(default_auth)
        .interact()
        .unwrap();

    let auth = match auth_choice {
        0 => {
            let pw: String = Password::new()
                .with_prompt("Password")
                .interact()
                .unwrap();
            Some(Auth::Password(pw))
        }
        1 => {
            let default_key = match &server.auth {
                Some(Auth::Key(k)) => k.clone(),
                _ => "~/.ssh/id_rsa".to_string(),
            };
            let key: String = Input::new()
                .with_prompt("Key file path")
                .default(default_key)
                .interact_text()
                .unwrap();
            Some(Auth::Key(key))
        }
        _ => None,
    };

    servers[index] = Server {
        alias,
        host,
        port,
        user,
        auth,
    };

    config::save_servers(&servers).expect("Failed to save config");
    println!("{}", "Server updated.".green().bold());
}

fn cmd_remove(query: &str) {
    let mut servers = config::load_servers().unwrap_or_default();
    let matched = matcher::match_servers(&servers, query);

    let index = match resolve_single(&servers, &matched, query) {
        Some(i) => i,
        None => return,
    };

    let server = &servers[index];
    println!(
        "{} {}",
        "Removing:".red().bold(),
        server.to_string().white()
    );

    let confirm = Select::new()
        .with_prompt("Are you sure?")
        .items(&["Yes", "No"])
        .default(1)
        .interact()
        .unwrap();

    if confirm == 0 {
        servers.remove(index);
        config::save_servers(&servers).expect("Failed to save config");
        println!("{}", "Server removed.".green().bold());
    } else {
        println!("{}", "Cancelled.".yellow());
    }
}

fn cmd_print_ssh(query: &str) {
    let servers = config::load_servers().unwrap_or_default();
    if servers.is_empty() {
        eprintln!("No servers configured. Use `sgo add` to add one.");
        std::process::exit(1);
    }

    let matched = matcher::match_servers(&servers, query);

    if matched.is_empty() {
        eprintln!("No server matching \"{}\"", query);
        std::process::exit(1);
    }

    let server = if matched.len() == 1 {
        matched[0]
    } else {
        // Interactive selector writes to stderr, so it won't pollute stdout
        let items: Vec<String> = matched.iter().map(|s| s.to_string()).collect();
        let choice = Select::new()
            .with_prompt("Multiple matches — select a server")
            .items(&items)
            .default(0)
            .interact()
            .unwrap();
        matched[choice]
    };

    ssh::print_command(server);
}

fn cmd_connect(query: &str) {
    let servers = config::load_servers().unwrap_or_default();
    if servers.is_empty() {
        println!(
            "{}",
            "No servers configured. Use `sgo add` to add one.".yellow()
        );
        return;
    }

    let matched = matcher::match_servers(&servers, query);

    if matched.is_empty() {
        println!(
            "{} No server matching \"{}\"",
            "Error:".red().bold(),
            query
        );
        return;
    }

    let server = if matched.len() == 1 {
        matched[0]
    } else {
        println!(
            "{} Multiple servers match \"{}\":",
            "Note:".yellow().bold(),
            query
        );
        let items: Vec<String> = matched.iter().map(|s| s.to_string()).collect();
        let choice = Select::new()
            .with_prompt("Select a server")
            .items(&items)
            .default(0)
            .interact()
            .unwrap();
        matched[choice]
    };

    println!(
        "{} {}@{}:{}",
        "Connecting to".green().bold(),
        server.user,
        server.host,
        server.port
    );

    ssh::connect(server);
}

fn cmd_tunnel(query: &str, port_spec: &str, dynamic: bool, reverse: bool, verbose: u8) {
    if dynamic && reverse {
        println!(
            "{} -d and -r cannot be used together",
            "Error:".red().bold()
        );
        return;
    }

    let mode = if dynamic {
        match port_spec.parse::<u16>() {
            Ok(port) => TunnelMode::Dynamic { port },
            Err(_) => {
                println!(
                    "{} invalid port for dynamic forward: \"{}\"",
                    "Error:".red().bold(),
                    port_spec
                );
                return;
            }
        }
    } else if reverse {
        let parts: Vec<&str> = port_spec.split(':').collect();
        match parts.as_slice() {
            [p] => match p.parse::<u16>() {
                Ok(port) => TunnelMode::Reverse {
                    remote_port: port,
                    local_host: "localhost".to_string(),
                    local_port: port,
                },
                Err(_) => {
                    println!("{} invalid port: \"{}\"", "Error:".red().bold(), p);
                    return;
                }
            },
            [r, l] => {
                let remote = match r.parse::<u16>() {
                    Ok(p) => p,
                    Err(_) => {
                        println!("{} invalid port: \"{}\"", "Error:".red().bold(), r);
                        return;
                    }
                };
                let local = match l.parse::<u16>() {
                    Ok(p) => p,
                    Err(_) => {
                        println!("{} invalid port: \"{}\"", "Error:".red().bold(), l);
                        return;
                    }
                };
                TunnelMode::Reverse {
                    remote_port: remote,
                    local_host: "localhost".to_string(),
                    local_port: local,
                }
            }
            _ => {
                println!(
                    "{} invalid reverse spec \"{}\" — expected PORT or REMOTE:LOCAL",
                    "Error:".red().bold(),
                    port_spec
                );
                return;
            }
        }
    } else {
        match tunnel::parse_local_spec(port_spec) {
            Ok(m) => m,
            Err(e) => {
                println!("{} {}", "Error:".red().bold(), e);
                return;
            }
        }
    };

    let servers = config::load_servers().unwrap_or_default();
    if servers.is_empty() {
        println!(
            "{}",
            "No servers configured. Use `sgo add` to add one.".yellow()
        );
        return;
    }

    let matched = matcher::match_servers(&servers, query);

    if matched.is_empty() {
        println!(
            "{} No server matching \"{}\"",
            "Error:".red().bold(),
            query
        );
        return;
    }

    let server = if matched.len() == 1 {
        matched[0]
    } else {
        println!(
            "{} Multiple servers match \"{}\":",
            "Note:".yellow().bold(),
            query
        );
        let items: Vec<String> = matched.iter().map(|s| s.to_string()).collect();
        let choice = Select::new()
            .with_prompt("Select a server")
            .items(&items)
            .default(0)
            .interact()
            .unwrap();
        matched[choice]
    };

    eprintln!(
        "{} {}",
        "Tunnel:".green().bold(),
        mode.describe()
    );
    if verbose > 0 {
        eprintln!(
            "{} ssh verbose level = {}",
            "Verbose:".green().bold(),
            verbose
        );
    }

    tunnel::open(server, &mode, verbose);
}

/// Resolve matched servers to a single index in the original servers vec.
/// Returns None if no match or user cancels.
fn resolve_single(servers: &[Server], matched: &[&Server], query: &str) -> Option<usize> {
    if matched.is_empty() {
        println!(
            "{} No server matching \"{}\"",
            "Error:".red().bold(),
            query
        );
        return None;
    }

    let target = if matched.len() == 1 {
        matched[0]
    } else {
        println!(
            "{} Multiple servers match \"{}\":",
            "Note:".yellow().bold(),
            query
        );
        let items: Vec<String> = matched.iter().map(|s| s.to_string()).collect();
        let choice = Select::new()
            .with_prompt("Select a server")
            .items(&items)
            .default(0)
            .interact()
            .unwrap();
        matched[choice]
    };

    // Find index by pointer equality
    servers
        .iter()
        .position(|s| std::ptr::eq(s, target))
}
