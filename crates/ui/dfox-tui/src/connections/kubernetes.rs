//! Databases in the Kubernetes cluster of the current kubeconfig context:
//! its services whose port is a database's, ready to connect to through
//! `kubectl port-forward`. Read-only: `kubectl get services`, in every
//! namespace the user may list, else in the context's own.

use std::{collections::BTreeMap, time::Duration};

use dfox_core::models::connections::{DbType, KubeParams};
use serde::Deserialize;

use super::{Connection, Tag};
use crate::components::manager::Detected;

/// Logging in to a cloud cluster can take a few seconds.
const TIMEOUT: Duration = Duration::from_secs(10);

pub async fn find() -> Vec<Detected> {
    let Some(context) = kubectl(&["config", "current-context"]).await else {
        return Vec::new();
    };
    let context = context.trim().to_string();
    let all = kubectl(&["get", "services", "--all-namespaces", "-o", "json"]).await;
    // Without the right to list every namespace, the context's own.
    let json = match all {
        Some(json) => json,
        None => match kubectl(&["get", "services", "-o", "json"]).await {
            Some(json) => json,
            None => return Vec::new(),
        },
    };
    parse(&json, &context)
}

async fn kubectl(args: &[&str]) -> Option<String> {
    let run = tokio::process::Command::new("kubectl")
        .args(args)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(TIMEOUT, run).await.ok()?.ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Deserialize)]
struct List {
    #[serde(default)]
    items: Vec<Service>,
}

#[derive(Deserialize)]
struct Service {
    metadata: Metadata,
    spec: Spec,
}

#[derive(Deserialize)]
struct Metadata {
    name: String,
    #[serde(default)]
    namespace: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Spec {
    #[serde(default, rename = "clusterIP")]
    cluster_ip: Option<String>,
    #[serde(default)]
    ports: Vec<Port>,
}

#[derive(Deserialize)]
struct Port {
    port: u16,
    #[serde(default)]
    name: Option<String>,
}

/// The engine a service port speaks, by its name or else its number.
fn engine(port: &Port) -> Option<DbType> {
    let name = port.name.as_deref().unwrap_or_default().to_lowercase();
    let named = [
        (DbType::Postgres, ["postgres", "pg"].as_slice()),
        (DbType::MySql, &["mysql", "mariadb"]),
        (DbType::Mongo, &["mongo"]),
        (DbType::Redis, &["redis", "valkey"]),
        (DbType::Cassandra, &["cql", "cassandra", "scylla"]),
    ];
    if let Some((driver, _)) = named
        .iter()
        .find(|(_, words)| words.iter().any(|w| name.contains(w)))
    {
        return Some(*driver);
    }
    [
        DbType::Postgres,
        DbType::MySql,
        DbType::Mongo,
        DbType::Redis,
        DbType::Cassandra,
    ]
    .into_iter()
    .find(|d| Connection::default_port(*d) == port.port)
}

pub fn parse(json: &str, context: &str) -> Vec<Detected> {
    let list: List = match serde_json::from_str(json) {
        Ok(list) => list,
        Err(err) => {
            log::info!("kubectl get services: {err}");
            return Vec::new();
        }
    };
    let names: BTreeMap<(String, String), ()> = list
        .items
        .iter()
        .map(|s| ((s.metadata.namespace.clone(), s.metadata.name.clone()), ()))
        .collect();
    let has = |namespace: &str, name: &str| {
        names.contains_key(&(namespace.to_string(), name.to_string()))
    };
    let prod = context.to_lowercase().contains("prod");
    list.items
        .into_iter()
        .filter(|s| {
            // A headless service reaches one pod at a time, and CloudNativePG's
            // read-only ones sit next to the writable `-rw`.
            let headless = s.spec.cluster_ip.as_deref() == Some("None");
            let name = &s.metadata.name;
            let read_only = ["-ro", "-r"].iter().any(|suffix| {
                name.strip_suffix(suffix)
                    .is_some_and(|base| has(&s.metadata.namespace, &format!("{base}-rw")))
            });
            !headless && !read_only
        })
        .filter_map(|s| {
            let (driver, port) = s
                .spec
                .ports
                .iter()
                .find_map(|p| Some((engine(p)?, p.port)))?;
            let namespace = if s.metadata.namespace.is_empty() {
                "default".to_string()
            } else {
                s.metadata.namespace
            };
            let mut connection =
                Connection::new(&format!("{} ({namespace})", s.metadata.name), driver);
            connection.port = Some(port);
            connection.kube = Some(KubeParams {
                context: Some(context.to_string()),
                namespace,
                service: s.metadata.name,
            });
            if prod {
                connection.tag = Tag::Red;
            }
            Some(Detected {
                connection,
                password: None,
                source: format!("kubernetes: {context}"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `kubectl get services -A -o json`, cut down to what is read.
    const SERVICES: &str = r#"{
      "items": [
        { "metadata": { "name": "stage-postgresql", "namespace": "stage" },
          "spec": { "clusterIP": "10.0.0.5", "ports": [{ "name": "tcp-postgresql", "port": 5432 }] } },
        { "metadata": { "name": "stage-postgresql-hl", "namespace": "stage" },
          "spec": { "clusterIP": "None", "ports": [{ "name": "tcp-postgresql", "port": 5432 }] } },
        { "metadata": { "name": "crm-rw", "namespace": "crm" },
          "spec": { "clusterIP": "10.0.0.6", "ports": [{ "name": "postgres", "port": 5432 }] } },
        { "metadata": { "name": "crm-ro", "namespace": "crm" },
          "spec": { "clusterIP": "10.0.0.7", "ports": [{ "name": "postgres", "port": 5432 }] } },
        { "metadata": { "name": "cache", "namespace": "stage" },
          "spec": { "clusterIP": "10.0.0.8", "ports": [{ "port": 6379 }] } },
        { "metadata": { "name": "api", "namespace": "stage" },
          "spec": { "clusterIP": "10.0.0.9", "ports": [{ "name": "http", "port": 80 }] } }
      ]
    }"#;

    #[test]
    fn database_services_are_found_ready_to_forward() {
        let found = parse(SERVICES, "gke_stage");
        let shown: Vec<(String, String, DbType)> = found
            .iter()
            .map(|d| {
                (
                    d.connection.name.clone(),
                    d.connection.address(),
                    d.connection.driver,
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "stage-postgresql (stage)".into(),
                    "svc/stage-postgresql:5432 in stage (gke_stage)".into(),
                    DbType::Postgres
                ),
                (
                    "crm-rw (crm)".into(),
                    "svc/crm-rw:5432 in crm (gke_stage)".into(),
                    DbType::Postgres
                ),
                (
                    "cache (stage)".into(),
                    "svc/cache:6379 in stage (gke_stage)".into(),
                    DbType::Redis
                ),
            ]
        );
        assert_eq!(found[0].source, "kubernetes: gke_stage");
        assert_eq!(found[0].connection.tag, Tag::None);
    }

    #[test]
    fn a_prod_context_tags_its_databases_red() {
        let found = parse(SERVICES, "gke_product-crm-prod");
        assert!(found.iter().all(|d| d.connection.tag == Tag::Red));
        assert!(parse("not json", "x").is_empty());
    }
}
