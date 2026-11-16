//! `wshare-release`: the CI side of updates (see the `release` crate).
//!
//! ```text
//! wshare-release keygen --out FILE
//!     New signing key: the private key (hex) goes to FILE, never printed;
//!     the public key is printed, for crates/release/update-key.pub.
//! wshare-release manifest --version V --base-url URL [--notes TEXT] [--date D] --out FILE ASSET...
//!     Writes latest.json for ASSET... (downloaded from URL/<file name>).
//! wshare-release sign --out SIG FILE
//!     Signs FILE with the private key in WSHARE_UPDATE_KEY and checks the
//!     signature against the compiled-in public key (a secret that does not
//!     match it would publish updates no installed wshare accepts).
//! wshare-release verify FILE SIG
//!     Checks SIG against the compiled-in public key.
//! ```

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use release::{FileEntry, KeyPair, Manifest, PUBLIC_KEY, PublicKey, sha256_hex, verify};

const KEY_ENV: &str = "WSHARE_UPDATE_KEY";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let (command, rest) = args.split_first().ok_or_else(usage)?;
    let mut flags = Flags::parse(rest)?;
    match command.as_str() {
        "keygen" => keygen(&flags.required("--out")?),
        "manifest" => manifest(&mut flags),
        "sign" => sign(&flags.required("--out")?, &flags.one_positional()?),
        "verify" => match flags.positional.as_slice() {
            [file, sig] => check(&read(file)?, &read_text(sig)?),
            _ => Err(usage()),
        },
        _ => Err(usage()),
    }
}

fn usage() -> String {
    "usage: wshare-release keygen|manifest|sign|verify (see the source for the options)".into()
}

fn keygen(out: &str) -> Result<(), String> {
    let (pair, private) = KeyPair::generate().map_err(|e| e.to_string())?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(|e| format!("creating {out} (it must not exist): {e}"))?;
    file.write_all(private.as_bytes())
        .map_err(|e| format!("writing {out}: {e}"))?;
    println!("{}", pair.public_key().to_hex());
    Ok(())
}

fn manifest(flags: &mut Flags) -> Result<(), String> {
    let version = flags.required("--version")?;
    let base = flags.required("--base-url")?;
    let notes = flags.optional("--notes").unwrap_or_default();
    let date = flags.optional("--date").unwrap_or_default();
    let out = flags.required("--out")?;
    if flags.positional.is_empty() {
        return Err("manifest: no assets given".into());
    }
    let files = flags
        .positional
        .iter()
        .map(|path| asset(Path::new(path), &base))
        .collect::<Result<Vec<_>, _>>()?;
    let manifest = Manifest {
        version,
        notes,
        pub_date: date,
        files,
    };
    // What the app will parse: refuse here what it would refuse there.
    let json = manifest.to_json();
    Manifest::parse(json.as_bytes()).map_err(|e| e.to_string())?;
    fs::write(&out, &json).map_err(|e| format!("writing {out}: {e}"))?;
    println!("{json}");
    Ok(())
}

fn asset(path: &Path, base: &str) -> Result<FileEntry, String> {
    let bytes = read(&path.to_string_lossy())?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_string_lossy()
        .into_owned();
    Ok(FileEntry {
        url: format!("{}/{name}", base.trim_end_matches('/')),
        sha256: sha256_hex(&bytes),
        size: bytes.len() as u64,
        name,
    })
}

fn sign(out: &str, file: &str) -> Result<(), String> {
    let private = std::env::var(KEY_ENV).map_err(|_| format!("{KEY_ENV} is not set"))?;
    let pair = KeyPair::from_hex(&private).map_err(|e| e.to_string())?;
    let message = read(file)?;
    let signature = pair.sign(&message);
    check(&message, &signature)
        .map_err(|e| format!("{e} ({KEY_ENV} does not belong to crates/release/update-key.pub)"))?;
    fs::write(out, &signature).map_err(|e| format!("writing {out}: {e}"))?;
    println!("signed {file} -> {out}");
    Ok(())
}

fn check(message: &[u8], signature: &str) -> Result<(), String> {
    let key = PublicKey::from_hex(PUBLIC_KEY).map_err(|e| e.to_string())?;
    verify(&key, message, signature).map_err(|e| e.to_string())
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    fs::read(PathBuf::from(path)).map_err(|e| format!("reading {path}: {e}"))
}

fn read_text(path: &str) -> Result<String, String> {
    String::from_utf8(read(path)?).map_err(|_| format!("{path} is not text"))
}

/// `--name value` pairs and the arguments that are not flags.
struct Flags {
    named: Vec<(String, String)>,
    positional: Vec<String>,
}

impl Flags {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut named = Vec::new();
        let mut positional = Vec::new();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if arg.starts_with("--") {
                let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
                named.push((arg.clone(), value.clone()));
            } else {
                positional.push(arg.clone());
            }
        }
        Ok(Self { named, positional })
    }

    fn optional(&mut self, name: &str) -> Option<String> {
        let at = self.named.iter().position(|(n, _)| n == name)?;
        Some(self.named.remove(at).1)
    }

    fn required(&mut self, name: &str) -> Result<String, String> {
        self.optional(name)
            .ok_or_else(|| format!("{name} is required"))
    }

    fn one_positional(&self) -> Result<String, String> {
        match self.positional.as_slice() {
            [one] => Ok(one.clone()),
            _ => Err(usage()),
        }
    }
}
