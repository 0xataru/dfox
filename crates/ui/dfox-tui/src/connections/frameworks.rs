//! The databases a project's framework config names, in the folder dfox
//! starts in: Rails' `config/database.yml`, Spring's `application.properties`
//! and `application.yml` (JDBC URLs), Prisma's `schema.prisma` and
//! `alembic.ini`. Values taken from the environment are read as the
//! environment has them, or as the file's default.

use std::{collections::BTreeMap, path::Path};

use dfox_core::models::connections::DbType;

use super::{engines, url, Connection};
use crate::components::manager::Detected;

pub fn find(dir: &Path, var: &dyn Fn(&str) -> Option<String>) -> Vec<Detected> {
    let read = |file: &str| std::fs::read_to_string(dir.join(file)).ok();
    let mut found = Vec::new();
    if let Some(text) = read("config/database.yml") {
        found.extend(rails(&text, dir, var));
    }
    for file in [
        "application.properties",
        "src/main/resources/application.properties",
        "application.yml",
        "src/main/resources/application.yml",
        "application.yaml",
        "src/main/resources/application.yaml",
    ] {
        if let Some(text) = read(file) {
            let values = if file.ends_with(".properties") {
                properties(&text)
            } else {
                yaml(&text)
            };
            found.extend(spring(&values, file, var));
        }
    }
    if let Some(text) = read("prisma/schema.prisma").or_else(|| read("schema.prisma")) {
        found.extend(prisma(&text));
    }
    if let Some(text) = read("alembic.ini") {
        found.extend(alembic(&text));
    }
    found
}

/// A config value with `${NAME:default}` (Spring) or `<%= ENV.fetch("NAME")
/// { "default" } %>` (Rails) read from the environment, or its default.
/// None when it can't be known.
fn resolve(value: &str, var: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
    if let Some(inner) = value.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
        let (name, default) = inner.split_once(':').unwrap_or((inner, ""));
        return var(name).or_else(|| (!default.is_empty()).then(|| default.to_string()));
    }
    if value.contains("<%") {
        let name = value
            .split(['"', '\''])
            .nth(1)
            .filter(|_| value.contains("ENV"));
        let default = value
            .split_once('{')
            .and_then(|(_, rest)| rest.split(['"', '\'']).nth(1));
        return name.and_then(var).or_else(|| default.map(str::to_string));
    }
    (!value.is_empty()).then(|| value.to_string())
}

/// Simple YAML as `a.b.c` paths to values: the nesting by indentation,
/// `&anchor` names, and `<<: *anchor` merges, which is all database
/// configs use.
fn yaml(text: &str) -> BTreeMap<String, String> {
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    let mut anchors: BTreeMap<String, String> = BTreeMap::new();
    let mut merges: Vec<(String, String)> = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    for line in text.lines() {
        let content = line.trim_start();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        let indent = line.len() - content.len();
        let Some((key, value)) = content.split_once(':') else {
            continue;
        };
        while path.last().is_some_and(|(i, _)| *i >= indent) {
            path.pop();
        }
        let parent: Vec<&str> = path.iter().map(|(_, k)| k.as_str()).collect();
        let parent = parent.join(".");
        let value = value.trim();
        let value = match value.split_once(" #") {
            Some((value, _)) => value.trim(),
            None => value,
        };
        if key.trim() == "<<" {
            merges.push((parent, value.trim_start_matches('*').to_string()));
            continue;
        }
        let key = key
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string();
        let full = if parent.is_empty() {
            key.clone()
        } else {
            format!("{parent}.{key}")
        };
        match value.strip_prefix('&') {
            Some(anchor) => {
                anchors.insert(anchor.to_string(), full);
                path.push((indent, key));
            }
            None if value.is_empty() => path.push((indent, key)),
            None => {
                values.insert(full, value.to_string());
            }
        }
    }
    for (into, anchor) in merges {
        let Some(from) = anchors.get(&anchor) else {
            continue;
        };
        let copied: Vec<(String, String)> = values
            .iter()
            .filter_map(|(k, v)| {
                let rest = k.strip_prefix(&format!("{from}."))?;
                Some((format!("{into}.{rest}"), v.clone()))
            })
            .collect();
        for (k, v) in copied {
            values.entry(k).or_insert(v);
        }
    }
    values
}

