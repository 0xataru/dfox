//! Databases already reachable on this machine through a forward or tunnel
//! someone opened (`kubectl port-forward`, k9s, `ssh -L`, a cloud proxy) or
//! a server running here. The ports listening on the loopback come from
//! `lsof` (`ss` where there is none); each is told apart by a short
//! handshake, not its number, since a forward is often on 15432. Nothing
//! logs in: a probe only reads how the server greets.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
    time::Duration,
};

use dfox_core::models::connections::DbType;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use super::Connection;
use crate::components::manager::Detected;

/// How long one probe waits for an answer.
const PROBE: Duration = Duration::from_millis(400);
/// How long a command may take.
const COMMAND: Duration = Duration::from_secs(2);
/// Ports probed at most, so a machine full of services stays quick.
const MAX_PORTS: usize = 64;

/// A port listening on the loopback, and who holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub port: u16,
    pub pid: u32,
    pub process: String,
}

/// What each port held by a process was found to be, so a port is probed
/// once while the same process holds it.
pub type Probed = Mutex<HashMap<(u16, u32), Option<Detected>>>;

/// The databases behind the loopback's listening ports, probing only the
/// ports `probed` doesn't know yet.
pub async fn find(probed: &Probed) -> Vec<Detected> {
    let listeners: Vec<Listener> = match listeners().await {
        Some(listeners) => listeners.into_iter().take(MAX_PORTS).collect(),
        None => return Vec::new(),
    };
    let new: Vec<Listener> = {
        let known = probed.lock().unwrap_or_else(|e| e.into_inner());
        listeners
            .iter()
            .filter(|l| !known.contains_key(&(l.port, l.pid)))
            .cloned()
            .collect()
    };
    let probes = new.into_iter().map(|l| async move {
        let found = match identify(l.port).await {
            Some(driver) => {
                let args = if l.process == "kubectl" {
                    run("ps", &["-o", "args=", "-p", &l.pid.to_string()]).await
                } else {
                    None
                };
                Some(detected(&l, driver, args.as_deref()))
            }
            None => None,
        };
        ((l.port, l.pid), found)
    });
    let results = futures_util::future::join_all(probes).await;
    let mut known = probed.lock().unwrap_or_else(|e| e.into_inner());
    known.extend(results);
    // A port given up is forgotten, so a forward opened on it again counts.
    known.retain(|key, _| listeners.iter().any(|l| (l.port, l.pid) == *key));
    listeners
        .iter()
        .filter_map(|l| known.get(&(l.port, l.pid)).cloned().flatten())
        .collect()
}

