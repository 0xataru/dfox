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

/// The databases of the `wanted` contexts (`kube_contexts` in the config):
/// `current`, `all` but those named like prod, or contexts by name. Each is
/// asked on its own, so one that doesn't answer holds up none of the others.
pub async fn find(wanted: Vec<String>) -> Vec<Detected> {
    let contexts = contexts(&wanted).await;
    let found = contexts.iter().map(|context| async move {
        let all = kubectl(&[
            "--context",
            context,
            "get",
            "services",
            "--all-namespaces",
            "-o",
            "json",
        ])
        .await;
        // Without the right to list every namespace, the context's own.
        let json = match all {
            Some(json) => Some(json),
            None => kubectl(&["--context", context, "get", "services", "-o", "json"]).await,
        };
        let Some(json) = json else {
            log::info!("kubernetes: {context} didn't answer");
            return Vec::new();
        };
        // Which pods each service leads to, to show a database once under
        // two services. Without the right to list them, every service shows.
        let endpoints = kubectl(&[
            "--context",
            context,
            "get",
            "endpointslices",
            "--all-namespaces",
            "-o",
            "json",
        ])
        .await
        .map(|json| parse_endpoints(&json));
        collapse(parse(&json, context), endpoints.as_ref(), &externals(&json))
    });
    futures_util::future::join_all(found).await.concat()
}

