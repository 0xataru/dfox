//! Saved connections, and connections found on this machine.
//!
//! Saved connections live in `connections.toml` in the config directory.
//! Passwords never do: they go to the OS keychain ([`secrets`]).

pub mod pgpass;
pub mod secrets;
pub mod url;

use std::path::{Path, PathBuf};

use dfox_core::models::connections::{ConnectionParams, DbType, TlsMode};
use serde::{Deserialize, Serialize};

/// A colour to tell connections apart, such as red for production.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tag {
    #[default]
    None,
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl Tag {
    pub const ALL: [Tag; 7] = [
        Tag::None,
        Tag::Red,
        Tag::Orange,
        Tag::Yellow,
        Tag::Green,
        Tag::Blue,
        Tag::Purple,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tag::None => "none",
            Tag::Red => "red",
            Tag::Orange => "orange",
            Tag::Yellow => "yellow",
            Tag::Green => "green",
            Tag::Blue => "blue",
            Tag::Purple => "purple",
        }
    }
}

/// A connection as saved in `connections.toml`. It has no password field,
/// so a password can't end up in the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub name: String,
    pub driver: DbType,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The database to open; for SQLite, the file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    #[serde(default)]
    pub tls: TlsMode,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub tag: Tag,
}

impl Connection {
    pub fn new(name: &str, driver: DbType) -> Self {
        Self {
            name: name.to_string(),
            driver,
            host: String::new(),
            port: None,
            database: None,
            user: String::new(),
            tls: TlsMode::default(),
            read_only: false,
            tag: Tag::None,
        }
    }

    pub fn default_port(driver: DbType) -> u16 {
        match driver {
            DbType::MySql => 3306,
            _ => 5432,
        }
    }

    /// The database opened when none is set: one every server has.
    pub fn default_database(&self) -> &str {
        match self.driver {
            DbType::MySql => "mysql",
            _ => "postgres",
        }
    }

    /// Driver connection details, with `password`.
    pub fn params(&self, password: &str) -> ConnectionParams {
        ConnectionParams {
            password: password.to_string(),
            ..self.target()
        }
    }

    /// Where the connection goes, without a password: for pings and labels.
    pub fn target(&self) -> ConnectionParams {
        let host = match self.host.trim() {
            "" => "localhost",
            host => host,
        };
        let database = match (&self.database, self.driver) {
            (Some(database), _) => database.clone(),
            (None, DbType::Sqlite) => String::new(),
            (None, _) => self.default_database().to_string(),
        };
        ConnectionParams {
            host: host.to_string(),
            port: self.port.unwrap_or(Self::default_port(self.driver)),
            username: self.user.clone(),
            password: String::new(),
            database: Some(database),
            tls: self.tls,
            ca_file: None,
            ssh: None,
            tunnel: None,
            read_only: self.read_only,
        }
    }

    /// `user@host:port/db`, or the file path for SQLite.
    pub fn address(&self) -> String {
        if self.driver == DbType::Sqlite {
            return self.database.clone().unwrap_or_default();
        }
        let params = self.target();
        let mut address = format!("{}:{}", params.host, params.port);
        if !self.user.is_empty() {
            address = format!("{}@{address}", self.user);
        }
        if let Some(database) = &self.database {
            address = format!("{address}/{database}");
        }
        address
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "connection")]
    connections: Vec<Connection>,
}

/// The saved connections and the file they come from.
#[derive(Debug, Default)]
pub struct Store {
    path: Option<PathBuf>,
    pub connections: Vec<Connection>,
}

