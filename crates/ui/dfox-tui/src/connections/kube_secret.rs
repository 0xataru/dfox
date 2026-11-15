//! A Kubernetes database's password from the Secret its operator or chart
//! made, read when the user asks for it at the password prompt: `kubectl
//! get secrets`, then the one Secret that fits the service. CloudNativePG's
//! `<cluster>-app`, Zalando's `<user>.<cluster>.credentials…`, and charts'
//! `<release>` with `postgres-password`, `mysql-root-password`… The
//! password is never logged.

use std::{collections::BTreeMap, time::Duration};

use base64::Engine;
use dfox_core::models::connections::{KubeParams, KUBECTL_MISSING};

const TIMEOUT: Duration = Duration::from_secs(15);

/// What a Secret gave: the user when it names one, and the password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub user: Option<String>,
    pub password: String,
}

/// The password for `user` of the database behind `kube`'s service, or why
/// it can't be had. With `any_user`, as at the user prompt, a Secret that
/// names its own user gives that user instead.
pub async fn find(kube: &KubeParams, user: &str, any_user: bool) -> Result<Found, String> {
    let names = kubectl(kube, &["get", "secrets", "-o", "name"]).await?;
    let names: Vec<&str> = names
        .lines()
        .filter_map(|line| line.trim().strip_prefix("secret/"))
        .collect();
    // What each Secret read held, by key names only, for the error.
    let mut tried = Vec::new();
    for name in candidates(&kube.service, user, &names) {
        let json = kubectl(kube, &["get", "secret", &name, "-o", "json"]).await?;
        let data = decode(&json);
        if let Some(found) = pick(&data, user, any_user) {
            return Ok(found);
        }
        if hashed(&data, user) {
            return Err(format!(
                "Secret {name} keeps only a hash of {user}'s password (userlist.txt): type it"
            ));
        }
        let keys: Vec<&str> = data.keys().map(String::as_str).collect();
        tried.push(format!("{name} (keys {})", keys.join(", ")));
    }
    Err(match tried.is_empty() {
        true => format!(
            "no Secret next to svc/{} in {} looks like its database's",
            kube.service, kube.namespace
        ),
        false => format!(
            "no password for {} in Secret {}",
            if user.is_empty() { "any user" } else { user },
            tried.join("; ")
        ),
    })
}

