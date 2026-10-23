//! `~/.my.cnf`: the MySQL client's option file. Its `[client]` and `[mysql]`
//! sections name the user, password, host, port and database `mysql` uses
//! when none are given; dfox takes the password for MySQL connections the
//! same way, and offers the connection when the file names a host.

use std::path::PathBuf;

use dfox_core::models::connections::DbType;

use super::pgpass::Entry;

/// Where the `mysql` client looks for the user's file.
pub fn path() -> Option<PathBuf> {
    Some(directories::BaseDirs::new()?.home_dir().join(".my.cnf"))
}

pub fn load() -> Vec<Entry> {
    path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| parse(&text))
        .into_iter()
        .collect()
}

/// The entry the `[client]` and `[mysql]` sections make, a later setting
/// winning; `None` without a password. What isn't set matches any value.
pub fn parse(text: &str) -> Option<Entry> {
    let mut section = String::new();
    let mut entry = Entry {
        driver: DbType::MySql,
        host: "*".into(),
        port: "*".into(),
        database: "*".into(),
        user: "*".into(),
        password: String::new(),
    };
    let mut password = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(['#', ';', '!']) {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_lowercase();
            continue;
        }
        if section != "client" && section != "mysql" {
            continue;
        }
        // `password` alone asks for it: nothing to take.
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim().to_lowercase().replace('-', "_").as_str() {
            "user" => entry.user = value,
            "password" => password = Some(value),
            "host" => entry.host = value,
            "port" => entry.port = value,
            "database" => entry.database = value,
            _ => {}
        }
    }
    entry.password = password.filter(|p| !p.is_empty())?;
    Some(entry)
}

/// A value without the quotes around it.
fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner.to_string();
        }
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::{pgpass, Connection};

    const FILE: &str = "# mine\n\
        [mysqldump]\n\
        password = not-this\n\
        [client]\n\
        user = app\n\
        password = \"s3cret = yes\"\n\
        host=db.example.com\n\
        [mysql]\n\
        database = shop\n\
        !includedir /etc/mysql/conf.d/\n";

    #[test]
    fn the_client_sections_make_one_entry() {
        let entry = parse(FILE).expect("an entry");
        assert_eq!(entry.user, "app");
        assert_eq!(entry.password, "s3cret = yes");
        assert_eq!(entry.host, "db.example.com");
        assert_eq!(
            (entry.port.as_str(), entry.database.as_str()),
            ("*", "shop")
        );
        // No password, nothing to take.
        assert_eq!(parse("[client]\nuser = app\npassword\n"), None);
    }

    #[test]
    fn its_password_goes_to_mysql_connections_of_its_user() {
        let entries = vec![parse("[client]\nuser=app\npassword=pw\n").expect("entry")];
        let mysql = Connection {
            host: "anywhere".into(),
            user: "app".into(),
            ..Connection::new("my", DbType::MySql)
        };
        assert_eq!(pgpass::lookup(&entries, &mysql), Some("pw"));
        let other = Connection {
            user: "root".into(),
            ..mysql.clone()
        };
        assert_eq!(pgpass::lookup(&entries, &other), None);
        let postgres = Connection {
            driver: DbType::Postgres,
            ..mysql
        };
        assert_eq!(pgpass::lookup(&entries, &postgres), None);
        // A host makes it a connection to offer.
        let names: Vec<_> = pgpass::connections(&[parse(FILE).expect("entry")])
            .into_iter()
            .map(|c| (c.name, c.driver))
            .collect();
        assert_eq!(
            names,
            [("my.cnf: app@db.example.com/shop".to_string(), DbType::MySql)]
        );
    }
}
