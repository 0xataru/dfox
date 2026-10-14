//! SSH tunnels: reach a database through a jump host, for any driver.
//!
//! [`Tunnel::open`] logs in to the jump host, checks its key against
//! `known_hosts`, and forwards a local port to the database. [`open`] does
//! that for a connection with [`ConnectionParams::ssh`] set, then opens the
//! driver's session through the tunnel and keeps the tunnel alive with it.
//!
//! The jump host's key must already be in `known_hosts`: an unknown key is
//! refused with the command that adds it, and a changed key is always
//! refused.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use dfox_core::{
    arrow::ResultStream,
    catalog::Catalog,
    driver::Driver,
    errors::DbError,
    models::connections::{ConnectionParams, SshAuth, SshParams},
    session::{
        ApplyReport, CancelHandle, Capabilities, ChangeSet, QueryLanguage, Request, Session,
    },
};
use futures_util::future::BoxFuture;
use russh::{
    client::{self, Handle},
    keys::{
        self, agent::client::AgentClient, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey,
        PublicKeyOrCertificate,
    },
};
use tokio::{net::TcpListener, task::JoinHandle};

/// SSH's port when none is set.
const DEFAULT_PORT: u16 = 22;

/// A forwarded port: connections to [`Tunnel::local`] reach the target
/// through the jump host. Closing happens on drop.
pub struct Tunnel {
    local: SocketAddr,
    accept: JoinHandle<()>,
    _session: Arc<Handle<HostKeys>>,
}

impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel")
            .field("local", &self.local)
            .finish()
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

impl Tunnel {
    /// Logs in to the jump host in `ssh` and forwards a local port to
    /// `target_host:target_port`, as the jump host sees them.
    pub async fn open(
        ssh: &SshParams,
        target_host: &str,
        target_port: u16,
    ) -> Result<Self, DbError> {
        let credentials = Credentials::new(ssh).await?;
        let port = ssh.port.unwrap_or(DEFAULT_PORT);
        let refusal = Arc::new(Mutex::new(None));
        let checker = HostKeys {
            host: ssh.host.clone(),
            port,
            known_hosts: ssh.known_hosts.as_ref().map(PathBuf::from),
            refusal: refusal.clone(),
        };
        let config = Arc::new(client::Config::default());
        let mut session = client::connect(config, (ssh.host.as_str(), port), checker)
            .await
            .map_err(|err| {
                let why = lock(&refusal).take();
                DbError::Connection(
                    why.unwrap_or_else(|| format!("SSH to {}:{port}: {err}", ssh.host)),
                )
            })?;
        credentials.login(&mut session, ssh).await?;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|err| DbError::Connection(format!("can't open a local port: {err}")))?;
        let local = listener
            .local_addr()
            .map_err(|err| DbError::Connection(err.to_string()))?;
        let session = Arc::new(session);
        let accept = tokio::spawn(forward(
            listener,
            session.clone(),
            target_host.to_string(),
            target_port,
        ));
        log::info!(
            "SSH tunnel to {target_host}:{target_port} via {} on {local}",
            ssh.host
        );
        Ok(Self {
            local,
            accept,
            _session: session,
        })
    }

    /// Where to connect to reach the target.
    pub fn local(&self) -> SocketAddr {
        self.local
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Accepts local connections and carries each over its own SSH channel.
async fn forward(
    listener: TcpListener,
    session: Arc<Handle<HostKeys>>,
    target_host: String,
    target_port: u16,
) {
    loop {
        let (mut socket, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                log::warn!("SSH tunnel: accepting failed: {err}");
                return;
            }
        };
        let session = session.clone();
        let target_host = target_host.clone();
        tokio::spawn(async move {
            let channel = match session
                .channel_open_direct_tcpip(
                    target_host.as_str(),
                    target_port.into(),
                    peer.ip().to_string(),
                    peer.port().into(),
                )
                .await
            {
                Ok(channel) => channel,
                Err(err) => {
                    log::warn!("SSH tunnel: can't reach {target_host}:{target_port}: {err}");
                    return;
                }
            };
            let mut stream = channel.into_stream();
            if let Err(err) = tokio::io::copy_bidirectional(&mut socket, &mut stream).await {
                log::debug!("SSH tunnel: connection closed: {err}");
            }
        });
    }
}

/// What to log in with, made ready before connecting, so a wrong passphrase
/// or a missing agent fails without bothering the server.
enum Credentials {
    Password(String),
    Key(Arc<PrivateKey>),
    Agent(Agent),
}

fn failed(ssh: &SshParams, why: impl std::fmt::Display) -> DbError {
    DbError::Connection(format!(
        "SSH login as {}@{} failed: {why}",
        ssh.user, ssh.host
    ))
}

impl Credentials {
    async fn new(ssh: &SshParams) -> Result<Self, DbError> {
        Ok(match &ssh.auth {
            SshAuth::Password { password } => Self::Password(password.clone()),
            SshAuth::Key { path, passphrase } => Self::Key(Arc::new(
                keys::load_secret_key(path, passphrase.as_deref())
                    .map_err(|err| failed(ssh, format!("{path}: {err}")))?,
            )),
            SshAuth::Agent { socket } => Self::Agent(
                connect_agent(socket.as_deref())
                    .await
                    .map_err(|err| failed(ssh, format!("no ssh-agent: {err}")))?,
            ),
        })
    }

