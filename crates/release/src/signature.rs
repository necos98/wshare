//! Ed25519 key pairs, signatures and their verification (`ring`).
//!
//! Text forms, all hex: the private key is its PKCS#8 document (what the
//! `WSHARE_UPDATE_KEY` secret holds), the public key its 32 bytes and a
//! signature its 64 bytes.

use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use thiserror::Error;

use crate::hex;

const PUBLIC_KEY_LEN: usize = 32;
const SIGNATURE_LEN: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SignatureError {
    #[error("the private key is not a hex PKCS#8 Ed25519 key")]
    BadPrivateKey,
    #[error("the public key is not 32 bytes of hex")]
    BadPublicKey,
    #[error("the signature is not 64 bytes of hex")]
    Malformed,
    #[error("the signature does not match: the file was not signed by wshare's key")]
    Mismatch,
    #[error("generating the key failed")]
    Generation,
}

/// A public key, parsed from its hex form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey([u8; PUBLIC_KEY_LEN]);

impl PublicKey {
    pub fn from_hex(text: &str) -> Result<Self, SignatureError> {
        hex::decode(text)
            .and_then(|bytes| <[u8; PUBLIC_KEY_LEN]>::try_from(bytes).ok())
            .map(Self)
            .ok_or(SignatureError::BadPublicKey)
    }

    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }
}

/// A signing key (the CI side).
pub struct KeyPair(Ed25519KeyPair);

impl KeyPair {
    /// A new random key pair and its private key as hex, to store.
    pub fn generate() -> Result<(Self, String), SignatureError> {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| SignatureError::Generation)?;
        let private = hex::encode(document.as_ref());
        Ok((Self::from_hex(&private)?, private))
    }

    pub fn from_hex(private: &str) -> Result<Self, SignatureError> {
        let document = hex::decode(private).ok_or(SignatureError::BadPrivateKey)?;
        Ed25519KeyPair::from_pkcs8(&document)
            .map(Self)
            .map_err(|_| SignatureError::BadPrivateKey)
    }

    pub fn public_key(&self) -> PublicKey {
        let mut bytes = [0; PUBLIC_KEY_LEN];
        bytes.copy_from_slice(self.0.public_key().as_ref());
        PublicKey(bytes)
    }

    /// The signature of `message`, as hex.
    pub fn sign(&self, message: &[u8]) -> String {
        hex::encode(self.0.sign(message).as_ref())
    }
}

/// Checks that `signature` (hex) is `key`'s signature of `message`.
pub fn verify(key: &PublicKey, message: &[u8], signature: &str) -> Result<(), SignatureError> {
    let signature = hex::decode(signature)
        .filter(|bytes| bytes.len() == SIGNATURE_LEN)
        .ok_or(SignatureError::Malformed)?;
    UnparsedPublicKey::new(&ED25519, &key.0)
        .verify(message, &signature)
        .map_err(|_| SignatureError::Mismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_verifies_with_its_public_key() {
        let (pair, private) = KeyPair::generate().unwrap();
        let signature = pair.sign(b"latest.json");
        verify(&pair.public_key(), b"latest.json", &signature).unwrap();
        // The stored private key signs the same way.
        let again = KeyPair::from_hex(&private).unwrap();
        assert_eq!(again.public_key(), pair.public_key());
    }

    #[test]
    fn a_changed_message_or_another_key_is_refused() {
        let (pair, _) = KeyPair::generate().unwrap();
        let (other, _) = KeyPair::generate().unwrap();
        let signature = pair.sign(b"version 0.2.0");
        assert_eq!(
            verify(&pair.public_key(), b"version 9.9.9", &signature),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify(&other.public_key(), b"version 0.2.0", &signature),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn malformed_inputs_are_told_apart() {
        let (pair, _) = KeyPair::generate().unwrap();
        assert_eq!(
            verify(&pair.public_key(), b"x", "abcd"),
            Err(SignatureError::Malformed)
        );
        assert_eq!(
            PublicKey::from_hex("00ff"),
            Err(SignatureError::BadPublicKey)
        );
        assert!(matches!(
            KeyPair::from_hex("zz"),
            Err(SignatureError::BadPrivateKey)
        ));
    }

    #[test]
    fn the_compiled_in_public_key_is_well_formed() {
        PublicKey::from_hex(crate::PUBLIC_KEY).unwrap();
    }
}
