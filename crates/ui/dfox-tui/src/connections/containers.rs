//! Databases running in local containers: `docker ps` and `docker inspect`,
//! or `podman`'s, which answer the same. A container counts when its image
//! is one of the engines dfox speaks and it publishes the engine's port; the
//! user, password and database come from the image's own environment
//! variables, as `POSTGRES_PASSWORD`.

use std::time::Duration;

use dfox_core::models::connections::DbType;
use serde::Deserialize;

use super::Connection;
use crate::components::manager::Detected;

/// How long the container CLI may take, all calls together.
const TIMEOUT: Duration = Duration::from_secs(3);

/// The running containers' databases, from the first of `docker` and
/// `podman` that answers. Nothing, and no error, without either.
pub async fn find() -> Vec<Detected> {
    for cli in ["docker", "podman"] {
        match tokio::time::timeout(TIMEOUT, inspect(cli)).await {
            Ok(Some(json)) => return parse(&json, cli),
            Ok(None) => continue,
            Err(_) => log::info!("{cli}: no answer in {TIMEOUT:?}"),
        }
    }
    Vec::new()
}

/// `inspect`'s JSON for the running containers, or None when `cli` isn't
/// there or its daemon doesn't answer.
async fn inspect(cli: &str) -> Option<String> {
    let run = |args: Vec<String>| async move {
        let out = tokio::process::Command::new(cli)
            .args(&args)
            .kill_on_drop(true)
            .output()
            .await
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let ids: Vec<String> = run(vec!["ps".into(), "-q".into()])
        .await?
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return Some("[]".into());
    }
    run([vec!["inspect".to_string()], ids].concat()).await
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Container {
    name: String,
    config: Config,
    network_settings: Network,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Config {
    image: String,
    #[serde(default)]
    env: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Network {
    #[serde(default)]
    ports: Option<std::collections::HashMap<String, Option<Vec<Binding>>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Binding {
    #[serde(default)]
    host_ip: String,
    host_port: String,
}

/// The engine an image runs, by its name without registry, owner and tag.
fn engine(image: &str) -> Option<DbType> {
    let name = image.rsplit('/').next()?.split([':', '@']).next()?;
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if has(&["postgres", "postgis", "timescale"]) {
        Some(DbType::Postgres)
    } else if has(&["mysql", "mariadb", "percona"]) {
        Some(DbType::MySql)
    } else if has(&["mongo"]) {
        Some(DbType::Mongo)
    } else if has(&["redis", "valkey", "keydb", "dragonfly"]) {
        Some(DbType::Redis)
    } else if has(&["cassandra", "scylla"]) {
        Some(DbType::Cassandra)
    } else {
        None
    }
}

/// The databases among `inspect`'s containers.
pub fn parse(json: &str, cli: &str) -> Vec<Detected> {
    let containers: Vec<Container> = match serde_json::from_str(json) {
        Ok(containers) => containers,
        Err(err) => {
            log::info!("{cli} inspect: {err}");
            return Vec::new();
        }
    };
    containers
        .into_iter()
        .filter_map(|c| detected(c, cli))
        .collect()
}

fn detected(container: Container, cli: &str) -> Option<Detected> {
    let driver = engine(&container.config.image)?;
    let inner = format!("{}/tcp", Connection::default_port(driver));
    // A port that isn't published can't be reached from here.
    let binding = container
        .network_settings
        .ports?
        .remove(&inner)??
        .into_iter()
        .next()?;
    let env = container.config.env.unwrap_or_default();
    let var = |name: &str| {
        env.iter()
            .find_map(|e| e.strip_prefix(name)?.strip_prefix('='))
            .map(str::to_string)
    };
    let name = container.name.trim_start_matches('/');
    let source = format!("{cli}: {}", container.config.image);
    database(
        name,
        &container.config.image,
        &var,
        &binding.host_ip,
        &binding.host_port,
        source,
    )
}

/// The database a container of `image` serves on the host's `host_port`,
/// with the user, password and database its environment sets.
pub fn database(
    name: &str,
    image: &str,
    env: &dyn Fn(&str) -> Option<String>,
    host_ip: &str,
    host_port: &str,
    source: String,
) -> Option<Detected> {
    let driver = engine(image)?;
    let var = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| env(name).filter(|v| !v.is_empty()))
    };
    let (user, password, database) = match driver {
        DbType::Postgres => {
            let user = var(&["POSTGRES_USER"]).unwrap_or_else(|| "postgres".into());
            let database = var(&["POSTGRES_DB"]).unwrap_or_else(|| user.clone());
            (user, var(&["POSTGRES_PASSWORD"]), Some(database))
        }
        DbType::MySql => {
            let database = var(&["MYSQL_DATABASE", "MARIADB_DATABASE"]);
            match var(&["MYSQL_USER", "MARIADB_USER"]) {
                Some(user) => (user, var(&["MYSQL_PASSWORD", "MARIADB_PASSWORD"]), database),
                None => (
                    "root".into(),
                    var(&["MYSQL_ROOT_PASSWORD", "MARIADB_ROOT_PASSWORD"]),
                    database,
                ),
            }
        }
        DbType::Mongo => (
            var(&["MONGO_INITDB_ROOT_USERNAME"]).unwrap_or_default(),
            var(&["MONGO_INITDB_ROOT_PASSWORD"]),
            var(&["MONGO_INITDB_DATABASE"]),
        ),
        DbType::Redis => (String::new(), var(&["REDIS_PASSWORD"]), None),
        _ => (String::new(), None, None),
    };
    let host = match host_ip {
        "" | "0.0.0.0" | "::" => "127.0.0.1".to_string(),
        ip => ip.to_string(),
    };
    let mut connection = Connection::new(name, driver);
    connection.host = host;
    connection.port = host_port.parse().ok();
    connection.user = user;
    connection.database = database;
    Some(Detected {
        connection,
        password,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSPECT: &str = r#"[
      {
        "Name": "/shop-db-1",
        "Config": {
          "Image": "postgres:16",
          "Env": ["POSTGRES_PASSWORD=secret", "POSTGRES_DB=shop", "PATH=/usr/bin"]
        },
        "NetworkSettings": {
          "Ports": { "5432/tcp": [{ "HostIp": "0.0.0.0", "HostPort": "15432" }] }
        }
      },
      {
        "Name": "/maria",
        "Config": {
          "Image": "docker.io/library/mariadb:11",
          "Env": ["MARIADB_ROOT_PASSWORD=root"]
        },
        "NetworkSettings": {
          "Ports": { "3306/tcp": [{ "HostIp": "127.0.0.1", "HostPort": "3307" }] }
        }
      },
      {
        "Name": "/cache",
        "Config": { "Image": "valkey/valkey:8", "Env": null },
        "NetworkSettings": { "Ports": { "6379/tcp": [{ "HostIp": "", "HostPort": "6380" }] } }
      },
      {
        "Name": "/hidden",
        "Config": { "Image": "mongo:7", "Env": [] },
        "NetworkSettings": { "Ports": { "27017/tcp": null } }
      },
      {
        "Name": "/web",
        "Config": { "Image": "nginx:latest", "Env": [] },
        "NetworkSettings": { "Ports": { "80/tcp": [{ "HostIp": "0.0.0.0", "HostPort": "8080" }] } }
      }
    ]"#;

    #[test]
    fn databases_come_with_their_port_and_environment() {
        let found = parse(INSPECT, "docker");
        let shown: Vec<(String, String, Option<String>, String)> = found
            .iter()
            .map(|d| {
                (
                    d.connection.name.clone(),
                    d.connection.address(),
                    d.password.clone(),
                    d.source.clone(),
                )
            })
            .collect();
        let row = |name: &str, address: &str, password: Option<&str>, source: &str| {
            (
                name.to_string(),
                address.to_string(),
                password.map(str::to_string),
                source.to_string(),
            )
        };
        assert_eq!(
            shown,
            [
                row(
                    "shop-db-1",
                    "postgres@127.0.0.1:15432/shop",
                    Some("secret"),
                    "docker: postgres:16"
                ),
                row(
                    "maria",
                    "root@127.0.0.1:3307",
                    Some("root"),
                    "docker: docker.io/library/mariadb:11"
                ),
                row("cache", "127.0.0.1:6380", None, "docker: valkey/valkey:8"),
            ]
        );
        assert_eq!(found[1].connection.driver, DbType::MySql);
        assert_eq!(found[2].connection.driver, DbType::Redis);
    }

    #[test]
    fn images_name_their_engine() {
        assert_eq!(engine("bitnami/postgresql:16"), Some(DbType::Postgres));
        assert_eq!(
            engine("timescale/timescaledb-ha:pg16"),
            Some(DbType::Postgres)
        );
        assert_eq!(engine("scylladb/scylla:6.2"), Some(DbType::Cassandra));
        assert_eq!(engine("mongo@sha256:abc"), Some(DbType::Mongo));
        assert_eq!(engine("nginx"), None);
    }

    #[test]
    fn odd_output_finds_nothing() {
        assert!(parse("not json", "docker").is_empty());
        assert!(parse("[]", "podman").is_empty());
    }
}
