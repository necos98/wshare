//! The signed update manifest of wshare.
//!
//! Every GitHub Release publishes `latest.json` (a [`Manifest`]: version and
//! the files of the release with their SHA-256) and `latest.json.sig`, an
//! Ed25519 signature of the exact bytes of `latest.json`. The app checks the
//! signature with [`PUBLIC_KEY`], compiled in, before it trusts anything the
//! manifest says; the files are then checked against their hashes. The
//! private key lives only in the repository's GitHub secrets (see the
//! README, «Rilasci e aggiornamenti»).
//!
//! - `version`: version comparison (`1.10.0` > `1.9.0`, release >
//!   prerelease);
//! - `signature`: key pairs, signing and verification;
//! - `hex`: the text form of keys, signatures and hashes.
//!
//! `wshare-release` (the binary of this crate) is the CI side: it writes
//! the manifest, signs it and checks the signature against [`PUBLIC_KEY`].

mod hex;
mod manifest;
mod signature;
mod version;

pub use hex::{decode as hex_decode, encode as hex_encode};
pub use manifest::{FileEntry, Manifest, ManifestError, sha256_hex};
pub use signature::{KeyPair, PublicKey, SignatureError, verify};
pub use version::{Version, is_newer};

/// The public key that signs wshare's releases, as hex (32 bytes).
pub const PUBLIC_KEY: &str = include_str!("../update-key.pub");

/// File name of the manifest in every release.
pub const MANIFEST_FILE: &str = "latest.json";
/// File name of the manifest's signature in every release.
pub const SIGNATURE_FILE: &str = "latest.json.sig";
