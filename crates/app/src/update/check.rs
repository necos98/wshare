//! Is there a newer wshare? Downloads the signed manifest and decides.

use release::{MANIFEST_FILE, Manifest, PUBLIC_KEY, PublicKey, SIGNATURE_FILE};

use super::{PROGRAM_FILES, UpdateFailure, current_version, fetch, latest_url};

/// A newer release, from a manifest whose signature was verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    manifest: Manifest,
}

impl Available {
    pub fn version(&self) -> &str {
        &self.manifest.version
    }

    pub fn notes(&self) -> &str {
        &self.manifest.notes
    }

    pub(super) fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

/// The newer release, or `None` if this wshare is the latest.
pub fn check() -> Result<Option<Available>, UpdateFailure> {
    let base = latest_url();
    let base = base.trim_end_matches('/');
    let manifest = fetch::bytes(&format!("{base}/{MANIFEST_FILE}"))?;
    let signature = fetch::bytes(&format!("{base}/{SIGNATURE_FILE}"))?;
    decide(current_version(), &manifest, &signature, PUBLIC_KEY)
}

/// What a downloaded manifest means for `current`: nothing is read from it
/// before its signature checks out with `public_key`.
fn decide(
    current: &str,
    manifest: &[u8],
    signature: &[u8],
    public_key: &str,
) -> Result<Option<Available>, UpdateFailure> {
    let key = PublicKey::from_hex(public_key).map_err(|_| UpdateFailure::BadSignature)?;
    let signature = std::str::from_utf8(signature).map_err(|_| UpdateFailure::BadSignature)?;
    release::verify(&key, manifest, signature).map_err(|_| UpdateFailure::BadSignature)?;
    let manifest =
        Manifest::parse(manifest).map_err(|e| UpdateFailure::BadManifest(e.to_string()))?;
    if !release::is_newer(current, &manifest.version) {
        return Ok(None);
    }
    if manifest.file(PROGRAM_FILES[0]).is_none() {
        return Err(UpdateFailure::BadManifest(format!(
            "no {} in the release",
            PROGRAM_FILES[0]
        )));
    }
    Ok(Some(Available { manifest }))
}

#[cfg(test)]
pub(crate) mod tests {
    use release::{FileEntry, KeyPair, sha256_hex};

    use super::*;

    pub fn manifest(version: &str, files: &[(&str, &[u8])]) -> Manifest {
        Manifest {
            version: version.to_string(),
            notes: format!("wshare v{version}"),
            pub_date: String::new(),
            files: files
                .iter()
                .map(|(name, bytes)| FileEntry {
                    name: (*name).to_string(),
                    url: format!("https://example.invalid/{name}"),
                    sha256: sha256_hex(bytes),
                    size: bytes.len() as u64,
                })
                .collect(),
        }
    }

    /// A signed manifest of `version` with a wshare.exe, and its key.
    fn signed(version: &str) -> (Vec<u8>, Vec<u8>, String) {
        let (pair, _) = KeyPair::generate().unwrap();
        let json = manifest(version, &[("wshare.exe", b"exe")])
            .to_json()
            .into_bytes();
        let signature = pair.sign(&json).into_bytes();
        (json, signature, pair.public_key().to_hex())
    }

    #[test]
    fn a_newer_signed_release_is_available() {
        let (json, sig, key) = signed("0.2.0");
        let available = decide("0.1.0", &json, &sig, &key).unwrap().unwrap();
        assert_eq!(available.version(), "0.2.0");
        assert_eq!(available.notes(), "wshare v0.2.0");
    }

    #[test]
    fn the_same_or_an_older_release_is_not_an_update() {
        let (json, sig, key) = signed("0.1.0");
        assert_eq!(decide("0.1.0", &json, &sig, &key), Ok(None));
        assert_eq!(decide("0.3.0", &json, &sig, &key), Ok(None));
    }

    #[test]
    fn an_unsigned_or_tampered_manifest_is_refused() {
        let (json, sig, key) = signed("0.2.0");
        let (_, _, other_key) = signed("0.2.0");
        assert_eq!(
            decide("0.1.0", &json, &sig, &other_key),
            Err(UpdateFailure::BadSignature)
        );
        let tampered = String::from_utf8(json)
            .unwrap()
            .replace("0.2.0", "9.9.9")
            .into_bytes();
        assert_eq!(
            decide("0.1.0", &tampered, &sig, &key),
            Err(UpdateFailure::BadSignature)
        );
        assert_eq!(
            decide("0.1.0", b"{}", b"not hex", &key),
            Err(UpdateFailure::BadSignature)
        );
    }

    #[test]
    fn a_release_without_wshare_exe_is_refused() {
        let (pair, _) = KeyPair::generate().unwrap();
        let json = manifest("0.2.0", &[("wshare-cli.exe", b"cli")])
            .to_json()
            .into_bytes();
        let sig = pair.sign(&json).into_bytes();
        assert!(matches!(
            decide("0.1.0", &json, &sig, &pair.public_key().to_hex()),
            Err(UpdateFailure::BadManifest(_))
        ));
    }

    /// For `install`'s tests: an update to `manifest` without a signature.
    pub fn available(manifest: Manifest) -> Available {
        Available { manifest }
    }
}