async fn kubectl(kube: &KubeParams, args: &[&str]) -> Result<String, String> {
    let mut command = tokio::process::Command::new("kubectl");
    if let Some(context) = &kube.context {
        command.args(["--context", context]);
    }
    command
        .args(["-n", &kube.namespace])
        .args(args)
        .kill_on_drop(true);
    let out = tokio::time::timeout(TIMEOUT, command.output())
        .await
        .map_err(|_| "kubectl: timed out".to_string())?
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => KUBECTL_MISSING.to_string(),
            _ => format!("kubectl: {err}"),
        })?;
    if !out.status.success() {
        let error = String::from_utf8_lossy(&out.stderr);
        let line = error
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("kubectl failed");
        return Err(format!(
            "kubectl: {}",
            line.strip_prefix("Error from server (Forbidden): ")
                .or_else(|| line.strip_prefix("error: "))
                .unwrap_or(line)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The service's name without the role suffixes operators and charts add:
/// `crm-rw` → `crm`, `stage-postgresql-hl` → `stage-postgresql`.
fn base(service: &str) -> &str {
    for suffix in [
        "-rw",
        "-ro",
        "-r",
        "-primary",
        "-replica",
        "-headless",
        "-hl",
        "-master",
    ] {
        if let Some(base) = service.strip_suffix(suffix) {
            return base;
        }
    }
    service
}

/// The Secrets to read, the likeliest first, among the namespace's `names`.
fn candidates(service: &str, user: &str, names: &[&str]) -> Vec<String> {
    let base = base(service);
    let zalando = format!(".{base}.credentials.postgresql.acid.zalan.do");
    let mut wanted: Vec<String> = vec![format!("{base}-app")];
    match user {
        "" => wanted.extend(
            names
                .iter()
                .filter(|n| n.ends_with(&zalando))
                .map(|n| n.to_string()),
        ),
        user => wanted.push(format!("{user}{zalando}")),
    }
    wanted.extend([service.to_string(), base.to_string()]);
    // Then a chart's other Secrets for the release, as `<release>-auth`.
    wanted.extend(
        names
            .iter()
            .filter(|n| n.starts_with(base) && !n.contains("tls") && !n.contains("token"))
            .map(|n| n.to_string()),
    );
    let mut out: Vec<String> = Vec::new();
    for name in wanted {
        if names.contains(&name.as_str()) && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// A Secret's `data`, decoded.
fn decode(json: &str) -> BTreeMap<String, String> {
    #[derive(serde::Deserialize)]
    struct Secret {
        #[serde(default)]
        data: BTreeMap<String, String>,
    }
    let Ok(secret) = serde_json::from_str::<Secret>(json) else {
        return BTreeMap::new();
    };
    secret
        .data
        .into_iter()
        .filter_map(|(key, value)| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(value)
                .ok()?;
            Some((key, String::from_utf8(bytes).ok()?))
        })
        .collect()
}

/// pgbouncer's `userlist.txt`: `"user" "password"` a line.
fn userlist(data: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let Some(text) = data.get("userlist.txt") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('"').skip(1).step_by(2);
            Some((parts.next()?.to_string(), parts.next()?.to_string()))
        })
        .collect()
}

/// A password pgbouncer keeps as a hash: of no use to log in with.
fn is_hash(password: &str) -> bool {
    password.starts_with("SCRAM-SHA-256$")
        || (password.len() == 35
            && password.starts_with("md5")
            && password[3..].bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Whether the Secret's `userlist.txt` has `user`, but only hashed.
fn hashed(data: &BTreeMap<String, String>, user: &str) -> bool {
    userlist(data).iter().any(|(u, p)| u == user && is_hash(p))
}

/// The password for `user` among a Secret's keys: with the user it names,
/// pgbouncer's `userlist.txt`, or by the key charts give that user's
/// password. With `any_user`, a user the Secret names comes along.
fn pick(data: &BTreeMap<String, String>, user: &str, any_user: bool) -> Option<Found> {
    let get = |key: &str| data.get(key).filter(|v| !v.is_empty()).cloned();
    if let (Some(named), Some(password)) =
        (get("username").or_else(|| get("user")), get("password"))
    {
        return (any_user || user.is_empty() || user == named).then_some(Found {
            user: Some(named),
            password,
        });
    }
    let listed = userlist(data);
    let plain = listed
        .iter()
        .find(|(u, p)| (u == user || (any_user && listed.len() == 1)) && !is_hash(p));
    if let Some((named, password)) = plain {
        return Some(Found {
            user: Some(named.clone()),
            password: password.clone(),
        });
    }
    let admin = [
        "postgres-password",
        "mysql-root-password",
        "mongodb-root-password",
        "root-password",
    ];
    let keys: Vec<&str> = match user {
        "postgres" | "root" => admin.to_vec(),
        // A user of the chart's own, or none named yet: its password, then
        // the admin's, which needs the admin user too.
        _ => [
            "password",
            "mysql-password",
            "redis-password",
            "valkey-password",
        ]
        .into_iter()
        .chain(admin)
        .collect(),
    };
    let (key, password) = keys.iter().find_map(|key| Some((*key, get(key)?)))?;
    let user = match key {
        "postgres-password" => Some("postgres".to_string()),
        "mysql-root-password" | "mongodb-root-password" | "root-password" => {
            Some("root".to_string())
        }
        _ => None,
    };
    Some(Found { user, password })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_secret_next_to_the_service_comes_first() {
        let names = [
            "crm-app",
            "crm-superuser",
            "crm-ca",
            "app.crm.credentials.postgresql.acid.zalan.do",
            "stage-postgresql",
            "stage-postgresql-tls",
            "default-token-x",
        ];
        assert_eq!(
            candidates("crm-rw", "", &names),
            [
                "crm-app",
                "app.crm.credentials.postgresql.acid.zalan.do",
                "crm-superuser",
                "crm-ca"
            ]
        );
        assert_eq!(
            candidates("stage-postgresql", "app", &names),
            ["stage-postgresql"]
        );
        assert!(candidates("other", "", &names).is_empty());
    }

    #[test]
    fn passwords_by_operator_and_chart_keys() {
        // CloudNativePG and Zalando name the user.
        let cnpg = data(&[
            ("username", "app"),
            ("password", "pw"),
            ("uri", "postgresql://…"),
        ]);
        assert_eq!(
            pick(&cnpg, "", false),
            Some(Found {
                user: Some("app".into()),
                password: "pw".into()
            })
        );
        assert_eq!(pick(&cnpg, "other", false), None);
        // A Bitnami chart: the admin's and the app user's.
        let chart = data(&[("postgres-password", "admin"), ("password", "user")]);
        assert_eq!(
            pick(&chart, "postgres", false),
            Some(Found {
                user: Some("postgres".into()),
                password: "admin".into()
            })
        );
        assert_eq!(
            pick(&chart, "app", false).map(|f| f.password),
            Some("user".into())
        );
        assert_eq!(
            pick(&data(&[("redis-password", "r")]), "", false).map(|f| f.password),
            Some("r".into())
        );
        assert_eq!(pick(&data(&[("ca.crt", "x")]), "", false), None);
    }

    #[test]
    fn secret_data_is_decoded() {
        let json = r#"{ "data": { "password": "cHc=", "bad": "%%%" } }"#;
        assert_eq!(decode(json), data(&[("password", "pw")]));
    }

    #[test]
    fn the_user_prompt_takes_the_secrets_own_user() {
        let named = data(&[("username", "productcrm"), ("password", "pw")]);
        assert_eq!(pick(&named, "postgres", false), None);
        assert_eq!(
            pick(&named, "postgres", true),
            Some(Found {
                user: Some("productcrm".into()),
                password: "pw".into()
            })
        );
    }

    #[test]
    fn pgbouncer_userlists_give_plain_passwords_not_hashes() {
        let list = data(&[(
            "userlist.txt",
            "\"app\" \"plain-pw\"\n\"admin\" \"md5c4ca4238a0b923820dcc509a6f75849b\"\n\"scram\" \"SCRAM-SHA-256$4096:abc\"\n",
        )]);
        assert_eq!(
            pick(&list, "app", false).map(|f| f.password),
            Some("plain-pw".into())
        );
        assert_eq!(pick(&list, "admin", false), None);
        assert!(hashed(&list, "admin") && hashed(&list, "scram") && !hashed(&list, "app"));
    }
}
