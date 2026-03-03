mod config;
mod matcher;
mod ssh;

use clap::{Parser, Subcommand};
use colored::Colorize;
use config::{Auth, Server};
use dialoguer::{Input, Password, Select};

/// When ssh invokes us as SSH_ASKPASS, just print the password and exit.
fn maybe_handle_askpass() {
    if let Ok(pass) = std::env::var("SGO_PASS") {
        print!("{}", pass);
        std::process::exit(0);
    }
}

#[derive(Parser)]
#[command(name = "sgo", about = "SSH server manager with fuzzy matching")]
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
}

fn main() {
    maybe_handle_askpass();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Add) => cmd_add(),
        Some(Commands::List) => cmd_list(),
        Some(Commands::Edit { query }) => cmd_edit(&query),
        Some(Commands::Remove { query }) => cmd_remove(&query),
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
