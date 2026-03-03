use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub alias: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Option<Auth>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Auth {
    #[serde(rename = "password")]
    Password(String),
    #[serde(rename = "key")]
    Key(String),
}

impl std::fmt::Display for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let auth_info = match &self.auth {
            Some(Auth::Password(_)) => "password",
            Some(Auth::Key(path)) => path.as_str(),
            None => "none",
        };
        write!(
            f,
            "{} ({}@{}:{}) [auth: {}]",
            self.alias, self.user, self.host, self.port, auth_info
        )
    }
}

fn config_dir() -> PathBuf {
    let home = dirs::home_dir().expect("Cannot determine home directory");
    home.join(".ssh-helper")
}

fn config_path() -> PathBuf {
    config_dir().join("servers.json")
}

pub fn load_servers() -> io::Result<Vec<Server>> {
    let path = config_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&path)?;
    let servers: Vec<Server> =
        serde_json::from_str(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(servers)
}

pub fn save_servers(servers: &[Server]) -> io::Result<()> {
    let dir = config_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
    }

    let path = config_path();
    let data = serde_json::to_string_pretty(servers)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&path, data)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}
