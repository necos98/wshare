//! The optional password of unattended access, kept only as a salted
//! PBKDF2-HMAC-SHA256 hash (`ring`), never in clear.
//!
//! Written as `pbkdf2-sha256$<iterations>$<salt hex>$<hash hex>`, so the
//! number of iterations can grow later without breaking saved hashes.
//! Passwords are compared trimmed, as every PIN field of wshare trims what
//! is typed.

use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail, ensure};
use ring::pbkdf2;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

/// Longest password accepted, in characters (fields stop there too).
pub const MAX_PASSWORD_CHARS: usize = 128;

const ALGORITHM: pbkdf2::Algorithm = pbkdf2::PBKDF2_HMAC_SHA256;
const SCHEME: &str = "pbkdf2-sha256";
const SEPARATOR: char = '$';
/// OWASP's recommendation for PBKDF2-HMAC-SHA256 (2023).
const ITERATIONS: u32 = 600_000;
/// Saved hashes outside this range are refused: too few is weak, too many
/// would let a damaged file stall every connection.
const MIN_ITERATIONS: u32 = 100_000;
const MAX_ITERATIONS: u32 = 10_000_000;
const SALT_LEN: usize = 16;
const HASH_LEN: usize = 32;

/// A password as it is stored: salt and derived key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PasswordHash {
    iterations: NonZeroU32,
    salt: [u8; SALT_LEN],
    hash: [u8; HASH_LEN],
}

impl PasswordHash {
    /// Hashes `password` with a fresh random salt. Slow on purpose (a few
    /// hundred milliseconds): never on a UI or network thread.
    pub fn new(password: &str) -> Result<Self> {
        Self::with_iterations(password, ITERATIONS)
    }

    /// The cheapest hash a saved file may hold, for quick tests.
    #[cfg(test)]
    pub(crate) fn quick(password: &str) -> Self {
        Self::with_iterations(password, MIN_ITERATIONS).unwrap()
    }

    fn with_iterations(password: &str, iterations: u32) -> Result<Self> {
        let password = normalized(password)?;
        let iterations = NonZeroU32::new(iterations).context("no iterations")?;
        let mut salt = [0; SALT_LEN];
        SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| anyhow!("the system random generator failed"))?;
        let mut hash = [0; HASH_LEN];
        pbkdf2::derive(ALGORITHM, iterations, &salt, password.as_bytes(), &mut hash);
        Ok(Self {
            iterations,
            salt,
            hash,
        })
    }

    /// Whether `given` is the password (compared in constant time). Slow
    /// on purpose, like [`Self::new`].
    pub fn verify(&self, given: &str) -> bool {
        pbkdf2::verify(
            ALGORITHM,
            self.iterations,
            &self.salt,
            given.trim().as_bytes(),
            &self.hash,
        )
        .is_ok()
    }
}

/// The password as compared: trimmed, not empty, not too long.
fn normalized(password: &str) -> Result<&str> {
    let password = password.trim();
    ensure!(!password.is_empty(), "the password is empty");
    ensure!(
        password.chars().count() <= MAX_PASSWORD_CHARS,
        "the password is longer than {MAX_PASSWORD_CHARS} characters"
    );
    Ok(password)
}

impl fmt::Display for PasswordHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{SCHEME}{SEPARATOR}{}{SEPARATOR}{}{SEPARATOR}{}",
            self.iterations,
            hex(&self.salt),
            hex(&self.hash)
        )
    }
}

/// Never shows the hash: logs must not help an offline attack.
impl fmt::Debug for PasswordHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordHash(..)")
    }
}

impl FromStr for PasswordHash {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let parts: Vec<&str> = text.trim().split(SEPARATOR).collect();
        let [scheme, iterations, salt, hash] = parts[..] else {
            bail!("not a password hash");
        };
        ensure!(scheme == SCHEME, "unknown password scheme '{scheme}'");
        let iterations: u32 = iterations.parse().context("password hash iterations")?;
        ensure!(
            (MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations),
            "password hash iterations out of range"
        );
        Ok(Self {
            iterations: NonZeroU32::new(iterations).context("no iterations")?,
            salt: unhex(salt).context("password hash salt")?,
            hash: unhex(hash).context("password hash")?,
        })
    }
}

