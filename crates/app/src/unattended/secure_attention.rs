//! Ctrl+Alt+Del (the secure attention sequence) on the console session.
//!
//! Injected keys never trigger it: only `SendSAS`, called by a service, and
//! only while the `SoftwareSASGeneration` policy lets services generate it.
//! That value is set just for the call and put back as it was (removed if
//! it was absent), so the policy of the PC is never changed for good and
//! uninstalling leaves nothing behind.

use anyhow::{Context, Result, ensure};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::Security::Authentication::Identity::SendSAS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_VALUE_TYPE,
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::HSTRING;

const POLICY_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
const POLICY_VALUE: &str = "SoftwareSASGeneration";
/// Bit of the policy that lets services generate the sequence (the other
/// one is for Ease of Access applications).
const SERVICES_BIT: u32 = 1;

/// Generates Ctrl+Alt+Del on the session at the console. Must run in the
/// service (LocalSystem).
pub(super) fn send() -> Result<()> {
    let key = PolicyKey::open()?;
    let previous = key.read()?;
    key.write(Some(allowing_services(previous)))?;
    // SAFETY: no preconditions; `false`: as the service, not as a user.
    unsafe { SendSAS(false) };
    key.write(previous)
        .context("restoring SoftwareSASGeneration")
}

/// The policy value that lets services generate the sequence, keeping what
/// `previous` already allowed.
fn allowing_services(previous: Option<u32>) -> u32 {
    previous.unwrap_or(0) | SERVICES_BIT
}

/// The policy key, closed on drop.
struct PolicyKey(HKEY);

impl PolicyKey {
    fn open() -> Result<Self> {
        let mut key = HKEY::default();
        // SAFETY: the string outlives the call and `key` is a valid output.
        unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                &HSTRING::from(POLICY_KEY),
                None,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                &mut key,
            )
        }
        .ok()
        .context("opening the system policies")?;
        Ok(Self(key))
    }

    /// The value; `None` when it is absent.
    fn read(&self) -> Result<Option<u32>> {
        let mut value = 0u32;
        let mut kind = REG_VALUE_TYPE::default();
        let mut size = size_of::<u32>() as u32;
        // SAFETY: the buffer is a u32 and `size` says so.
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                &HSTRING::from(POLICY_VALUE),
                None,
                Some(&mut kind),
                Some(std::ptr::from_mut(&mut value).cast()),
                Some(&mut size),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok().context("reading SoftwareSASGeneration")?;
        // Anything but a number is not ours to rewrite.
        ensure!(kind == REG_DWORD, "SoftwareSASGeneration is not a number");
        Ok(Some(value))
    }

    /// Writes `value`, or removes it for `None`.
    fn write(&self, value: Option<u32>) -> Result<()> {
        let name = HSTRING::from(POLICY_VALUE);
        let status: WIN32_ERROR = match value {
            // SAFETY: the bytes of a u32, as REG_DWORD wants.
            Some(value) => unsafe {
                RegSetValueExW(self.0, &name, None, REG_DWORD, Some(&value.to_le_bytes()))
            },
            // SAFETY: valid key and name.
            None => match unsafe { RegDeleteValueW(self.0, &name) } {
                status if status == ERROR_FILE_NOT_FOUND => return Ok(()),
                status => status,
            },
        };
        status.ok().context("writing SoftwareSASGeneration")
    }
}

impl Drop for PolicyKey {
    fn drop(&mut self) {
        // SAFETY: the key was opened by us and is closed once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_are_allowed_without_dropping_what_was_allowed() {
        assert_eq!(allowing_services(None), 1);
        assert_eq!(allowing_services(Some(0)), 1);
        assert_eq!(allowing_services(Some(1)), 1);
        assert_eq!(allowing_services(Some(2)), 3, "Ease of Access stays");
        assert_eq!(allowing_services(Some(3)), 3);
    }
}