/// The contexts `wanted` names, each once.
async fn contexts(wanted: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in wanted {
        let names: Vec<String> = match name.as_str() {
            "current" => kubectl(&["config", "current-context"])
                .await
                .map(|c| vec![c.trim().to_string()])
                .unwrap_or_default(),
            // A prod cluster is asked only when named: not behind the back.
            "all" => kubectl(&["config", "get-contexts", "-o", "name"])
                .await
                .unwrap_or_default()
                .lines()
                .map(str::trim)
                .filter(|c| !c.is_empty() && !is_prod(c))
                .map(str::to_string)
                .collect(),
            name => vec![name.to_string()],
        };
        for name in names {
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

fn is_prod(context: &str) -> bool {
    context.to_lowercase().contains("prod")
}

/// A context's name as people say it: `gke_project_zone_crm-stage` →
/// `crm-stage (gke)`, `do-ams3-k8s-1-29-stage-crm-1-do-0-ams3-1714463690478`
/// → `stage-crm-1 (do)`, `arn:aws:eks:…:cluster/crm` → `crm (eks)`.
pub fn short_context(context: &str) -> String {
    if let Some(rest) = context.strip_prefix("gke_") {
        if let Some(cluster) = rest.rsplit('_').next().filter(|c| !c.is_empty()) {
            return format!("{cluster} (gke)");
        }
    }
    if let Some((_, cluster)) = context
        .split_once(":cluster/")
        .filter(|_| context.starts_with("arn:aws:eks:"))
    {
        return format!("{cluster} (eks)");
    }
    if let Some(rest) = context.strip_prefix("do-") {
        // `do-<region>-<cluster>`, the cluster often DigitalOcean's own
        // `k8s-<version>-<name>-do-<n>-<region>-<time>`.
        let cluster = rest.split_once('-').map_or(rest, |(_, c)| c);
        let named = cluster
            .strip_prefix("k8s-")
            .map(|c| c.splitn(3, '-').nth(2).unwrap_or(c))
            .and_then(|c| c.split("-do-").next())
            .filter(|c| !c.is_empty())
            .unwrap_or(cluster);
        return format!("{named} (do)");
    }
    if let Some(cluster) = context.strip_prefix("kind-") {
        return format!("{cluster} (kind)");
    }
    context.to_string()
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
    /// `ExternalName` for a service that is another one's DNS name.
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default, rename = "externalName")]
    external_name: Option<String>,
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
    let prod = is_prod(context);
    let short = short_context(context);
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
            // The namespace said as one, and the cluster: `(stage)` read as
            // the environment.
            let name = format!("{} · ns {namespace} · {short}", s.metadata.name);
            let mut connection = Connection::new(&name, driver);
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

#[derive(Deserialize)]
struct Slice {
    metadata: SliceMetadata,
    #[serde(default)]
    endpoints: Vec<SliceEndpoint>,
    #[serde(default)]
    ports: Vec<SlicePort>,
}

#[derive(Deserialize)]
struct SliceMetadata {
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct SliceEndpoint {
    #[serde(default)]
    addresses: Vec<String>,
    #[serde(default)]
    conditions: Conditions,
}

#[derive(Deserialize, Default)]
struct Conditions {
    /// Unset means ready.
    #[serde(default)]
    ready: Option<bool>,
}

#[derive(Deserialize)]
struct SlicePort {
    #[serde(default)]
    port: Option<u16>,
}

/// Per namespace and service, the ready pods' addresses it leads to,
/// sorted, from its EndpointSlices: two services with the same ones reach
/// the same database, and one with none reaches nothing.
fn parse_endpoints(json: &str) -> BTreeMap<(String, String), Vec<String>> {
    #[derive(Deserialize)]
    struct List {
        #[serde(default)]
        items: Vec<Slice>,
    }
    let Ok(list) = serde_json::from_str::<List>(json) else {
        return BTreeMap::new();
    };
    let mut out: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for slice in list.items {
        let Some(service) = slice.metadata.labels.get("kubernetes.io/service-name") else {
            continue;
        };
        let ports: Vec<u16> = slice.ports.iter().filter_map(|p| p.port).collect();
        let at = out
            .entry((slice.metadata.namespace.clone(), service.clone()))
            .or_default();
        for endpoint in slice
            .endpoints
            .iter()
            .filter(|e| e.conditions.ready != Some(false))
        {
            for address in &endpoint.addresses {
                at.extend(ports.iter().map(|port| format!("{address}:{port}")));
            }
        }
    }
    for at in out.values_mut() {
        at.sort();
        at.dedup();
    }
    out.retain(|_, at| !at.is_empty());
    out
}

/// `ExternalName` services naming another service of the cluster
/// (`stage-postgresql.stage.svc.cluster.local`): which one, by namespace
/// and name.
fn externals(json: &str) -> BTreeMap<(String, String), (String, String)> {
    let Ok(list) = serde_json::from_str::<List>(json) else {
        return BTreeMap::new();
    };
    list.items
        .into_iter()
        .filter(|s| s.spec.kind.as_deref() == Some("ExternalName"))
        .filter_map(|s| {
            let target = s.spec.external_name?;
            let mut parts = target.split('.');
            let (name, namespace) = (parts.next()?, parts.next()?);
            (parts.next() == Some("svc")).then(|| {
                (
                    (s.metadata.namespace, s.metadata.name),
                    (namespace.to_string(), name.to_string()),
                )
            })
        })
        .collect()
}

/// One entry per database: services that lead to the same pods, in any
/// namespace (pod addresses are the cluster's own), or that name another
/// service, show once under the most specific name (`stage-postgresql`
/// rather than `postgresql`), the others named in its source.
fn collapse(
    found: Vec<Detected>,
    endpoints: Option<&BTreeMap<(String, String), Vec<String>>>,
    externals: &BTreeMap<(String, String), (String, String)>,
) -> Vec<Detected> {
    // Known endpoints tell a service that reaches no pod: left out, as
    // there is nothing to connect to. Unknown ones (no right to list them)
    // leave every service in.
    let none = BTreeMap::new();
    let (endpoints, known) = match endpoints {
        Some(endpoints) => (endpoints, true),
        None => (&none, false),
    };
    let found: Vec<Detected> = found
        .into_iter()
        .filter(|d| {
            let Some(kube) = &d.connection.kube else {
                return true;
            };
            let key = (kube.namespace.clone(), kube.service.clone());
            !known || externals.contains_key(&key) || endpoints.contains_key(&key)
        })
        .collect();
    let pods = |d: &Detected| -> Option<Vec<String>> {
        let kube = d.connection.kube.as_ref()?;
        let mut key = (kube.namespace.clone(), kube.service.clone());
        if let Some(target) = externals.get(&key) {
            key = target.clone();
        }
        Some(
            endpoints
                .get(&key)
                .cloned()
                // A service without ready pods is told by its name.
                .unwrap_or_else(|| vec![format!("svc:{}/{}", key.0, key.1)]),
        )
    };
    let specific = |d: &Detected| {
        let service = d
            .connection
            .kube
            .as_ref()
            .map_or("", |k| k.service.as_str());
        (service.split('-').count(), service.len())
    };
    let mut kept: Vec<Detected> = Vec::new();
    for entry in found {
        let same =
            pods(&entry).and_then(|at| kept.iter().position(|k| pods(k).as_ref() == Some(&at)));
        let Some(i) = same else {
            kept.push(entry);
            continue;
        };
        let (keep, other) = if specific(&entry) > specific(&kept[i]) {
            (entry, kept[i].clone())
        } else {
            (kept[i].clone(), entry)
        };
        let mut keep = keep;
        if let Some(kube) = &other.connection.kube {
            // After ` · `, so the section is still told by the first part.
            keep.source = format!(
                "{} · also svc/{} in ns {}",
                keep.source, kube.service, kube.namespace
            );
        }
        // The aliases it already had come along.
        for alias in other
            .source
            .split(" · ")
            .filter(|s| s.starts_with("also svc/"))
        {
            keep.source = format!("{} · {alias}", keep.source);
        }
        kept[i] = keep;
    }
    kept
}

/// The other services a found Kubernetes connection stands for, as
/// `(namespace, service)`: from its source's `also svc/<name> in ns <ns>`.
pub fn aliases(source: &str) -> Vec<(String, String)> {
    source
        .split(" · ")
        .filter_map(|part| {
            let (service, namespace) = part.strip_prefix("also svc/")?.split_once(" in ns ")?;
            Some((namespace.to_string(), service.to_string()))
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
                    "stage-postgresql · ns stage · stage (gke)".into(),
                    "svc/stage-postgresql:5432 in ns stage · stage (gke)".into(),
                    DbType::Postgres
                ),
                (
                    "crm-rw · ns crm · stage (gke)".into(),
                    "svc/crm-rw:5432 in ns crm · stage (gke)".into(),
                    DbType::Postgres
                ),
                (
                    "cache · ns stage · stage (gke)".into(),
                    "svc/cache:6379 in ns stage · stage (gke)".into(),
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

    #[test]
    fn clusters_are_named_as_people_say_them() {
        assert_eq!(
            short_context("gke_emerald-griffin-400808_europe-west1-b_profi-crm-stage-cluster"),
            "profi-crm-stage-cluster (gke)"
        );
        assert_eq!(
            short_context("do-ams3-k8s-1-29-stage-crm-1-do-0-ams3-1714463690478"),
            "stage-crm-1 (do)"
        );
        assert_eq!(short_context("do-fra1-billing"), "billing (do)");
        assert_eq!(
            short_context("arn:aws:eks:eu-west-1:123456789012:cluster/crm"),
            "crm (eks)"
        );
        assert_eq!(short_context("kind-dfox"), "dfox (kind)");
        assert_eq!(short_context("minikube"), "minikube");
        assert!(is_prod(
            "gke_profi-crm-416111_europe-west1-b_product-crm-prod"
        ));
    }

    #[test]
    fn services_leading_to_the_same_pods_show_once() {
        let services = r#"{ "items": [
          { "metadata": { "name": "postgresql", "namespace": "stage" },
            "spec": { "clusterIP": "10.0.0.1", "ports": [{ "port": 5432 }] } },
          { "metadata": { "name": "stage-postgresql", "namespace": "stage" },
            "spec": { "clusterIP": "10.0.0.2", "ports": [{ "port": 5432 }] } },
          { "metadata": { "name": "other-postgresql", "namespace": "stage" },
            "spec": { "clusterIP": "10.0.0.3", "ports": [{ "port": 5432 }] } }
        ] }"#;
        let endpoints = r#"{ "items": [
          { "metadata": { "namespace": "stage", "labels": { "kubernetes.io/service-name": "postgresql" } },
            "endpoints": [{ "addresses": ["10.1.0.7"], "conditions": { "ready": true } }], "ports": [{ "port": 5432 }] },
          { "metadata": { "namespace": "stage", "labels": { "kubernetes.io/service-name": "stage-postgresql" } },
            "endpoints": [{ "addresses": ["10.1.0.7"], "conditions": { "ready": true } }], "ports": [{ "port": 5432 }] },
          { "metadata": { "namespace": "stage", "labels": { "kubernetes.io/service-name": "other-postgresql" } },
            "endpoints": [{ "addresses": ["10.1.0.9"], "conditions": { "ready": true } }], "ports": [{ "port": 5432 }] }
        ] }"#;
        let found = collapse(
            parse(services, "do-ams3-stage"),
            Some(&parse_endpoints(endpoints)),
            &BTreeMap::new(),
        );
        let shown: Vec<(String, String)> = found
            .iter()
            .map(|d| {
                (
                    d.connection.kube.as_ref().expect("kube").service.clone(),
                    d.source.clone(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "stage-postgresql".into(),
                    "kubernetes: do-ams3-stage · also svc/postgresql in ns stage".into()
                ),
                (
                    "other-postgresql".into(),
                    "kubernetes: do-ams3-stage".into()
                ),
            ]
        );
        // Without the right to list endpoints, every service shows.
        assert_eq!(
            collapse(parse(services, "x"), None, &BTreeMap::new()).len(),
            3
        );
    }

    #[test]
    fn aliases_in_other_namespaces_and_external_names_show_once() {
        let services = r#"{ "items": [
          { "metadata": { "name": "stage-postgresql", "namespace": "stage" },
            "spec": { "clusterIP": "10.0.0.2", "ports": [{ "port": 5432 }] } },
          { "metadata": { "name": "stage-postgresql", "namespace": "default" },
            "spec": { "clusterIP": "10.0.0.4", "ports": [{ "port": 5432 }] } },
          { "metadata": { "name": "db", "namespace": "apps" },
            "spec": { "type": "ExternalName", "externalName": "stage-postgresql.stage.svc.cluster.local",
                      "ports": [{ "port": 5432 }] } }
        ] }"#;
        let endpoints = r#"{ "items": [
          { "metadata": { "namespace": "stage", "labels": { "kubernetes.io/service-name": "stage-postgresql" } },
            "endpoints": [{ "addresses": ["10.1.0.7"], "conditions": { "ready": true } }], "ports": [{ "port": 5432 }] },
          { "metadata": { "namespace": "default", "labels": { "kubernetes.io/service-name": "stage-postgresql" } },
            "endpoints": [{ "addresses": ["10.1.0.7"], "conditions": { "ready": true } }], "ports": [{ "port": 5432 }] }
        ] }"#;
        let found = collapse(
            parse(services, "do"),
            Some(&parse_endpoints(endpoints)),
            &externals(services),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            aliases(&found[0].source),
            [
                ("default".to_string(), "stage-postgresql".to_string()),
                ("apps".to_string(), "db".to_string()),
            ]
        );
    }

    #[test]
    fn a_service_that_reaches_no_pod_is_left_out() {
        let services = r#"{ "items": [
          { "metadata": { "name": "stage-postgresql", "namespace": "stage" },
            "spec": { "clusterIP": "10.0.0.2", "ports": [{ "port": 5432 }] } },
          { "metadata": { "name": "stage-postgresql", "namespace": "default" },
            "spec": { "clusterIP": "10.0.0.4", "ports": [{ "port": 5432 }] } }
        ] }"#;
        // The one in `default` selects pods of its own namespace: none.
        let slices = r#"{ "items": [
          { "metadata": { "namespace": "stage", "labels": { "kubernetes.io/service-name": "stage-postgresql" } },
            "endpoints": [{ "addresses": ["10.1.0.7"] }], "ports": [{ "port": 5432 }] },
          { "metadata": { "namespace": "default", "labels": { "kubernetes.io/service-name": "stage-postgresql" } },
            "endpoints": [], "ports": [{ "port": 5432 }] },
          { "metadata": { "namespace": "stage", "labels": { "kubernetes.io/service-name": "down" } },
            "endpoints": [{ "addresses": ["10.1.0.8"], "conditions": { "ready": false } }], "ports": [{ "port": 5432 }] }
        ] }"#;
        let endpoints = parse_endpoints(slices);
        assert!(
            !endpoints.contains_key(&("stage".into(), "down".into())),
            "not ready"
        );
        let found = collapse(parse(services, "do"), Some(&endpoints), &BTreeMap::new());
        let namespaces: Vec<&str> = found
            .iter()
            .map(|d| d.connection.kube.as_ref().expect("kube").namespace.as_str())
            .collect();
        assert_eq!(namespaces, ["stage"]);
    }
}
