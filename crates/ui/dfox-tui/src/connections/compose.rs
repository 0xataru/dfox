//! The databases a project's Docker Compose file defines, running or not:
//! `docker compose config` reads the file as Compose does, with its
//! `.env` and overrides, so no YAML is read here.

use std::{collections::HashMap, path::Path, time::Duration};

use serde::Deserialize;

use super::containers;
use crate::components::manager::Detected;

const TIMEOUT: Duration = Duration::from_secs(3);

/// The databases of the Compose project in `dir`, or nothing without one
/// or without `docker`.
pub async fn find(dir: &Path) -> Vec<Detected> {
    let run = tokio::process::Command::new("docker")
        .args(["compose", "config", "--format", "json"])
        .current_dir(dir)
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(TIMEOUT, run).await {
        Ok(Ok(out)) if out.status.success() => parse(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

#[derive(Deserialize)]
struct Project {
    #[serde(default)]
    services: HashMap<String, Service>,
}

#[derive(Deserialize)]
struct Service {
    #[serde(default)]
    image: String,
    #[serde(default)]
    ports: Vec<Port>,
    #[serde(default)]
    environment: Option<HashMap<String, Option<String>>>,
}

#[derive(Deserialize)]
struct Port {
    target: u16,
    #[serde(default)]
    published: Option<serde_json::Value>,
    #[serde(default)]
    host_ip: Option<String>,
}

pub fn parse(json: &str) -> Vec<Detected> {
    let project: Project = match serde_json::from_str(json) {
        Ok(project) => project,
        Err(err) => {
            log::info!("docker compose config: {err}");
            return Vec::new();
        }
    };
    let mut services: Vec<(String, Service)> = project.services.into_iter().collect();
    services.sort_by(|a, b| a.0.cmp(&b.0));
    services
        .into_iter()
        .filter_map(|(name, service)| {
            let env = service.environment.unwrap_or_default();
            let var = |key: &str| env.get(key).cloned().flatten();
            // The engine's own port, published on the host.
            let found = service.ports.iter().find_map(|port| {
                let published = match port.published.as_ref()? {
                    serde_json::Value::String(p) => p.clone(),
                    other => other.to_string(),
                };
                let detected = containers::database(
                    &name,
                    &service.image,
                    &var,
                    port.host_ip.as_deref().unwrap_or_default(),
                    &published,
                    format!("docker compose: {}", service.image),
                )?;
                let inner = super::Connection::default_port(detected.connection.driver);
                (port.target == inner).then_some(detected)
            });
            found
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_with_a_published_engine_port_count() {
        let json = r#"{
          "name": "shop",
          "services": {
            "db": {
              "image": "postgres:16",
              "ports": [{ "target": 5432, "published": "15432", "protocol": "tcp" }],
              "environment": { "POSTGRES_PASSWORD": "dfox", "POSTGRES_DB": "shop" }
            },
            "cache": {
              "image": "redis:7",
              "ports": [{ "target": 6379, "published": 6380, "host_ip": "127.0.0.1" }]
            },
            "inner": { "image": "mysql:8", "environment": { "MYSQL_ROOT_PASSWORD": "x" } },
            "web": { "image": "nginx", "ports": [{ "target": 80, "published": "8080" }] }
          }
        }"#;
        let found: Vec<(String, String, Option<String>)> = parse(json)
            .into_iter()
            .map(|d| (d.connection.address(), d.connection.name, d.password))
            .collect();
        assert_eq!(
            found,
            [
                ("127.0.0.1:6380".into(), "cache".into(), None),
                (
                    "postgres@127.0.0.1:15432/shop".into(),
                    "db".into(),
                    Some("dfox".into())
                ),
            ]
        );
        assert!(parse("no").is_empty());
    }
}
