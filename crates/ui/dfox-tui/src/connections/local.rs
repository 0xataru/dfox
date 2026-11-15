//! What the machine already says about its databases, read at the start:
//! the engines' own environment variables besides `DATABASE_URL`, SQLite
//! files in the project, and `~/.pg_service.conf`.

use std::path::{Path, PathBuf};

use dfox_core::models::connections::DbType;

use super::{url, Connection};
use crate::components::manager::Detected;

/// Variables holding a whole URL, as frameworks and hosts name them.
const URL_VARS: &[&str] = &[
    "POSTGRES_URL",
    "POSTGRESQL_URL",
    "PG_URL",
    "MYSQL_URL",
    "MONGODB_URI",
    "MONGODB_URL",
    "MONGO_URI",
    "MONGO_URL",
    "REDIS_URL",
    "VALKEY_URL",
    "CASSANDRA_URL",
];

/// Folders a project keeps its SQLite files in, besides its root:
/// Laravel's `database`, Rails' `db`, Prisma's `prisma`.
const SQLITE_DIRS: &[&str] = &["", "database", "db", "data", "prisma", "storage"];

/// Everything this module finds, for the project in `cwd`.
pub fn find(cwd: &Path, var: &dyn Fn(&str) -> Option<String>) -> Vec<Detected> {
    let mut found = environment(var);
    found.extend(sqlite_files(cwd));
    let services = var("PGSERVICEFILE").map(PathBuf::from).or_else(|| {
        Some(
            directories::BaseDirs::new()?
                .home_dir()
                .join(".pg_service.conf"),
        )
    });
    if let Some(text) = services.and_then(|path| std::fs::read_to_string(path).ok()) {
        found.extend(pg_services(&text));
    }
    found
}

/// The URL variables, and libpq's and MySQL's own (`PGHOST`, `MYSQL_HOST`…).
fn environment(var: &dyn Fn(&str) -> Option<String>) -> Vec<Detected> {
    let var = |name: &str| var(name).filter(|v| !v.trim().is_empty());
    let mut found = Vec::new();
    for name in URL_VARS {
        let Some(value) = var(name) else { continue };
        match url::parse(name, &value) {
            Ok((connection, password)) => found.push(Detected {
                connection,
                password,
                source: (*name).to_string(),
            }),
            Err(err) => log::info!("{name}: {err}"),
        }
    }
    // A socket directory as PGHOST isn't reachable over TCP.
    let pg = ["PGHOST", "PGPORT", "PGUSER", "PGDATABASE"]
        .iter()
        .any(|name| var(name).is_some())
        && !var("PGHOST").is_some_and(|h| h.starts_with('/'));
    if pg {
        let mut connection = Connection::new("PG variables", DbType::Postgres);
        connection.host = var("PGHOST").unwrap_or_else(|| "localhost".into());
        connection.port = var("PGPORT").and_then(|p| p.parse().ok());
        connection.user = var("PGUSER").unwrap_or_default();
        connection.database = var("PGDATABASE");
        found.push(Detected {
            connection,
            password: var("PGPASSWORD"),
            source: "PGHOST, PGUSER… variables".into(),
        });
    }
    if let Some(host) = var("MYSQL_HOST") {
        let mut connection = Connection::new("MYSQL_HOST", DbType::MySql);
        connection.host = host;
        connection.port = var("MYSQL_TCP_PORT").and_then(|p| p.parse().ok());
        found.push(Detected {
            connection,
            password: var("MYSQL_PWD"),
            source: "MYSQL_HOST variables".into(),
        });
    }
    found
}

/// SQLite files in the project's usual folders, told by their header, not
/// only their name.
fn sqlite_files(cwd: &Path) -> Vec<Detected> {
    let mut found = Vec::new();
    for dir in SQLITE_DIRS {
        let Ok(entries) = std::fs::read_dir(cwd.join(dir)) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| matches!(e.to_str(), Some("db" | "sqlite" | "sqlite3")))
            })
            .filter(|p| is_sqlite(p))
            .collect();
        files.sort();
        for path in files {
            let shown = path
                .strip_prefix(cwd)
                .unwrap_or(&path)
                .display()
                .to_string();
            let mut connection = Connection::new(&shown, DbType::Sqlite);
            connection.database = Some(path.display().to_string());
            found.push(Detected {
                connection,
                password: None,
                source: "a SQLite file in this folder".into(),
            });
        }
    }
    found
}

