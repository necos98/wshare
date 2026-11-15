//! rustls configuration for both sides, plus the certificate verifiers.
//!
//! Only TLS 1.3 (required by QUIC) with the `ring` provider is used; the
//! provider is passed explicitly instead of relying on a process-wide
//! default, so it does not matter what other crates install.
//!
//! Both peers use self-signed certificates and are identified by their
//! fingerprint, never by a CA chain: the viewer pins (or learns) the host's
//! with [`FingerprintVerifier`]; the host accepts any viewer certificate
//! with [`AnyClientCert`] and leaves the decision to the application, which
//! reads the proven fingerprint from the connection.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::identity::{Fingerprint, Identity};

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// TLS server configuration presenting `identity`. Viewers *may* present a
/// certificate (see [`AnyClientCert`]); those that do not still reach the
/// control channel and are admitted with a PIN.
pub(crate) fn server_config(identity: &Identity) -> Result<rustls::ServerConfig> {
    let provider = provider();
    let verifier = AnyClientCert::new(&provider);
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("selecting TLS 1.3")?
        .with_client_cert_verifier(Arc::new(verifier))
        .with_single_cert(vec![identity.cert_der().clone()], identity.key_der())
        .context("loading host certificate/key")?;
    config.alpn_protocols = vec![crate::ALPN.to_vec()];
    Ok(config)
}

/// TLS client configuration that verifies the host with `verifier` and, when
/// `identity` is given, presents it as the client certificate.
pub(crate) fn client_config(
    verifier: Arc<FingerprintVerifier>,
    identity: Option<&Identity>,
) -> Result<rustls::ClientConfig> {
    let builder = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("selecting TLS 1.3")?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let mut config = match identity {
        Some(identity) => builder
            .with_client_auth_cert(vec![identity.cert_der().clone()], identity.key_der())
            .context("loading viewer certificate/key")?,
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = vec![crate::ALPN.to_vec()];
    Ok(config)
}

/// Checks that an identity's key is loadable and matches its certificate.
pub(crate) fn check_identity(identity: &Identity) -> Result<()> {
    server_config(identity).map(drop)
}

/// Verifies handshake signatures against the presented certificate, so a
/// peer must hold the private key of the certificate whose fingerprint we
/// accept. Shared by both verifiers.
#[derive(Debug)]
struct SignatureCheck {
    algorithms: WebPkiSupportedAlgorithms,
}

impl SignatureCheck {
    fn new(provider: &CryptoProvider) -> Self {
        Self {
            algorithms: provider.signature_verification_algorithms,
        }
    }

    fn tls12(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn tls13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Accepts the host certificate based on its SHA-256 fingerprint instead of
/// a CA chain.
///
/// - `expected = Some(fp)`: pinning; any other certificate aborts the
///   handshake, and [`Self::mismatch`] then tells which one was presented.
/// - `expected = None`: trust on first use; any certificate is accepted and
///   the caller reads the fingerprint from the established connection.
///
/// Either way the handshake signature is still verified against the
/// presented certificate.
#[derive(Debug)]
pub(crate) struct FingerprintVerifier {
    expected: Option<Fingerprint>,
    /// Fingerprint of a rejected certificate (pinning only).
    mismatch: OnceLock<Fingerprint>,
    signatures: SignatureCheck,
}

impl FingerprintVerifier {
    pub(crate) fn new(expected: Option<Fingerprint>) -> Self {
        Self {
            expected,
            mismatch: OnceLock::new(),
            signatures: SignatureCheck::new(&provider()),
        }
    }

    /// The certificate the host presented instead of the pinned one, if the
    /// handshake failed for that reason.
    pub(crate) fn mismatch(&self) -> Option<Fingerprint> {
        self.mismatch.get().copied()
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
        let actual = Fingerprint::of_cert(end_entity);
        match self.expected {
            Some(expected) if actual != expected => {
                let _ = self.mismatch.set(actual);
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
        self.signatures.tls12(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.signatures.tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.signatures.schemes()
    }
}

/// Host side: asks every viewer for a certificate but does not require one,
/// and accepts any self-signed certificate. Whether that certificate is
/// trusted is the application's decision, based on the fingerprint read
/// from the connection; the handshake signature is still verified, so that
/// fingerprint is proven (the viewer holds its private key).
#[derive(Debug)]
pub(crate) struct AnyClientCert {
    signatures: SignatureCheck,
}

impl AnyClientCert {
    pub(crate) fn new(provider: &CryptoProvider) -> Self {
        Self {
            signatures: SignatureCheck::new(provider),
        }
    }
}

impl ClientCertVerifier for AnyClientCert {
    fn offer_client_auth(&self) -> bool {
        true
    }

    /// Optional, so a viewer without a certificate (e.g. an older version)
    /// gets a readable rejection on the control channel instead of a TLS
    /// error.
    fn client_auth_mandatory(&self) -> bool {
        false
    }

    /// No CA hints: a viewer then always presents its certificate.
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.signatures.tls12(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.signatures.tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.signatures.schemes()
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
        let verifier = FingerprintVerifier::new(Some(host.fingerprint()));
        assert!(verify(&verifier, &host));
        assert_eq!(verifier.mismatch(), None);
        assert!(!verify(&verifier, &other));
        assert_eq!(verifier.mismatch(), Some(other.fingerprint()));
    }

    #[test]
    fn tofu_verifier_accepts_any_cert() {
        let verifier = FingerprintVerifier::new(None);
        assert!(verify(&verifier, &Identity::generate("a").unwrap()));
        assert!(verify(&verifier, &Identity::generate("b").unwrap()));
        assert_eq!(verifier.mismatch(), None);
    }

    #[test]
    fn verifiers_advertise_signature_schemes() {
        let schemes = FingerprintVerifier::new(None).supported_verify_schemes();
        assert!(schemes.contains(&SignatureScheme::ECDSA_NISTP256_SHA256));
        let schemes = AnyClientCert::new(&provider()).supported_verify_schemes();
        assert!(schemes.contains(&SignatureScheme::ECDSA_NISTP256_SHA256));
    }

    #[test]
    fn client_certificates_are_requested_but_optional() {
        let verifier = AnyClientCert::new(&provider());
        assert!(verifier.offer_client_auth());
        assert!(!verifier.client_auth_mandatory());
        assert!(verifier.root_hint_subjects().is_empty());
        let viewer = Identity::generate("viewer").unwrap();
        assert!(
            verifier
                .verify_client_cert(viewer.cert_der(), &[], UnixTime::now())
                .is_ok()
        );
    }

    #[test]
    fn configs_build_with_alpn() {
        let host = Identity::generate("host").unwrap();
        assert_eq!(server_config(&host).unwrap().alpn_protocols, [crate::ALPN]);
        for (identity, presents_cert) in [(None, false), (Some(&host), true)] {
            let verifier = Arc::new(FingerprintVerifier::new(None));
            let config = client_config(verifier, identity).unwrap();
            assert_eq!(config.alpn_protocols, [crate::ALPN]);
            assert_eq!(config.client_auth_cert_resolver.has_certs(), presents_cert);
        }
    }
}
