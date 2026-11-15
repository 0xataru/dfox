//! A Kubernetes service reached through `kubectl port-forward`, the user's
//! kubeconfig and its auth plugins doing the login: no client crate, no new
//! dependency, and EKS, GKE and AKS logins work as they do in a shell.
//!
//! `kubectl` runs under a small `sh` that stops it when dfox's end of its
//! stdin closes, which happens when the [`Forward`] drops and when dfox
//! exits or dies: the forward never outlives dfox.

use std::{net::SocketAddr, process::Stdio, time::Duration};

use dfox_core::{errors::DbError, models::connections::KubeParams};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, ChildStdin, Command},
};

/// How long `kubectl` may take to start forwarding.
const START: Duration = Duration::from_secs(20);

/// A running `kubectl port-forward`; it stops on drop.
#[derive(Debug)]
pub struct Forward {
    local: SocketAddr,
    // Dropped first: closing it stops kubectl.
    _stdin: ChildStdin,
    _child: Child,
}

impl Forward {
    pub fn local(&self) -> SocketAddr {
        self.local
    }

    /// Forwards a free local port to the service's `port`.
    pub async fn open(kube: &KubeParams, port: u16) -> Result<Forward, DbError> {
        Self::open_with("kubectl", kube, port).await
    }

    async fn open_with(kubectl: &str, kube: &KubeParams, port: u16) -> Result<Forward, DbError> {
        let mut command = if cfg!(unix) {
            // The shell outlives nothing: it stops kubectl once its stdin
            // closes, then ends itself, so it isn't killed on drop.
            let mut sh = Command::new("sh");
            sh.args([
                "-c",
                r#""$@" & pid=$!; read -r _; kill "$pid""#,
                "sh",
                kubectl,
            ]);
            sh
        } else {
            // shortcut: on Windows kubectl stops with the forward, but not if
            // dfox is killed; tie it to a job object if that matters.
            let mut direct = Command::new(kubectl);
            direct.kill_on_drop(true);
            direct
        };
        command
            .args(args(kube, port))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|err| DbError::Connection(format!("kubectl: {err}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let mut lines = BufReader::new(stdout).lines();
        let started = tokio::time::timeout(START, async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(local) = forwarding(&line) {
                    return Some(local);
                }
            }
            None
        })
        .await;
        match started {
            Ok(Some(local)) => {
                // kubectl keeps writing a line per connection; read them so
                // its pipe never fills.
                tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
                Ok(Forward {
                    local,
                    _stdin: stdin,
                    _child: child,
                })
            }
            Ok(None) => {
                let mut error = String::new();
                let _ = stderr.read_to_string(&mut error).await;
                Err(DbError::Connection(kubectl_error(&error)))
            }
            Err(_) => Err(DbError::Connection(format!(
                "kubectl port-forward to svc/{} didn't start in {}s",
                kube.service,
                START.as_secs()
            ))),
        }
    }
}

/// `kubectl`'s arguments: `[--context c] -n ns port-forward svc/name :port`,
/// the empty local port letting it pick a free one.
pub fn args(kube: &KubeParams, port: u16) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(context) = &kube.context {
        args.extend(["--context".to_string(), context.clone()]);
    }
    args.extend([
        "-n".to_string(),
        kube.namespace.clone(),
        "port-forward".to_string(),
        format!("svc/{}", kube.service),
        format!(":{port}"),
        "--address".to_string(),
        "127.0.0.1".to_string(),
    ]);
    args
}

/// The local end of `Forwarding from 127.0.0.1:54321 -> 5432`.
pub fn forwarding(line: &str) -> Option<SocketAddr> {
    line.strip_prefix("Forwarding from ")?
        .split(" -> ")
        .next()?
        .parse()
        .ok()
}

/// kubectl's error, its last line without the `error: ` prefix.
fn kubectl_error(stderr: &str) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("kubectl port-forward exited");
    format!("kubectl: {}", line.strip_prefix("error: ").unwrap_or(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kube(context: Option<&str>) -> KubeParams {
        KubeParams {
            context: context.map(str::to_string),
            namespace: "db".into(),
            service: "pg".into(),
        }
    }

    #[test]
    fn kubectl_is_asked_for_the_service_on_a_free_port() {
        assert_eq!(
            args(&kube(Some("stage")), 5432).join(" "),
            "--context stage -n db port-forward svc/pg :5432 --address 127.0.0.1"
        );
        assert_eq!(
            args(&kube(None), 6379).join(" "),
            "-n db port-forward svc/pg :6379 --address 127.0.0.1"
        );
    }

    #[test]
    fn the_local_port_is_read_from_kubectl() {
        assert_eq!(
            forwarding("Forwarding from 127.0.0.1:54321 -> 5432"),
            Some("127.0.0.1:54321".parse().expect("addr"))
        );
        assert_eq!(forwarding("Handling connection for 54321"), None);
        assert_eq!(
            kubectl_error("error: services \"pg\" not found\n"),
            "kubectl: services \"pg\" not found"
        );
    }

    /// A stand-in `kubectl` on the PATH that prints what the real one
    /// prints, then waits: closing the forward must stop it.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_forward_stops_with_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("dfox-kubectl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let marker = dir.join("alive");
        let script = format!(
            "#!/bin/sh\necho 'Forwarding from 127.0.0.1:45678 -> 5432'\ntouch '{}'\ntrap 'rm -f \"{}\"; exit 0' TERM\nwhile :; do sleep 0.1; done\n",
            marker.display(),
            marker.display()
        );
        let kubectl = dir.join("kubectl");
        std::fs::write(&kubectl, script).expect("write");
        std::fs::set_permissions(&kubectl, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let forward = Forward::open_with(kubectl.to_str().expect("utf-8"), &kube(None), 5432)
            .await
            .expect("forward");
        assert_eq!(forward.local().port(), 45678);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(marker.exists(), "kubectl runs");
        drop(forward);
        for _ in 0..30 {
            if !marker.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!marker.exists(), "kubectl stopped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