impl Store {
    /// Reads `connections.toml` from `dir`. A missing file is an empty list;
    /// a broken one is an empty list and a message.
    pub fn load(dir: Option<&Path>) -> (Self, Option<String>) {
        let path = dir.map(|dir| dir.join("connections.toml"));
        let mut store = Store {
            path: path.clone(),
            connections: Vec::new(),
        };
        let Some(path) = path else {
            return (store, None);
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<File>(&text) {
                Ok(file) => store.connections = file.connections,
                Err(err) => {
                    return (
                        store,
                        Some(format!("{}: {}", path.display(), err.message())),
                    )
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return (store, Some(format!("can't read {}: {err}", path.display()))),
        }
        (store, None)
    }

    pub fn get(&self, name: &str) -> Option<&Connection> {
        self.connections.iter().find(|c| c.name == name)
    }

    /// Adds `connection`, or replaces the one named `replacing`, and writes
    /// the file.
    pub fn save(&mut self, replacing: Option<&str>, connection: Connection) -> Result<(), String> {
        let clash = self
            .connections
            .iter()
            .any(|c| c.name == connection.name && Some(c.name.as_str()) != replacing);
        if clash {
            return Err(format!(
                "A connection named {:?} already exists",
                connection.name
            ));
        }
        match replacing.and_then(|name| self.connections.iter().position(|c| c.name == name)) {
            Some(index) => self.connections[index] = connection,
            None => self.connections.push(connection),
        }
        self.write()
    }

    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        self.connections.retain(|c| c.name != name);
        self.write()
    }

    /// Writes through a temporary file, so a crash can't leave half a file.
    fn write(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Err("No config directory to save connections in".to_string());
        };
        let text = toml::to_string(&File {
            connections: self.connections.clone(),
        })
        .map_err(|err| err.to_string())?;
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("toml.tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, path)
        };
        write().map_err(|err| format!("can't write {}: {err}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dfox-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn prod() -> Connection {
        Connection {
            host: "db.example.com".into(),
            user: "app".into(),
            database: Some("shop".into()),
            tag: Tag::Red,
            read_only: true,
            ..Connection::new("prod", DbType::Postgres)
        }
    }

    #[test]
    fn saved_connections_survive_a_restart_without_passwords() {
        let dir = temp_dir("store");
        let (mut store, problem) = Store::load(Some(&dir));
        assert!(problem.is_none());
        store.save(None, prod()).expect("save");
        store
            .save(None, Connection::new("local", DbType::Sqlite))
            .expect("save");

        let text = std::fs::read_to_string(dir.join("connections.toml")).expect("file");
        assert!(!text.contains("password"), "{text}");
        insta::assert_snapshot!(text);

        let (reloaded, _) = Store::load(Some(&dir));
        assert_eq!(reloaded.connections, store.connections);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_unique_and_edits_replace() {
        let dir = temp_dir("names");
        let (mut store, _) = Store::load(Some(&dir));
        store.save(None, prod()).expect("save");
        assert!(store
            .save(None, prod())
            .unwrap_err()
            .contains("already exists"));

        let renamed = Connection {
            name: "production".into(),
            ..prod()
        };
        store.save(Some("prod"), renamed).expect("rename");
        assert_eq!(store.connections.len(), 1);
        assert!(store.get("production").is_some());

        store.remove("production").expect("remove");
        assert!(store.connections.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_password_field_in_the_file_is_rejected() {
        let dir = temp_dir("password");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join("connections.toml"),
            "[[connection]]\nname = \"x\"\ndriver = \"postgres\"\npassword = \"hunter2\"\n",
        )
        .expect("write");
        let (store, problem) = Store::load(Some(&dir));
        assert!(store.connections.is_empty());
        assert!(problem
            .expect("problem")
            .contains("unknown field `password`"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn params_fill_in_defaults() {
        let mysql = Connection::new("m", DbType::MySql);
        let params = mysql.target();
        assert_eq!(
            (
                params.host.as_str(),
                params.port,
                params.database.as_deref()
            ),
            ("localhost", 3306, Some("mysql"))
        );
        assert_eq!(prod().address(), "app@db.example.com:5432/shop");
        let params = prod().params("secret");
        assert!(params.read_only);
        assert_eq!(params.password, "secret");
    }
}
