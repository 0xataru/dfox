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
    for (key, value) in url.query_pairs() {
        if matches!(key.as_ref(), "sslmode" | "ssl-mode" | "ssl_mode") {
            connection.tls = match value.to_lowercase().as_str() {
                "disable" | "disabled" => TlsMode::Disable,
                "require" | "required" | "verify-ca" | "verify-full" | "verify_ca"
                | "verify_identity" => TlsMode::Require,
                _ => TlsMode::Prefer,
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
        assert!(parse("x", "redis://localhost")
            .unwrap_err()
            .contains("unsupported"));
        assert!(parse("x", "hello")
            .unwrap_err()
            .contains("not a database URL"));
    }
}
