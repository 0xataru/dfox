//! Databases the user already reaches through SSH: hosts of
//! `~/.ssh/config` with a `LocalForward` to a database's port, offered as a
//! connection through that host with dfox's own tunnel, its user, port and
//! key as the config has them. `Include` and `Match` are left as they are.

use super::{engines, Connection, Ssh, SshLogin};
use crate::components::manager::Detected;

pub fn find() -> Vec<Detected> {
    let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) else {
        return Vec::new();
    };
    match std::fs::read_to_string(home.join(".ssh").join("config")) {
        Ok(text) => parse(&text),
        Err(_) => Vec::new(),
    }
}

/// One `Host` block's settings that matter here.
#[derive(Default)]
struct Block {
    aliases: Vec<String>,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity: Option<String>,
    /// `LocalForward`s: the local port, and the target's host and port as
    /// seen from the SSH host.
    forwards: Vec<(Option<u16>, String, u16)>,
}

/// How a source names the local port a `LocalForward` listens on.
const ON: &str = "on localhost:";

/// The local port of the `LocalForward` a source names: while the tunnel
/// is up, the same database is found there too.
pub fn local_port(source: &str) -> Option<u16> {
    source
        .split(" · ")
        .find_map(|part| part.strip_prefix(ON)?.parse().ok())
}

pub fn parse(text: &str) -> Vec<Detected> {
    let mut blocks: Vec<Block> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once(|c: char| c.is_whitespace() || c == '=')
            .map(|(k, v)| (k.to_lowercase(), v.trim().trim_start_matches('=').trim()))
            .unwrap_or((line.to_lowercase(), ""));
        let value = value.trim_matches('"');
        match key.as_str() {
            "host" => blocks.push(Block {
                aliases: value.split_whitespace().map(str::to_string).collect(),
                ..Block::default()
            }),
            // Settings for other matches: not a host of its own.
            "match" => blocks.push(Block::default()),
            _ => {
                let Some(block) = blocks.last_mut() else {
                    continue;
                };
                match key.as_str() {
                    "hostname" => block.hostname = Some(value.to_string()),
                    "user" => block.user = Some(value.to_string()),
                    "port" => block.port = value.parse().ok(),
                    "identityfile" => block.identity = Some(value.to_string()),
                    "localforward" => {
                        // `[bind:]port host:hostport`
                        let mut parts = value.split_whitespace();
                        let local = parts
                            .next()
                            .and_then(|bind| bind.rsplit(':').next()?.parse().ok());
                        let target = parts.next().unwrap_or_default();
                        if let Some((host, port)) = target.rsplit_once(':') {
                            if let Ok(port) = port.parse() {
                                let host = host.trim_matches(['[', ']']).to_string();
                                block.forwards.push((local, host, port));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut found = Vec::new();
    for block in blocks {
        // A pattern isn't a host to log in to.
        let Some(alias) = block.aliases.iter().find(|a| !a.contains(['*', '?', '!'])) else {
            continue;
        };
        for (local, target, port) in &block.forwards {
            let Some(driver) = engines::by_port(*port) else {
                continue;
            };
            let mut connection = Connection::new(&format!("{alias} → {target}:{port}"), driver);
            connection.host = target.clone();
            connection.port = Some(*port);
            connection.ssh = Some(Ssh {
                host: block.hostname.clone().unwrap_or_else(|| alias.clone()),
                port: block.port,
                user: block.user.clone().unwrap_or_default(),
                login: if block.identity.is_some() {
                    SshLogin::Key
                } else {
                    SshLogin::Agent
                },
                key: block.identity.clone(),
                known_hosts: None,
            });
            found.push(Detected {
                connection,
                password: None,
                source: match local {
                    Some(local) => format!("~/.ssh/config: Host {alias} · {ON}{local}"),
                    None => format!("~/.ssh/config: Host {alias}"),
                },
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use dfox_core::models::connections::DbType;

    #[test]
    fn local_forwards_to_database_ports_become_connections() {
        let text = "\
# work
Host bastion-stage stage
    HostName 203.0.113.7
    User deploy
    Port 2222
    IdentityFile ~/.ssh/stage
    LocalForward 15432 db.internal:5432
    LocalForward 8080 web.internal:80
    LocalForward 127.0.0.1:16379 [10.0.0.5]:6379

Host *.example.com
    LocalForward 13306 db:3306

Host plain
    LocalForward 5433 localhost:5432

Match host other
    LocalForward 1 x:3306
";
        let found = parse(text);
        let shown: Vec<(String, String, DbType, String)> = found
            .iter()
            .map(|d| {
                (
                    d.connection.name.clone(),
                    d.connection.address(),
                    d.connection.driver,
                    d.source.clone(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "bastion-stage → db.internal:5432".into(),
                    "db.internal:5432 via 203.0.113.7".into(),
                    DbType::Postgres,
                    "~/.ssh/config: Host bastion-stage · on localhost:15432".into()
                ),
                (
                    "bastion-stage → 10.0.0.5:6379".into(),
                    "10.0.0.5:6379 via 203.0.113.7".into(),
                    DbType::Redis,
                    "~/.ssh/config: Host bastion-stage · on localhost:16379".into()
                ),
                (
                    "plain → localhost:5432".into(),
                    "localhost:5432 via plain".into(),
                    DbType::Postgres,
                    "~/.ssh/config: Host plain · on localhost:5433".into()
                ),
            ]
        );
        let ssh = found[0].connection.ssh.as_ref().expect("ssh");
        assert_eq!(
            (ssh.user.as_str(), ssh.port, ssh.login, ssh.key.as_deref()),
            ("deploy", Some(2222), SshLogin::Key, Some("~/.ssh/stage"))
        );
        assert_eq!(
            found[2].connection.ssh.as_ref().map(|s| s.login),
            Some(SshLogin::Agent)
        );
    }
}