    /// Logs in as `ssh.user`.
    async fn login(self, session: &mut Handle<HostKeys>, ssh: &SshParams) -> Result<(), DbError> {
        let user = ssh.user.as_str();
        let ok = match self {
            Self::Password(password) => session
                .authenticate_password(user, password)
                .await
                .map_err(|err| failed(ssh, err))?
                .success(),
            Self::Key(key) => {
                let hash = rsa_hash(session).await;
                session
                    .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, hash))
                    .await
                    .map_err(|err| failed(ssh, err))?
                    .success()
            }
            Self::Agent(agent) => agent_login(session, user, agent)
                .await
                .map_err(|why| failed(ssh, why))?,
        };
        if ok {
            Ok(())
        } else {
            Err(failed(ssh, "the server refused the credentials"))
        }
    }
}

#[cfg(unix)]
type Agent = AgentClient<tokio::net::UnixStream>;
#[cfg(windows)]
type Agent = AgentClient<tokio::net::windows::named_pipe::NamedPipeClient>;

/// Connects to the agent at `socket`, or to the user's own one.
#[cfg(unix)]
async fn connect_agent(socket: Option<&str>) -> Result<Agent, keys::Error> {
    match socket {
        Some(path) => AgentClient::connect_uds(path).await,
        None => AgentClient::connect_env().await,
    }
}

/// Connects to the agent at the named pipe `socket`, or to OpenSSH's.
#[cfg(windows)]
async fn connect_agent(socket: Option<&str>) -> Result<Agent, keys::Error> {
    AgentClient::connect_named_pipe(socket.unwrap_or(r"\\.\pipe\openssh-ssh-agent")).await
}

/// Tries each of the agent's keys until the server takes one.
async fn agent_login<S>(
    session: &mut Handle<HostKeys>,
    user: &str,
    mut agent: AgentClient<S>,
) -> Result<bool, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let identities = agent
        .request_identities()
        .await
        .map_err(|err| format!("ssh-agent: {err}"))?;
    let hash = rsa_hash(session).await;
    for identity in identities {
        let keys::agent::AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let result = session
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
            .map_err(|err| err.to_string())?;
        if result.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The hash to sign with when the key is RSA.
async fn rsa_hash(session: &Handle<HostKeys>) -> Option<HashAlg> {
    session
        .best_supported_rsa_hash()
        .await
        .ok()
        .flatten()
        .flatten()
}

/// Checks the jump host's key against `known_hosts`, and says why it was
/// refused.
struct HostKeys {
    host: String,
    port: u16,
    known_hosts: Option<PathBuf>,
    refusal: Arc<Mutex<Option<String>>>,
}

impl HostKeys {
    fn check(&self, key: &PublicKey) -> Result<bool, String> {
        let result = match &self.known_hosts {
            Some(path) => keys::check_known_hosts_path(&self.host, self.port, key, path),
            None => keys::check_known_hosts(&self.host, self.port, key),
        };
        let fingerprint = key.fingerprint(HashAlg::Sha256);
        let place = match self.port {
            DEFAULT_PORT => self.host.clone(),
            port => format!("[{}]:{port}", self.host),
        };
        match result {
            Ok(true) => Ok(true),
            Ok(false) => Err(format!(
                "the SSH host key of {place} is not in known_hosts ({fingerprint}). \
                 If it's the right key, add it with: ssh-keyscan -p {} {} >> ~/.ssh/known_hosts",
                self.port, self.host
            )),
            Err(keys::Error::KeyChanged { line }) => Err(format!(
                "the SSH host key of {place} has changed since it was recorded \
                 (known_hosts line {line}); now {fingerprint}. Refusing to connect"
            )),
            Err(err) => Err(format!("can't check the SSH host key of {place}: {err}")),
        }
    }
}

impl client::Handler for HostKeys {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_public_key else {
            *lock(&self.refusal) =
                Some("SSH host certificates aren't supported; use a plain host key".into());
            return Ok(false);
        };
        match self.check(key) {
            Ok(trusted) => Ok(trusted),
            Err(why) => {
                *lock(&self.refusal) = Some(why);
                Ok(false)
            }
        }
    }
}

/// Opens `driver`'s session for `params`, through an SSH tunnel when
/// `params.ssh` is set. The tunnel lives as long as the session.
pub async fn open(
    driver: &dyn Driver,
    params: &ConnectionParams,
) -> Result<Box<dyn Session>, DbError> {
    let Some(ssh) = &params.ssh else {
        return driver.open(params).await;
    };
    let tunnel = Tunnel::open(ssh, &params.host, params.port).await?;
    let through = ConnectionParams {
        tunnel: Some(tunnel.local()),
        ssh: None,
        ..params.clone()
    };
    let session = driver.open(&through).await?;
    Ok(Box::new(Tunneled {
        session,
        _tunnel: tunnel,
    }))
}

/// A session and the tunnel it runs through.
struct Tunneled {
    session: Box<dyn Session>,
    _tunnel: Tunnel,
}

impl Session for Tunneled {
    fn caps(&self) -> Capabilities {
        self.session.caps()
    }

    fn language(&self) -> QueryLanguage {
        self.session.language()
    }

    fn catalog(&mut self) -> &mut dyn Catalog {
        self.session.catalog()
    }

    fn run(&mut self, request: Request) -> BoxFuture<'_, Result<ResultStream, DbError>> {
        self.session.run(request)
    }

    fn cancel_handle(&self) -> CancelHandle {
        self.session.cancel_handle()
    }

    fn apply(&mut self, changes: ChangeSet) -> BoxFuture<'_, Result<ApplyReport, DbError>> {
        self.session.apply(changes)
    }
}
