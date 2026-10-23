//! `~/.pgpass` (or `$PGPASSFILE`): Postgres passwords by host, port, database
//! and user, as libpq reads them. Entries from `~/.my.cnf` (see `mycnf`)
//! join them for MySQL.

use std::path::PathBuf;

use dfox_core::models::connections::DbType;

use super::Connection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The database it is for: Postgres from `.pgpass`, MySQL from `.my.cnf`.
    pub driver: DbType,
    pub host: String,
    pub port: String,
    pub database: String,
    pub user: String,
    pub password: String,
}

/// Where libpq looks for the file.
pub fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PGPASSFILE") {
        return Some(PathBuf::from(path));
    }
    let dirs = directories::BaseDirs::new()?;
    Some(if cfg!(windows) {
        dirs.config_dir().join("postgresql").join("pgpass.conf")
    } else {
        dirs.home_dir().join(".pgpass")
    })
}

pub fn load() -> Vec<Entry> {
    path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// Lines of `host:port:database:user:password`, where `\:` and `\\` escape.
pub fn parse(text: &str) -> Vec<Entry> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let mut fields = Vec::new();
            let mut field = String::new();
            let mut chars = line.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => field.extend(chars.next()),
                    ':' if fields.len() < 4 => fields.push(std::mem::take(&mut field)),
                    c => field.push(c),
                }
            }
            fields.push(field);
            let [host, port, database, user, password]: [String; 5] = fields.try_into().ok()?;
            Some(Entry {
                driver: DbType::Postgres,
                host,
                port,
                database,
                user,
                password,
            })
        })
        .collect()
}

/// The password for a connection: the first matching entry for its
/// database, where `*` matches anything.
pub fn lookup<'a>(entries: &'a [Entry], connection: &Connection) -> Option<&'a str> {
    let params = connection.target();
    let port = params.port.to_string();
    let database = params.database.unwrap_or_default();
    let matches = |pattern: &str, value: &str| pattern == "*" || pattern == value;
    entries
        .iter()
        .find(|e| {
            e.driver == connection.driver
                && matches(&e.host, &params.host)
                && matches(&e.port, &port)
                && matches(&e.database, &database)
                && matches(&e.user, &params.username)
        })
        .map(|e| e.password.as_str())
}

/// A connection for each line that names a host, to offer in the manager.
pub fn connections(entries: &[Entry]) -> Vec<Connection> {
    entries
        .iter()
        .filter(|e| e.host != "*" && e.user != "*")
        .map(|e| {
            let database = (e.database != "*").then(|| e.database.clone());
            let file = if e.driver == DbType::MySql {
                "my.cnf"
            } else {
                "pgpass"
            };
            let name = format!(
                "{file}: {}@{}{}",
                e.user,
                e.host,
                database
                    .as_ref()
                    .map(|d| format!("/{d}"))
                    .unwrap_or_default()
            );
            Connection {
                host: e.host.clone(),
                port: e.port.parse().ok(),
                database,
                user: e.user.clone(),
                ..Connection::new(&name, e.driver)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# comment\n\
        db.example.com:5432:shop:app:s3cr\\:et\n\
        *:*:*:admin:any\n\
        localhost:*:*:me:pw\\\\x\n\
        broken:line\n";

    #[test]
    fn lines_parse_with_escapes() {
        let entries = parse(FILE);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].password, "s3cr:et");
        assert_eq!(entries[2].password, "pw\\x");
    }

    #[test]
    fn lookup_matches_like_libpq() {
        let entries = parse(FILE);
        let shop = Connection {
            host: "db.example.com".into(),
            user: "app".into(),
            database: Some("shop".into()),
            ..Connection::new("shop", DbType::Postgres)
        };
        assert_eq!(lookup(&entries, &shop), Some("s3cr:et"));

        let admin = Connection {
            user: "admin".into(),
            ..shop.clone()
        };
        assert_eq!(lookup(&entries, &admin), Some("any"));

        let mysql = Connection {
            driver: DbType::MySql,
            ..shop
        };
        assert_eq!(lookup(&entries, &mysql), None);
    }

    #[test]
    fn named_hosts_become_connections() {
        let names: Vec<_> = connections(&parse(FILE))
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            ["pgpass: app@db.example.com/shop", "pgpass: me@localhost"]
        );
    }
}
