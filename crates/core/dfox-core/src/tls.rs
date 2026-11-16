//! TLS for drivers that encrypt with rustls.
//!
//! `prefer` and `require` encrypt without checking the server's
//! certificate, as libpq does: they guard against eavesdropping, not
//! impersonation. `verify-full` checks the certificate chain against the
//! system's roots and the connection's CA file, and the host name against
//! the certificate.

use std::sync::Arc;

use rustls::{
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        WebPkiServerVerifier,
    },
    crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider},
    pki_types::{pem::PemObject, CertificateDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};

use crate::{
    errors::DbError,
    models::connections::{ConnectionParams, TlsMode},
};

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The rustls client for `params`' TLS mode, `None` for `disable`. With
/// `name`, `verify-full` checks the certificate against it rather than the
/// address connected to: for drivers that connect by IP address.
pub fn client_config(
    params: &ConnectionParams,
    name: Option<&str>,
) -> Result<Option<ClientConfig>, DbError> {
    let builder = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|err| DbError::Config(format!("TLS: {err}")))?;
    Ok(Some(match params.tls {
        TlsMode::Disable => return Ok(None),
        TlsMode::Prefer | TlsMode::Require => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider())))
            .with_no_client_auth(),
        TlsMode::VerifyFull => {
            let roots = roots(params.ca_file.as_deref())?;
            match name {
                None => builder.with_root_certificates(roots).with_no_client_auth(),
                Some(name) => {
                    let name = ServerName::try_from(name.to_string())
                        .map_err(|err| DbError::Config(format!("TLS: {name}: {err}")))?;
                    let inner =
                        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
                            .build()
                            .map_err(|err| DbError::Config(format!("TLS: {err}")))?;
                    builder
                        .dangerous()
                        .with_custom_certificate_verifier(Arc::new(Named { inner, name }))
                        .with_no_client_auth()
                }
            }
        }
    }))
}

/// The system's trusted roots, plus the certificates in `ca_file`.
pub fn roots(ca_file: Option<&str>) -> Result<RootCertStore, DbError> {
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

/// Checks the certificate as [`WebPkiServerVerifier`] does, against a name
/// of its own instead of the one connected to.
#[derive(Debug)]
struct Named {
    inner: Arc<WebPkiServerVerifier>,
    name: ServerName<'static>,
}

impl ServerCertVerifier for Named {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.inner
            .verify_server_cert(end_entity, intermediates, &self.name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            for name in [None, Some("db.example.com")] {
                let made = client_config(&params, name).expect("config");
                assert_eq!(made.is_none(), tls == TlsMode::Disable);
            }
        }
    }
}
