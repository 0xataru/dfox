use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum DbType {
    Postgres,
    MySql,
    Sqlite,
}

/// Server connection details as typed by the user. Clients build driver
/// options from these fields directly, so no value is ever spliced into a URL
/// and passwords may contain any character.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct ConnectionParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: Option<String>,
    #[serde(default)]
    pub tls: TlsMode,
    /// A PEM file of certificates to trust for [`TlsMode::VerifyFull`], on
    /// top of the system's.
    #[serde(default)]
    pub ca_file: Option<String>,
    /// Reach the server through this SSH jump host; `host` and `port` are
    /// then as seen from the jump host.
    #[serde(default)]
    pub ssh: Option<SshParams>,
    /// Where to connect instead of `host:port`, set while an SSH tunnel
    /// runs. `host` still names the server, for TLS checks. Never saved.
    #[serde(skip)]
    pub tunnel: Option<std::net::SocketAddr>,
    /// Refuse writes. The server enforces it: Postgres and MySQL start every
    /// session read-only, and SQLite opens the file read-only.
    #[serde(default)]
    pub read_only: bool,
}

/// An SSH jump host to tunnel a connection through.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct SshParams {
    pub host: String,
    /// 22 when not set.
    #[serde(default)]
    pub port: Option<u16>,
    pub user: String,
    pub auth: SshAuth,
    /// A `known_hosts` file to check the jump host's key against, instead of
    /// `~/.ssh/known_hosts`.
    #[serde(default)]
    pub known_hosts: Option<String>,
}

/// How to log in to an SSH jump host. Secrets are filled in at connect time
/// and never saved.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SshAuth {
    /// The keys in a running ssh-agent: `$SSH_AUTH_SOCK`, or `socket`.
    Agent {
        #[serde(default)]
        socket: Option<String>,
    },
    /// A private key file, with its passphrase if it has one.
    Key {
        path: String,
        #[serde(default, skip_serializing)]
        passphrase: Option<String>,
    },
    Password {
        #[serde(default, skip_serializing)]
        password: String,
    },
}

/// Whether the connection to a server is encrypted.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// Encrypt when the server supports it.
    #[default]
    Prefer,
    /// Fail unless the connection is encrypted.
    Require,
    /// Encrypt, and check the server's certificate and host name against
    /// the system's trusted roots or [`ConnectionParams::ca_file`].
    #[serde(rename = "verify-full")]
    VerifyFull,
    Disable,
}
