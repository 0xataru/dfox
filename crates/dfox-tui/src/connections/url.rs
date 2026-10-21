//! Connection URLs: `postgres://`, `mysql://`, `sqlite:` or a path to a
//! SQLite file. Used for `DATABASE_URL` and `dfox <url>`.

use dfox_core::models::connections::{DbType, TlsMode};
use percent_encoding::percent_decode_str;

use super::Connection;

/// The connection a URL describes, and its password if it has one.
pub fn parse(name: &str, text: &str) -> Result<(Connection, Option<String>), String> {
    let text = text.trim();
    if let Some(path) = text
        .strip_prefix("sqlite://")
        .or_else(|| text.strip_prefix("sqlite:"))
    {
        return Ok((sqlite(name, path), None));
    }
    if !text.contains("://") {
        let is_file = [".db", ".sqlite", ".sqlite3"]
            .iter()
            .any(|ext| text.ends_with(ext));
        return if is_file {
            Ok((sqlite(name, text), None))
        } else {
            Err(format!("{text:?} is not a database URL or a SQLite file"))
        };
    }

    let url = url::Url::parse(text).map_err(|err| format!("{text:?}: {err}"))?;
    let driver = match url.scheme() {
        "postgres" | "postgresql" => DbType::Postgres,
        "mysql" | "mariadb" => DbType::MySql,
        "mongodb" => DbType::Mongo,
        "cassandra" | "scylla" | "scylladb" => DbType::Cassandra,
        "redis" | "rediss" | "valkey" | "valkeys" => DbType::Redis,
        "mongodb+srv" => {
            return Err(
                "mongodb+srv addresses aren't supported yet: use mongodb://host:port".into(),
            )
        }
        scheme => return Err(format!("unsupported database {scheme:?}")),
    };
    let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
    let database = url.path().trim_start_matches('/');
    let mut connection = Connection {
        host: url.host_str().unwrap_or_default().to_string(),
        port: url.port(),
        database: (!database.is_empty()).then(|| decode(database)),
        user: decode(url.username()),
        ..Connection::new(name, driver)
    };
    // `rediss://` and `valkeys://` are Redis over TLS.
    if matches!(url.scheme(), "rediss" | "valkeys") {
        connection.tls = TlsMode::VerifyFull;
    }
    for (key, value) in url.query_pairs() {
        if matches!(key.as_ref(), "sslmode" | "ssl-mode" | "ssl_mode") {
            connection.tls = match value.to_lowercase().as_str() {
                "disable" | "disabled" => TlsMode::Disable,
                "require" | "required" | "verify-ca" | "verify_ca" => TlsMode::Require,
                "verify-full" | "verify_identity" => TlsMode::VerifyFull,
                _ => TlsMode::Prefer,
            };
        }
        // MongoDB's own switch.
        if matches!(key.as_ref(), "tls" | "ssl") {
            connection.tls = match value.as_ref() {
                "true" => TlsMode::VerifyFull,
                _ => TlsMode::Disable,
            };
        }
    }
    Ok((connection, url.password().map(decode)))
}

fn sqlite(name: &str, path: &str) -> Connection {
    Connection {
        database: Some(path.to_string()),
        ..Connection::new(name, DbType::Sqlite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls() {
        let (c, password) = parse(
            "env",
            "postgresql://app:p%40ss@db.example.com:6543/shop?sslmode=require",
        )
        .expect("parse");
        assert_eq!(c.driver, DbType::Postgres);
        assert_eq!(c.address(), "app@db.example.com:6543/shop");
        assert_eq!(c.tls, TlsMode::Require);
        assert_eq!(password.as_deref(), Some("p@ss"));

        let (c, password) = parse("env", "mysql://root@localhost").expect("parse");
        assert_eq!((c.driver, c.port, c.database), (DbType::MySql, None, None));
        assert_eq!(password, None);
    }

    #[test]
    fn redis_urls() {
        let (c, password) = parse("env", "redis://:secret@cache.local:6380/2").expect("parse");
        assert_eq!(c.driver, DbType::Redis);
        assert_eq!(c.address(), "cache.local:6380/2");
        assert_eq!(c.tls, TlsMode::Prefer);
        assert_eq!(password.as_deref(), Some("secret"));
        let (c, _) = parse("env", "rediss://default@redis.example.com").expect("parse");
        assert_eq!(c.tls, TlsMode::VerifyFull);
        assert_eq!(
            (c.target().port, c.target().database.as_deref()),
            (6379, Some("0"))
        );
        assert_eq!(
            parse("env", "valkey://localhost").expect("parse").0.driver,
            DbType::Redis
        );
    }

    #[test]
    fn mongodb_urls() {
        let (c, password) =
            parse("env", "mongodb://root:dfox@127.0.0.1:57017/shop?tls=true").expect("parse");
        assert_eq!(c.driver, DbType::Mongo);
        assert_eq!(c.address(), "root@127.0.0.1:57017/shop");
        assert_eq!(c.tls, TlsMode::VerifyFull);
        assert_eq!(password.as_deref(), Some("dfox"));
        // Without a database, mongosh's.
        let (c, _) = parse("env", "mongodb://localhost").expect("parse");
        assert_eq!(c.target().database.as_deref(), Some("test"));
        assert_eq!(c.target().port, 27017);
        assert!(parse("x", "mongodb+srv://cluster.example.net")
            .unwrap_err()
            .contains("aren't supported yet"));
    }

    #[test]
    fn cassandra_urls() {
        let (c, password) =
            parse("env", "cassandra://cassandra:secret@10.0.0.5/shop").expect("parse");
        assert_eq!(c.driver, DbType::Cassandra);
        assert_eq!(c.target().port, 9042);
        assert_eq!(password.as_deref(), Some("secret"));
        let (c, _) = parse("env", "scylla://localhost").expect("parse");
        assert_eq!(c.target().database.as_deref(), Some("system"));
    }

    #[test]
    fn sqlite_urls_and_paths() {
        for text in ["sqlite:///tmp/a.db", "sqlite:/tmp/a.db", "/tmp/a.db"] {
            let (c, _) = parse("x", text).expect(text);
            assert_eq!(c.driver, DbType::Sqlite, "{text}");
            assert!(c
                .database
                .as_deref()
                .unwrap_or_default()
                .ends_with("tmp/a.db"));
        }
        assert_eq!(
            parse("x", "./data.sqlite3").expect("relative").0.address(),
            "./data.sqlite3"
        );
    }

    #[test]
    fn other_text_is_explained() {
        assert!(parse("x", "oracle://localhost")
            .unwrap_err()
            .contains("unsupported"));
        assert!(parse("x", "hello")
            .unwrap_err()
            .contains("not a database URL"));
    }
}
