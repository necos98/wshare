//! Host identity: a self-signed certificate and its private key.
//!
//! The certificate is never validated against a CA; viewers identify a host
//! by the SHA-256 fingerprint of its certificate DER. Persisting the identity
//! keeps that fingerprint stable across restarts so a pinned/TOFU viewer
//! keeps trusting the same machine.

use std::fmt;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

const CERT_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.der";

/// SHA-256 of a certificate's DER encoding. `Display` / `FromStr` use
/// lowercase hex (64 characters; uppercase is accepted when parsing).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    /// Fingerprint of a DER-encoded certificate.
    pub fn of_cert(cert_der: &[u8]) -> Self {
        let digest = ring::digest::digest(&ring::digest::SHA256, cert_der);
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        Self(out)
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for Fingerprint {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.len() != 64 || !s.is_ascii() {
            bail!("fingerprint must be 64 hex characters, got {s:?}");
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)
                .with_context(|| format!("invalid hex in fingerprint {s:?}"))?;
        }
        Ok(Self(out))
    }
}

/// Self-signed certificate + PKCS#8 private key, both DER.
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

impl Identity {
    /// Generates a fresh ECDSA P-256 identity. `name` (e.g. the machine
    /// name) goes into the certificate's common name purely as a label; the
    /// DNS SAN is the fixed name the viewer sends as SNI, so `name` may
    /// contain any Unicode.
    pub fn generate(name: &str) -> Result<Self> {
        let key_pair = rcgen::KeyPair::generate().context("generating key pair")?;
        let mut params = rcgen::CertificateParams::new(vec![crate::SERVER_NAME.to_owned()])
            .context("building certificate parameters")?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name);
        let cert = params
            .self_signed(&key_pair)
            .context("self-signing certificate")?;
        Ok(Self {
            cert: cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(key_pair.serialize_der()),
        })
    }

    /// Loads `cert.der` + `key.der` from `dir`, or generates a new identity
    /// and writes them there (creating `dir`) when either file is missing.
    /// Corrupt files are an error rather than silently replaced, because a
    /// new identity changes the fingerprint viewers have pinned.
    ///
    /// The key file gets the directory's default permissions; callers should
    /// pick a per-user directory (e.g. under `%LOCALAPPDATA%`).
    pub fn load_or_create(dir: &Path, name: &str) -> Result<Self> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        if cert_path.is_file() && key_path.is_file() {
            let cert =
                fs::read(&cert_path).with_context(|| format!("reading {}", cert_path.display()))?;
            let key =
                fs::read(&key_path).with_context(|| format!("reading {}", key_path.display()))?;
            let identity = Self {
                cert: CertificateDer::from(cert),
                key: PrivatePkcs8KeyDer::from(key),
            };
            crate::tls::check_identity(&identity)
                .with_context(|| format!("invalid identity in {}", dir.display()))?;
            return Ok(identity);
        }

        let identity = Self::generate(name)?;
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::write(&key_path, identity.key.secret_pkcs8_der())
            .with_context(|| format!("writing {}", key_path.display()))?;
        fs::write(&cert_path, identity.cert.as_ref())
            .with_context(|| format!("writing {}", cert_path.display()))?;
        Ok(identity)
    }

    /// Fingerprint viewers use to recognize this host.
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of_cert(&self.cert)
    }

    /// Certificate DER.
    pub fn cert_der(&self) -> &CertificateDer<'static> {
        &self.cert
    }

    pub(crate) fn key_der(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.key.clone_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_hex_roundtrip() {
        let fp = Fingerprint(core::array::from_fn(|i| (i * 7) as u8));
        let text = fp.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(text, text.to_lowercase());
        assert_eq!(text.parse::<Fingerprint>().unwrap(), fp);
        assert_eq!(text.to_uppercase().parse::<Fingerprint>().unwrap(), fp);
    }

    #[test]
    fn fingerprint_rejects_bad_input() {
        assert!("abc".parse::<Fingerprint>().is_err());
        assert!("zz".repeat(32).parse::<Fingerprint>().is_err());
        assert!("\u{e9}".repeat(32).parse::<Fingerprint>().is_err());
    }

    #[test]
    fn fingerprint_is_sha256_of_cert() {
        let id = Identity::generate("test").unwrap();
        assert_eq!(id.fingerprint(), Fingerprint::of_cert(id.cert_der()));
        let other = Identity::generate("test").unwrap();
        assert_ne!(id.fingerprint(), other.fingerprint());
    }

    #[test]
    fn generate_accepts_non_dns_names() {
        Identity::generate("Jacob's PC \u{2014} salotto").unwrap();
    }

    #[test]
    fn load_or_create_persists() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nested").join("identity");
        let first = Identity::load_or_create(&sub, "host").unwrap();
        assert!(sub.join(CERT_FILE).is_file());
        assert!(sub.join(KEY_FILE).is_file());
        let second = Identity::load_or_create(&sub, "host").unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn load_rejects_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        Identity::load_or_create(dir.path(), "host").unwrap();
        fs::write(dir.path().join(KEY_FILE), b"garbage").unwrap();
        assert!(Identity::load_or_create(dir.path(), "host").is_err());
    }
}
