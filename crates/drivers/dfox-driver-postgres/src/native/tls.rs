//! Encrypting connections with rustls.
//!
//! `prefer` and `require` encrypt without checking the server's certificate,
//! as libpq does: they guard against eavesdropping, not impersonation.
//! `verify-full` checks the certificate chain against the system's roots and
//! the connection's CA file, and the host name against the certificate.

use std::sync::{Arc, Mutex};

use dfox_core::{
    errors::DbError,
    models::connections::{ConnectionParams, TlsMode},
};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider},
    pki_types::{pem::PemObject, CertificateDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use tokio_postgres::{config::SslMode, tls::NoTls, CancelToken, Client, Config};
use tokio_postgres_rustls::MakeRustlsConnect;

/// How a session's connections are made, kept for cancel requests too.
#[derive(Clone)]
pub enum Tls {
    Plain,
    Rustls(MakeRustlsConnect),
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

impl Tls {
    pub fn new(params: &ConnectionParams) -> Result<Self, DbError> {
        let builder = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|err| DbError::Config(format!("TLS: {err}")))?;
        let config = match params.tls {
            TlsMode::Disable => return Ok(Tls::Plain),
            TlsMode::Prefer | TlsMode::Require => builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider())))
                .with_no_client_auth(),
            TlsMode::VerifyFull => builder
                .with_root_certificates(roots(params.ca_file.as_deref())?)
                .with_no_client_auth(),
        };
        Ok(Tls::Rustls(MakeRustlsConnect::new(config)))
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

/// The system's trusted roots, plus the certificates in `ca_file`.
fn roots(ca_file: Option<&str>) -> Result<RootCertStore, DbError> {
    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        // A system certificate rustls can't use is skipped, as browsers do.
        let _ = roots.add(cert);
    }
    if let Some(path) = ca_file {
        let pem = std::fs::read(path)
            .map_err(|err| DbError::Config(format!("can't read {path}: {err}")))?;
        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&pem)
            .collect::<Result<_, _>>()
            .map_err(|err| DbError::Config(format!("{path}: {err}")))?;
        if certs.is_empty() {
            return Err(DbError::Config(format!("{path} holds no certificates")));
        }
        for cert in certs {
            roots
                .add(cert)
                .map_err(|err| DbError::Config(format!("{path}: {err}")))?;
        }
    }
    Ok(roots)
}

/// Accepts any certificate, but still checks the handshake's signatures:
/// encryption without identity, like libpq's `require`.
#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
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
    fn a_missing_or_empty_ca_file_is_an_error() {
        let err = roots(Some("/nonexistent/ca.pem")).expect_err("an error");
        assert!(err.to_string().contains("can't read"), "{err}");
        let empty = std::env::temp_dir().join(format!("dfox-empty-ca-{}.pem", std::process::id()));
        std::fs::write(&empty, "").expect("write");
        let err = roots(empty.to_str()).expect_err("an error");
        assert!(err.to_string().contains("no certificates"), "{err}");
        let _ = std::fs::remove_file(&empty);
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