/// `key=value` and `key: value` lines.
fn properties(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#') && !line.starts_with('!'))
        .filter_map(|line| {
            let (key, value) = line.split_once(['=', ':'])?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

/// Rails' development database, the one used day to day.
fn rails(text: &str, dir: &Path, var: &dyn Fn(&str) -> Option<String>) -> Option<Detected> {
    let values = yaml(text);
    let get = |key: &str| {
        values
            .get(&format!("development.{key}"))
            .and_then(|v| resolve(v, var))
    };
    let source = "config/database.yml (development)".to_string();
    if let Some(found) = get("url") {
        let (connection, password) = url::parse("rails development", &found).ok()?;
        return Some(Detected {
            connection,
            password,
            source,
        });
    }
    let adapter = get("adapter")?;
    let driver = engines::find(|e| e.adapters, |w| w == adapter)?;
    let mut connection = Connection::new("rails development", driver);
    if driver == DbType::Sqlite {
        connection.database = Some(dir.join(get("database")?).display().to_string());
    } else {
        connection.host = get("host").unwrap_or_else(|| "localhost".into());
        connection.port = get("port").and_then(|p| p.parse().ok());
        connection.user = get("username").unwrap_or_default();
        connection.database = get("database");
    }
    Some(Detected {
        connection,
        password: get("password"),
        source,
    })
}

/// Spring's `spring.datasource`: a JDBC URL, with the user and password
/// beside it.
fn spring(
    values: &BTreeMap<String, String>,
    file: &str,
    var: &dyn Fn(&str) -> Option<String>,
) -> Option<Detected> {
    let get = |key: &str| {
        values
            .get(&format!("spring.datasource.{key}"))
            .and_then(|v| resolve(v, var))
    };
    let jdbc = get("url")?;
    let (mut connection, password) =
        url::parse("spring datasource", jdbc.strip_prefix("jdbc:")?).ok()?;
    if let Some(user) = get("username") {
        connection.user = user;
    }
    Some(Detected {
        connection,
        password: get("password").or(password),
        source: format!("{file} (spring.datasource)"),
    })
}

/// A Prisma datasource whose `url` is written out; `env("…")` is found with
/// the environment already.
fn prisma(text: &str) -> Option<Detected> {
    let url_line = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("url") && line.contains('"') && !line.contains("env("))?;
    let found = url_line.split('"').nth(1)?;
    let (connection, password) = url::parse("prisma datasource", found).ok()?;
    Some(Detected {
        connection,
        password,
        source: "schema.prisma".into(),
    })
}

/// `sqlalchemy.url`, its `+driver` taken off the scheme.
fn alembic(text: &str) -> Option<Detected> {
    let found = properties(text).remove("sqlalchemy.url")?;
    let (scheme, rest) = found.split_once("://")?;
    let scheme = scheme.split('+').next()?;
    let (connection, password) = url::parse("alembic", &format!("{scheme}://{rest}")).ok()?;
    Some(Detected {
        connection,
        password,
        source: "alembic.ini".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(found: Option<Detected>) -> (String, Option<String>) {
        let found = found.expect("found");
        (found.connection.address(), found.password)
    }

    #[test]
    fn rails_development_with_its_default_and_erb() {
        let text = r#"
default: &default
  adapter: postgresql
  encoding: unicode
  host: <%= ENV.fetch("DB_HOST") { "localhost" } %>
  username: app

development:
  <<: *default
  database: shop_development # day to day
  password: <%= ENV["DB_PASSWORD"] %>

test:
  <<: *default
  database: shop_test
"#;
        let none = |_: &str| None;
        assert_eq!(
            shown(rails(text, Path::new("/p"), &none)),
            ("app@localhost:5432/shop_development".into(), None)
        );
        let set = |name: &str| match name {
            "DB_HOST" => Some("db.local".to_string()),
            "DB_PASSWORD" => Some("pw".to_string()),
            _ => None,
        };
        assert_eq!(
            shown(rails(text, Path::new("/p"), &set)),
            (
                "app@db.local:5432/shop_development".into(),
                Some("pw".into())
            )
        );
        let sqlite = "development:\n  adapter: sqlite3\n  database: storage/development.sqlite3\n";
        assert_eq!(
            rails(sqlite, Path::new("/p"), &none).and_then(|d| d.connection.database),
            Some(
                Path::new("/p")
                    .join("storage/development.sqlite3")
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn spring_jdbc_urls_from_properties_and_yaml() {
        let none = |_: &str| None;
        let text = "spring.datasource.url=jdbc:postgresql://db:5433/shop\nspring.datasource.username=${DB_USER:app}\nspring.datasource.password=secret\n";
        assert_eq!(
            shown(spring(&properties(text), "application.properties", &none)),
            ("app@db:5433/shop".into(), Some("secret".into()))
        );
        let text = "spring:\n  datasource:\n    url: jdbc:mysql://127.0.0.1:3306/shop\n    username: root\n  jpa:\n    show-sql: true\n";
        assert_eq!(
            shown(spring(&yaml(text), "application.yml", &none)),
            ("root@127.0.0.1:3306/shop".into(), None)
        );
        // An unknown variable without a default: nothing to show.
        let text = "spring.datasource.url=${JDBC_URL}\n";
        assert!(spring(&properties(text), "application.properties", &none).is_none());
    }

    #[test]
    fn prisma_and_alembic_urls() {
        let schema = "datasource db {\n  provider = \"postgresql\"\n  url      = \"postgresql://app:pw@localhost:5432/shop\"\n}\n";
        assert_eq!(
            shown(prisma(schema)),
            ("app@localhost:5432/shop".into(), Some("pw".into()))
        );
        assert!(prisma("datasource db {\n  url = env(\"DATABASE_URL\")\n}").is_none());
        let ini = "[alembic]\nscript_location = migrations\nsqlalchemy.url = postgresql+psycopg2://app@db/shop\n";
        assert_eq!(shown(alembic(ini)), ("app@db:5432/shop".into(), None));
    }
}
