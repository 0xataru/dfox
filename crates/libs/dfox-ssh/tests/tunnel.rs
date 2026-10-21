//! Tunnels through a real jump host, started by `scripts/ssh-server.sh`:
//! every way of logging in, host keys that are unknown or changed, and a
//! Postgres session that queries and cancels through the tunnel, and a
//! MySQL one over TLS.
//!
//! `DFOX_TEST_SSH_DIR` names the script's `target/ssh`; the jump host
//! reaches the test Postgres as `host.docker.internal`.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use dfox_core::{
    arrow::{FetchStatus, Fetcher, Overflow, DEFAULT_BUDGET},
    errors::DbError,
    models::connections::{ConnectionParams, SshAuth, SshParams},
    result::ResultSet,
    session::{Request, Session},
};
use dfox_driver_postgres::PostgresDriver;
use dfox_ssh::Tunnel;
use dfox_testkit::{params_from_url, server_url};

const PORT: u16 = 12222;
/// How the jump host reaches the machine running the tests.
const HOST_FROM_JUMP: &str = "host.docker.internal";

struct Setup {
    dir: PathBuf,
    postgres: ConnectionParams,
}

fn setup() -> Option<Setup> {
    let dir = PathBuf::from(server_url("DFOX_TEST_SSH_DIR")?);
    let postgres = params_from_url(&server_url("DFOX_TEST_POSTGRES_URL")?);
    Some(Setup { dir, postgres })
}

impl Setup {
    fn ssh(&self, auth: SshAuth) -> SshParams {
        SshParams {
            host: "127.0.0.1".into(),
            port: Some(PORT),
            user: "dfox".into(),
            auth,
            known_hosts: Some(self.path("known_hosts")),
        }
    }

    fn key(&self, name: &str) -> SshAuth {
        SshAuth::Key {
            path: self.path(name),
            passphrase: None,
        }
    }

    fn path(&self, name: &str) -> String {
        self.dir.join(name).to_string_lossy().into_owned()
    }

    /// The test Postgres, through the jump host.
    fn postgres_via(&self, ssh: SshParams) -> ConnectionParams {
        ConnectionParams {
            host: HOST_FROM_JUMP.into(),
            ssh: Some(ssh),
            ..self.postgres.clone()
        }
    }
}

async fn tunnel(setup: &Setup, ssh: &SshParams) -> Result<Tunnel, DbError> {
    Tunnel::open(ssh, HOST_FROM_JUMP, setup.postgres.port).await
}

/// Runs `sql` and returns the first cell as text.
async fn scalar(session: &mut dyn Session, sql: &str) -> String {
    let stream = session.run(Request::new(sql)).await.expect("run");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    let status = fetcher
        .fetch()
        .await
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
    assert_eq!(status, FetchStatus::Done);
    let result = fetcher.result().expect("a result");
    result.cell(0, 0).expect("a cell").to_string()
}

async fn select_one(params: &ConnectionParams) {
    let mut session = dfox_ssh::open(&PostgresDriver, params)
        .await
        .expect("open through the tunnel");
    assert_eq!(scalar(session.as_mut(), "SELECT 1 + 1").await, "2");
}

#[tokio::test]
async fn a_key_logs_in_and_postgres_answers_through_the_tunnel() {
    let Some(setup) = setup() else { return };
    let params = setup.postgres_via(setup.ssh(setup.key("id_ed25519")));
    select_one(&params).await;
}

#[tokio::test]
async fn a_key_with_a_passphrase_needs_it() {
    let Some(setup) = setup() else { return };
    let with = |passphrase: Option<&str>| {
        setup.ssh(SshAuth::Key {
            path: setup.path("id_passphrase"),
            passphrase: passphrase.map(str::to_string),
        })
    };
    let err = tunnel(&setup, &with(None))
        .await
        .expect_err("no passphrase");
    assert!(err.to_string().contains("id_passphrase"), "{err}");
    let err = tunnel(&setup, &with(Some("wrong")))
        .await
        .expect_err("a wrong passphrase");
    assert!(err.to_string().contains("id_passphrase"), "{err}");
    select_one(&setup.postgres_via(with(Some("dfox")))).await;
}

#[tokio::test]
async fn a_password_logs_in() {
    let Some(setup) = setup() else { return };
    let password = |password: &str| {
        setup.ssh(SshAuth::Password {
            password: password.into(),
        })
    };
    let err = tunnel(&setup, &password("wrong"))
        .await
        .expect_err("a wrong password");
    assert!(err.to_string().contains("refused the credentials"), "{err}");
    select_one(&setup.postgres_via(password("dfox"))).await;
}

#[tokio::test]
async fn a_key_the_server_does_not_know_is_refused() {
    let Some(setup) = setup() else { return };
    let err = tunnel(&setup, &setup.ssh(setup.key("id_stranger")))
        .await
        .expect_err("refused");
    let message = err.to_string();
    assert!(message.contains("dfox@127.0.0.1"), "{message}");
    assert!(message.contains("refused the credentials"), "{message}");
}