async fn run(command: &str, args: &[&str]) -> Option<String> {
    let out = tokio::time::timeout(
        COMMAND,
        tokio::process::Command::new(command)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The loopback's listening ports, without the container engines' own:
/// their databases are found with their passwords as containers.
async fn listeners() -> Option<Vec<Listener>> {
    let found = match run("lsof", &["-iTCP", "-sTCP:LISTEN", "-P", "-n", "-F", "pcn"]).await {
        Some(out) => parse_lsof(&out),
        None => parse_ss(&run("ss", &["-ltnpH"]).await?),
    };
    Some(
        found
            .into_iter()
            .filter(|l| !is_container_engine(&l.process))
            .collect(),
    )
}

fn is_container_engine(process: &str) -> bool {
    let process = process.to_lowercase();
    [
        "docker",
        "vpnkit",
        "gvproxy",
        "podman",
        "orbstack",
        "limactl",
        "rootlessport",
    ]
    .iter()
    .any(|name| process.contains(name))
}

/// Whether a listening address is reachable on the loopback.
fn on_loopback(address: &str) -> Option<u16> {
    let (host, port) = address.rsplit_once(':')?;
    let host = host.trim_matches(['[', ']']);
    matches!(
        host,
        "*" | "127.0.0.1" | "::1" | "0.0.0.0" | "::" | "localhost"
    )
    .then(|| port.parse().ok())
    .flatten()
}

/// `lsof -F pcn`: `p` a pid, `c` its command, `n` an address it listens on.
fn parse_lsof(out: &str) -> Vec<Listener> {
    let mut ports: BTreeMap<u16, Listener> = BTreeMap::new();
    let (mut pid, mut process) = (0, String::new());
    for line in out.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => pid = value.parse().unwrap_or(0),
            "c" => process = value.to_string(),
            "n" => {
                if let Some(port) = on_loopback(value) {
                    ports.entry(port).or_insert_with(|| Listener {
                        port,
                        pid,
                        process: process.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    ports.into_values().collect()
}

/// `ss -ltnpH`: `LISTEN 0 4096 127.0.0.1:15432 0.0.0.0:* users:(("kubectl",pid=12,fd=8))`.
fn parse_ss(out: &str) -> Vec<Listener> {
    let mut ports: BTreeMap<u16, Listener> = BTreeMap::new();
    for line in out.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(port) = fields.get(3).and_then(|a| on_loopback(a)) else {
            continue;
        };
        let users = fields.get(5).copied().unwrap_or_default();
        let process = users.split('"').nth(1).unwrap_or_default().to_string();
        let pid = users
            .split("pid=")
            .nth(1)
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|pid| pid.parse().ok())
            .unwrap_or(0);
        ports.entry(port).or_insert(Listener { port, pid, process });
    }
    ports.into_values().collect()
}

/// The engine answering on `port`, by how it greets: MySQL speaks first;
/// the others answer a request of their own protocol. The engine whose
/// usual port this is goes first.
pub async fn identify(port: u16) -> Option<DbType> {
    if let Some(greeting) = exchange(port, &[]).await {
        // MySQL's handshake: a packet whose payload starts with protocol 10.
        return (greeting.len() > 5 && greeting[4] == 10).then_some(DbType::MySql);
    }
    let mut order = [
        DbType::Postgres,
        DbType::Redis,
        DbType::Mongo,
        DbType::Cassandra,
    ];
    if let Some(i) = order
        .iter()
        .position(|d| Connection::default_port(*d) == port)
    {
        order[..=i].rotate_right(1);
    }
    for driver in order {
        let answer = exchange(port, &request(driver)).await;
        if answer.is_some_and(|a| answers(driver, &a)) {
            return Some(driver);
        }
    }
    None
}

/// What `driver` is asked: something every server of it answers before
/// any login.
fn request(driver: DbType) -> Vec<u8> {
    match driver {
        // SSLRequest: answered with one byte, `S` or `N`.
        DbType::Postgres => vec![0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f],
        DbType::Redis => b"PING\r\n".to_vec(),
        // OPTIONS, protocol 4, stream 0.
        DbType::Cassandra => vec![0x04, 0, 0, 0, 0x05, 0, 0, 0, 0],
        DbType::Mongo => mongo_hello(),
        _ => Vec::new(),
    }
}

/// An `OP_MSG` with `{ hello: 1, $db: "admin" }`.
fn mongo_hello() -> Vec<u8> {
    let mut doc = Vec::new();
    doc.push(0x10);
    doc.extend(b"hello\0");
    doc.extend(1i32.to_le_bytes());
    doc.push(0x02);
    doc.extend(b"$db\0");
    doc.extend(6i32.to_le_bytes());
    doc.extend(b"admin\0");
    doc.push(0);
    let doc = [((doc.len() + 4) as i32).to_le_bytes().to_vec(), doc].concat();
    let body = [0u32.to_le_bytes().to_vec(), vec![0], doc].concat();
    let length = (16 + body.len()) as i32;
    let header = [length, 1, 0, 2013]
        .iter()
        .flat_map(|n| n.to_le_bytes())
        .collect::<Vec<u8>>();
    [header, body].concat()
}

fn answers(driver: DbType, answer: &[u8]) -> bool {
    match driver {
        DbType::Postgres => answer == b"S" || answer == b"N",
        DbType::Redis => {
            answer.starts_with(b"+PONG")
                || answer.starts_with(b"-NOAUTH")
                || answer.starts_with(b"-ERR")
        }
        DbType::Cassandra => answer.len() >= 5 && answer[0] & 0x80 != 0 && answer[4] == 0x06,
        DbType::Mongo => answer.len() >= 16 && answer[12..16] == 2013i32.to_le_bytes(),
        _ => false,
    }
}

/// Sends `request` to `port` and reads the first answer, or with nothing to
/// send, what the server says first. None without an answer in time.
async fn exchange(port: u16, request: &[u8]) -> Option<Vec<u8>> {
    tokio::time::timeout(PROBE, async {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        if !request.is_empty() {
            stream.write_all(request).await.ok()?;
        }
        let mut buf = vec![0u8; 512];
        let n = stream.read(&mut buf).await.ok()?;
        (n > 0).then(|| {
            buf.truncate(n);
            buf
        })
    })
    .await
    .ok()
    .flatten()
}

/// A `kubectl port-forward` command line: its target with namespace and
/// context, as `svc/pg in db`.
fn kubectl_target(args: &str) -> Option<String> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "port-forward")?;
    let flag = |names: &[&str]| {
        words.iter().enumerate().find_map(|(i, w)| {
            names.iter().find_map(|name| {
                w.strip_prefix(&format!("{name}="))
                    .map(str::to_string)
                    .or_else(|| {
                        (w == name)
                            .then(|| words.get(i + 1).map(|v| v.to_string()))
                            .flatten()
                    })
            })
        })
    };
    let mut rest = words[at + 1..].iter().filter(|w| !w.starts_with('-'));
    let target = rest.next()?;
    // A flag's value isn't the target.
    let target =
        if flag(&["-n", "--namespace", "--context", "--address"]).as_deref() == Some(*target) {
            rest.next()?
        } else {
            target
        };
    let mut shown = target.to_string();
    if let Some(namespace) = flag(&["-n", "--namespace"]) {
        shown.push_str(&format!(" in {namespace}"));
    }
    if let Some(context) = flag(&["--context"]) {
        shown.push_str(&format!(" ({context})"));
    }
    Some(shown)
}

fn detected(listener: &Listener, driver: DbType, args: Option<&str>) -> Detected {
    let source = match listener.process.as_str() {
        "kubectl" => match args.and_then(kubectl_target) {
            Some(target) => format!("kubectl port-forward {target}"),
            None => "kubectl port-forward".to_string(),
        },
        "k9s" => "port-forward from k9s".to_string(),
        "ssh" => "an SSH tunnel".to_string(),
        process => format!("port {} ({process})", listener.port),
    };
    let mut connection = Connection::new(&format!("localhost:{}", listener.port), driver);
    connection.host = "127.0.0.1".into();
    connection.port = Some(listener.port);
    Detected {
        connection,
        password: None,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn lsof_lists_loopback_ports_once() {
        let out = "p52\nccom.docker.backend\nf16\nn*:15432\np71\nckubectl\nf8\nn127.0.0.1:25432\nf9\nn[::1]:25432\np80\ncnode\nf3\nn10.0.0.5:3000\n";
        let found = parse_lsof(out);
        let ports: Vec<(u16, &str)> = found.iter().map(|l| (l.port, l.process.as_str())).collect();
        assert_eq!(ports, [(15432, "com.docker.backend"), (25432, "kubectl")]);
        assert!(is_container_engine("com.docker.backend"));
        assert!(!is_container_engine("kubectl"));
    }

    #[test]
    fn ss_lists_loopback_ports_with_their_process() {
        let out = "LISTEN 0 4096 127.0.0.1:15432 0.0.0.0:* users:((\"kubectl\",pid=1234,fd=8))\n\
                   LISTEN 0 128 192.168.1.2:22 0.0.0.0:* users:((\"sshd\",pid=1,fd=3))\n";
        assert_eq!(
            parse_ss(out),
            [Listener {
                port: 15432,
                pid: 1234,
                process: "kubectl".into()
            }]
        );
    }

    #[test]
    fn kubectl_names_where_its_forward_leads() {
        let args = "kubectl port-forward -n db svc/pg 15432:5432 --context prod";
        assert_eq!(kubectl_target(args).as_deref(), Some("svc/pg in db (prod)"));
        let args = "kubectl --namespace=cache port-forward pod/redis-0 6380:6379";
        assert_eq!(
            kubectl_target(args).as_deref(),
            Some("pod/redis-0 in cache")
        );
        assert_eq!(kubectl_target("kubectl get pods"), None);
    }

    /// A server on a free port that answers each read with `reply`, or
    /// greets with it first.
    async fn fake(reply: &'static [u8], first: bool) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    if first {
                        let _ = socket.write_all(reply).await;
                    }
                    let mut buf = [0u8; 512];
                    while let Ok(n) = socket.read(&mut buf).await {
                        if n == 0 || first {
                            break;
                        }
                        let _ = socket.write_all(reply).await;
                    }
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn handshakes_tell_the_engines_apart() {
        let mysql = fake(b"\x4a\0\0\0\x0a8.4.0\0", true).await;
        let postgres = fake(b"N", false).await;
        let redis = fake(b"-NOAUTH Authentication required.\r\n", false).await;
        let cassandra = fake(b"\x84\0\0\0\x06\0\0\0\0", false).await;
        let mut mongo_reply = vec![0u8; 16];
        mongo_reply[12..16].copy_from_slice(&2013i32.to_le_bytes());
        let mongo = fake(Box::leak(mongo_reply.into_boxed_slice()), false).await;
        let http = fake(b"HTTP/1.1 400 Bad Request\r\n\r\n", false).await;
        assert_eq!(identify(mysql).await, Some(DbType::MySql));
        assert_eq!(identify(postgres).await, Some(DbType::Postgres));
        assert_eq!(identify(redis).await, Some(DbType::Redis));
        assert_eq!(identify(cassandra).await, Some(DbType::Cassandra));
        assert_eq!(identify(mongo).await, Some(DbType::Mongo));
        assert_eq!(identify(http).await, None);
    }
}