impl TryFrom<String> for PasswordHash {
    type Error = anyhow::Error;

    fn try_from(text: String) -> Result<Self> {
        text.parse()
    }
}

impl From<PasswordHash> for String {
    fn from(hash: PasswordHash) -> Self {
        hash.to_string()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Exactly `N` bytes written as `2 * N` hex digits.
fn unhex<const N: usize>(text: &str) -> Result<[u8; N]> {
    ensure!(
        text.len() == 2 * N && text.is_ascii(),
        "expected {} hex digits",
        2 * N
    );
    let mut bytes = [0; N];
    for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        let pair = std::str::from_utf8(pair).context("not text")?;
        *byte = u8::from_str_radix(pair, 16).context("not a hex digit")?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_right_password_verifies() {
        let hash = PasswordHash::quick("Correct horse");
        assert!(hash.verify("Correct horse"));
        assert!(hash.verify("  Correct horse \n"), "trimmed like a PIN");
        assert!(!hash.verify("correct horse"));
        assert!(!hash.verify("Correct horse!"));
        assert!(!hash.verify(""));
    }

    #[test]
    fn the_same_password_gets_a_new_salt_every_time() {
        let (a, b) = (
            PasswordHash::quick("segreto"),
            PasswordHash::quick("segreto"),
        );
        assert_ne!(a, b);
        assert_ne!(a.to_string(), b.to_string());
        assert!(a.verify("segreto") && b.verify("segreto"));
    }

    #[test]
    fn the_saved_text_roundtrips_and_never_holds_the_password() {
        let hash = PasswordHash::quick("segreto-123");
        let text = hash.to_string();
        assert!(text.starts_with("pbkdf2-sha256$100000$"), "{text}");
        assert!(!text.contains("segreto"));
        assert_eq!(text.parse::<PasswordHash>().unwrap(), hash);
        assert_eq!(format!("{hash:?}"), "PasswordHash(..)");
    }

    #[test]
    fn new_hashes_use_the_recommended_iterations() {
        assert!((MIN_ITERATIONS..=MAX_ITERATIONS).contains(&ITERATIONS));
    }

    #[test]
    fn empty_and_overlong_passwords_are_refused() {
        assert!(PasswordHash::with_iterations("   ", MIN_ITERATIONS).is_err());
        let long = "x".repeat(MAX_PASSWORD_CHARS + 1);
        assert!(PasswordHash::with_iterations(&long, MIN_ITERATIONS).is_err());
        let longest = "è".repeat(MAX_PASSWORD_CHARS);
        assert!(PasswordHash::with_iterations(&longest, MIN_ITERATIONS).is_ok());
    }

    #[test]
    fn malformed_texts_are_refused() {
        let good = PasswordHash::quick("x").to_string();
        let salt = "00".repeat(SALT_LEN);
        let hash = "11".repeat(HASH_LEN);
        for bad in [
            String::new(),
            SCHEME.to_string(),
            good.replace(SCHEME, "md5"),
            format!("{SCHEME}$99999${salt}${hash}"),
            format!("{SCHEME}$10000001${salt}${hash}"),
            format!("{SCHEME}$abc${salt}${hash}"),
            format!("{SCHEME}$100000${}${hash}", &salt[2..]),
            format!("{SCHEME}$100000${salt}${}zz", &hash[2..]),
            format!("{SCHEME}$100000${salt}${hash}$extra"),
            format!("{SCHEME}$100000${}${hash}", "é".repeat(SALT_LEN)),
        ] {
            assert!(bad.parse::<PasswordHash>().is_err(), "{bad}");
        }
    }
}