#[tokio::test]
async fn an_unknown_host_key_is_refused_with_the_command_that_adds_it() {
    let Some(setup) = setup() else { return };
    let empty = setup
        .dir
        .join(format!("empty_known_hosts_{}", std::process::id()));
    std::fs::write(&empty, "").expect("write");
    let ssh = SshParams {
        known_hosts: Some(empty.to_string_lossy().into_owned()),
        ..setup.ssh(setup.key("id_ed25519"))
    };
    let err = tunnel(&setup, &ssh).await.expect_err("refused");
    let _ = std::fs::remove_file(&empty);
    let message = err.to_string();
    assert!(message.contains("not in known_hosts"), "{message}");
    assert!(message.contains("SHA256:"), "{message}");
    assert!(
        message.contains("ssh-keyscan -p 12222 127.0.0.1"),
        "{message}"
    );
}

#[tokio::test]
async fn a_changed_host_key_is_refused() {
    let Some(setup) = setup() else { return };
    let ssh = SshParams {
        known_hosts: Some(setup.path("other_known_hosts")),
        ..setup.ssh(setup.key("id_ed25519"))
    };
    let err = tunnel(&setup, &ssh).await.expect_err("refused");
    let message = err.to_string();
    assert!(message.contains("has changed"), "{message}");
    assert!(message.contains("[127.0.0.1]:12222"), "{message}");
}

#[cfg(unix)]
use std::{
    path::Path,
    process::{Child, Command},
};

/// A private agent on its own socket, so the user's agent is never touched.
#[cfg(unix)]
struct Agent {
    child: Child,
    socket: PathBuf,
}

#[cfg(unix)]
impl Agent {
    fn start(dir: &Path, key: &str) -> Self {
        let socket = dir.join(format!("agent_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let child = Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&socket)
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("start ssh-agent");
        let agent = Self { child, socket };
        let started = Instant::now();
        while !agent.socket.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no agent socket"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let added = Command::new("ssh-add")
            .arg(dir.join(key))
            .env("SSH_AUTH_SOCK", &agent.socket)
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run ssh-add");
        assert!(added.success(), "ssh-add failed");
        agent
    }
}

#[cfg(unix)]
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_agent_logs_in() {
    let Some(setup) = setup() else { return };
    let agent = Agent::start(&setup.dir, "id_ed25519");
    let auth = |socket: &Path| SshAuth::Agent {
        socket: Some(socket.to_string_lossy().into_owned()),
    };
    select_one(&setup.postgres_via(setup.ssh(auth(&agent.socket)))).await;
    drop(agent);

    let stranger = Agent::start(&setup.dir, "id_stranger");
    let err = tunnel(&setup, &setup.ssh(auth(&stranger.socket)))
        .await
        .expect_err("refused");
    assert!(err.to_string().contains("refused the credentials"), "{err}");
}

#[tokio::test]
async fn cancel_reaches_the_server_through_the_tunnel() {
    use futures_util::StreamExt;

    let Some(setup) = setup() else { return };
    let params = setup.postgres_via(setup.ssh(setup.key("id_ed25519")));
    let mut session = dfox_ssh::open(&PostgresDriver, &params)
        .await
        .expect("open");
    let handle = session.cancel_handle();
    let mut frames = session
        .run(Request::new("SELECT pg_sleep(30)"))
        .await
        .expect("run");
    let cancelled = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let asked = Instant::now();
        handle.cancel().await.expect("cancel");
        asked
    });
    let mut last = None;
    while let Some(frame) = frames.next().await {
        last = Some(frame);
    }
    let took = cancelled.await.expect("join").elapsed();
    assert!(matches!(last, Some(Err(DbError::Cancelled))), "{last:?}");
    eprintln!("cancelled through the tunnel in {took:?}");
    assert!(took < Duration::from_secs(1), "{took:?}");
    drop(frames);
    // The session still works after the cancel.
    assert_eq!(scalar(session.as_mut(), "SELECT 'ok'").await, "ok");
}

#[tokio::test]
async fn without_ssh_open_connects_directly() {
    let Some(setup) = setup() else { return };
    select_one(&setup.postgres).await;
}

/// MySQL through the tunnel, encrypted: the session connects to the
/// tunnel's end and still speaks TLS to the server behind it.
#[tokio::test]
async fn mysql_answers_through_the_tunnel_over_tls() {
    use dfox_core::models::connections::TlsMode;

    let Some(setup) = setup() else { return };
    let Some(url) = server_url("DFOX_TEST_MYSQL_URL") else {
        return;
    };
    let params = ConnectionParams {
        host: HOST_FROM_JUMP.into(),
        tls: TlsMode::Require,
        ssh: Some(setup.ssh(setup.key("id_ed25519"))),
        ..params_from_url(&url)
    };
    let mut session = dfox_ssh::open(&dfox_driver_mysql::MySqlDriver, &params)
        .await
        .expect("open through the tunnel");
    let cipher = scalar(
        session.as_mut(),
        "SELECT VARIABLE_VALUE FROM performance_schema.session_status \
         WHERE VARIABLE_NAME = 'Ssl_cipher'",
    )
    .await;
    assert!(!cipher.is_empty(), "the connection is encrypted");
}
