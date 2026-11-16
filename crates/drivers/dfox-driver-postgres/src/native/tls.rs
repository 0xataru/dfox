//! Encrypting connections with rustls, as `dfox_core::tls` sets it up.

use std::sync::{Arc, Mutex};

use dfox_core::{
    errors::DbError,
    models::connections::{ConnectionParams, TlsMode},
};
use tokio_postgres::{config::SslMode, tls::NoTls, CancelToken, Client, Config};
use tokio_postgres_rustls::MakeRustlsConnect;

/// How a session's connections are made, kept for cancel requests too.
#[derive(Clone)]
pub enum Tls {
    Plain,
    Rustls(MakeRustlsConnect),
}

impl Tls {
    pub fn new(params: &ConnectionParams) -> Result<Self, DbError> {
        Ok(match dfox_core::tls::client_config(params, None)? {
            Some(config) => Tls::Rustls(MakeRustlsConnect::new(config)),
            None => Tls::Plain,
        })
    }

    /// Connects, and drives the connection on its own task, sending notices
    /// to `notices` when given.
    pub async fn connect(
        &self,
        config: &Config,
        notices: Option<Arc<Mutex<Vec<String>>>>,
    ) -> Result<Client, DbError> {
        let mut config = config.clone();
        match self {
            Tls::Plain => {
                config.ssl_mode(SslMode::Disable);
                let (client, connection) = config.connect(NoTls).await.map_err(super::db_error)?;
                tokio::spawn(super::drive(connection, notices));
                Ok(client)
            }
            Tls::Rustls(tls) => {
                let (client, connection) =
                    config.connect(tls.clone()).await.map_err(super::db_error)?;
                tokio::spawn(super::drive(connection, notices));
                Ok(client)
            }
        }
    }

    /// Asks the server to stop what `token`'s connection is running.
    pub async fn cancel(&self, token: &CancelToken) -> Result<(), DbError> {
        match self {
            Tls::Plain => token.cancel_query(NoTls).await,
            Tls::Rustls(tls) => token.cancel_query(tls.clone()).await,
        }
        .map_err(super::db_error)
    }
}

/// The SSL mode a [`TlsMode`] asks the server for.
pub fn ssl_mode(mode: TlsMode) -> SslMode {
    match mode {
        TlsMode::Prefer => SslMode::Prefer,
        TlsMode::Require | TlsMode::VerifyFull => SslMode::Require,
        TlsMode::Disable => SslMode::Disable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_map_to_ssl_modes() {
        assert!(matches!(ssl_mode(TlsMode::Prefer), SslMode::Prefer));
        assert!(matches!(ssl_mode(TlsMode::VerifyFull), SslMode::Require));
        assert!(matches!(ssl_mode(TlsMode::Disable), SslMode::Disable));
    }

    #[test]
    fn every_mode_builds_a_config() {
        for tls in [
            TlsMode::Prefer,
            TlsMode::Require,
            TlsMode::VerifyFull,
            TlsMode::Disable,
        ] {
            let params = ConnectionParams {
                tls,
                ..Default::default()
            };
            let made = Tls::new(&params).expect("tls");
            assert_eq!(matches!(made, Tls::Plain), tls == TlsMode::Disable);
        }
    }
}