fn is_sqlite(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .is_ok_and(|()| &header == b"SQLite format 3\0")
}

/// `.pg_service.conf`: a `[name]` per service, then `key=value` lines.
fn pg_services(text: &str) -> Vec<Detected> {
    let mut found: Vec<Detected> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            found.push(Detected {
                connection: Connection::new(&format!("service {name}"), DbType::Postgres),
                password: None,
                source: ".pg_service.conf".into(),
            });
            continue;
        }
        let (Some(service), Some((key, value))) = (found.last_mut(), line.split_once('=')) else {
            continue;
        };
        let value = value.trim().to_string();
        let connection = &mut service.connection;
        match key.trim() {
            "host" => connection.host = value,
            "port" => connection.port = value.parse().ok(),
            "user" => connection.user = value,
            "dbname" => connection.database = Some(value),
            "password" => service.password = Some(value),
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn addresses(found: &[Detected]) -> Vec<(String, String, Option<String>)> {
        found
            .iter()
            .map(|d| {
                (
                    d.connection.name.clone(),
                    d.connection.address(),
                    d.password.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn engines_variables_name_their_databases() {
        let vars: HashMap<&str, &str> = HashMap::from([
            ("REDIS_URL", "redis://:pw@cache:6380"),
            ("MONGODB_URI", "mongodb://app@127.0.0.1:27017/shop"),
            ("MYSQL_URL", "not a url"),
            ("PGHOST", "db.local"),
            ("PGUSER", "app"),
            ("PGDATABASE", "shop"),
            ("PGPASSWORD", "secret"),
            ("MYSQL_HOST", "127.0.0.1"),
            ("MYSQL_TCP_PORT", "3307"),
        ]);
        let found = environment(&|name| vars.get(name).map(|v| v.to_string()));
        let row = |n: &str, a: &str, p: Option<&str>| {
            (n.to_string(), a.to_string(), p.map(str::to_string))
        };
        assert_eq!(
            addresses(&found),
            [
                row("MONGODB_URI", "app@127.0.0.1:27017/shop", None),
                row("REDIS_URL", "cache:6380", Some("pw")),
                row("PG variables", "app@db.local:5432/shop", Some("secret")),
                row("MYSQL_HOST", "127.0.0.1:3307", None),
            ]
        );
        // A socket directory is left out, as is nothing set at all.
        let socket = environment(&|name| (name == "PGHOST").then(|| "/tmp".to_string()));
        assert!(socket.is_empty());
        assert!(environment(&|_| None).is_empty());
    }

    #[test]
    fn sqlite_files_are_told_by_their_header() {
        let dir = std::env::temp_dir().join(format!("dfox-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("database")).expect("dir");
        let mut header = b"SQLite format 3\0".to_vec();
        header.extend([0u8; 84]);
        std::fs::write(dir.join("database/database.sqlite"), &header).expect("write");
        std::fs::write(dir.join("app.db"), &header).expect("write");
        std::fs::write(dir.join("notes.db"), b"not sqlite at all").expect("write");
        std::fs::write(dir.join("readme.md"), &header).expect("write");
        let found = sqlite_files(&dir);
        let names: Vec<&str> = found.iter().map(|d| d.connection.name.as_str()).collect();
        assert_eq!(names, ["app.db", "database/database.sqlite"]);
        assert_eq!(
            found[0].connection.database.as_deref(),
            Some(dir.join("app.db").to_str().expect("utf-8"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pg_services_become_connections() {
        let text = "# local\n[shop]\nhost=db.local\nport=5433\ndbname=shop\nuser=app\npassword=pw\n\n[bare]\ndbname=x\n";
        let row = |n: &str, a: &str, p: Option<&str>| {
            (n.to_string(), a.to_string(), p.map(str::to_string))
        };
        assert_eq!(
            addresses(&pg_services(text)),
            [
                row("service shop", "app@db.local:5433/shop", Some("pw")),
                row("service bare", "localhost:5432/x", None),
            ]
        );
    }
}
