//! Folders only SYSTEM and administrators can open.
//!
//! Anyone may create folders in `ProgramData`, so a folder found there is
//! never trusted: it is removed (a link is removed, not followed) and made
//! again, with an access list that grants SYSTEM and administrators only
//! and is not inherited from the parent. Everything written inside inherits
//! it, the service's private key included.

use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::core::HSTRING;

/// Protected (`P`, nothing inherited) access list: full control for SYSTEM
/// (`SY`) and administrators (`BA`), passed on to files and subfolders
/// (`OICI`). Nobody else is listed, so nobody else gets in.
const ADMINS_ONLY: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

/// Makes `dir` anew, empty, readable by SYSTEM and administrators only.
/// Its parent must exist.
pub(super) fn recreate(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("removing {}", dir.display())),
    }
    let descriptor = Descriptor::admins_only()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };
    // SAFETY: the path and the descriptor outlive the call. It fails if
    // something appeared at `dir` meanwhile: that is never reused.
    unsafe { CreateDirectoryW(&HSTRING::from(dir.as_os_str()), Some(&attributes)) }
        .with_context(|| format!("creating {}", dir.display()))
}

/// A security descriptor allocated by Windows, freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn admins_only() -> Result<Self> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the string outlives the call; the descriptor is freed by
        // `Drop` with LocalFree, as documented.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(ADMINS_ONLY),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .context("building the access list")?;
        Ok(Self(descriptor))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptor..., freed once.
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Creating the folder is left to the manual check (see the README): a
    // folder only administrators can open could not be cleaned up by a
    // test running as a standard user.
    #[test]
    fn the_access_list_is_valid() {
        assert!(Descriptor::admins_only().is_ok());
    }
}
