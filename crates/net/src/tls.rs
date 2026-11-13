//! rustls configuration for both sides, plus the viewer's certificate
//! verifier.
//!
//! Only TLS 1.3 (required by QUIC) with the `ring` provider is used; the
//! provider is passed explicitly instead of relying on a process-wide
//! default, so it does not matter what other crates install.

use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use crate::identity::{Fingerprint, Identity};

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// TLS server configuration presenting `identity`, no client auth (viewers
/// authenticate with a PIN inside the control channel).
pub(crate) fn server_config(identity: &Identity) -> Result<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("selecting TLS 1.3")?
        .with_no_client_auth()
        .with_single_cert(vec![identity.cert_der().clone()], identity.key_der())
        .context("loading host certificate/key")?;
    config.alpn_protocols = vec![crate::ALPN.to_vec()];
    Ok(config)
}

/// TLS client configuration that verifies the host with [`FingerprintVerifier`].
pub(crate) fn client_config(expected: Option<Fingerprint>) -> Result<rustls::ClientConfig> {
    let provider = provider();
    let verifier = FingerprintVerifier::new(expected, &provider);
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("selecting TLS 1.3")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![crate::ALPN.to_vec()];
    Ok(config)
}

/// Checks that an identity's key is loadable and matches its certificate.
pub(crate) fn check_identity(identity: &Identity) -> Result<()> {
    server_config(identity).map(drop)
}

/// Accepts the host certificate based on its SHA-256 fingerprint instead of
/// a CA chain.
///
/// - `expected = Some(fp)`: pinning; any other certificate aborts the
///   handshake.
/// - `expected = None`: trust on first use; any certificate is accepted and
///   the caller reads the fingerprint from the established connection.
///
/// Either way the handshake signature is still verified against the
/// presented certificate, so the peer must actually hold the private key of
/// the certificate whose fingerprint we accepted.
#[derive(Debug)]
pub(crate) struct FingerprintVerifier {
    expected: Option<Fingerprint>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl FingerprintVerifier {
    pub(crate) fn new(expected: Option<Fingerprint>, provider: &CryptoProvider) -> Self {
        Self {
            expected,
            algorithms: provider.signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.expected {
            Some(expected) if Fingerprint::of_cert(end_entity) != expected => {
                Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::ApplicationVerificationFailure,
                ))
            }
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verify(verifier: &FingerprintVerifier, identity: &Identity) -> bool {
        verifier
            .verify_server_cert(
                identity.cert_der(),
                &[],
                &ServerName::try_from(crate::SERVER_NAME).unwrap(),
                &[],
                UnixTime::now(),
            )
            .is_ok()
    }

    #[test]
    fn pinned_verifier_accepts_only_matching_cert() {
        let host = Identity::generate("host").unwrap();
        let other = Identity::generate("other").unwrap();
        let verifier = FingerprintVerifier::new(Some(host.fingerprint()), &provider());
        assert!(verify(&verifier, &host));
        assert!(!verify(&verifier, &other));
    }

    #[test]
    fn tofu_verifier_accepts_any_cert() {
        let verifier = FingerprintVerifier::new(None, &provider());
        assert!(verify(&verifier, &Identity::generate("a").unwrap()));
        assert!(verify(&verifier, &Identity::generate("b").unwrap()));
    }

    #[test]
    fn verifier_advertises_signature_schemes() {
        let verifier = FingerprintVerifier::new(None, &provider());
        assert!(
            verifier
                .supported_verify_schemes()
                .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
        );
    }

    #[test]
    fn configs_build_with_alpn() {
        let host = Identity::generate("host").unwrap();
        assert_eq!(server_config(&host).unwrap().alpn_protocols, [crate::ALPN]);
        assert_eq!(client_config(None).unwrap().alpn_protocols, [crate::ALPN]);
    }
}
