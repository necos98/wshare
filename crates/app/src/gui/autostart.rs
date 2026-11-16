//! "Avvia con Windows": a value under the per-user `Run` key of the
//! registry. Nothing is ever written unless the user switches the option
//! on, and switching it off deletes the value again.

use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegSetValueExW,
};
use windows::core::HSTRING;

use crate::cli::MINIMIZED_FLAG;

pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const VALUE_NAME: &str = "wshare";

/// The command Windows runs at sign-in: the executable, quoted because its
/// path may hold spaces, and the flag that opens it hidden in the tray.
pub fn run_command(exe: &Path, minimized: bool) -> String {
    let mut command = format!("\"{}\"", exe.display());
    if minimized {
        command.push(' ');
        command.push_str(MINIMIZED_FLAG);
    }
    command
}

/// Registers (or, with `None`, removes) the start-up command of this exe.
pub fn apply(command: Option<&str>) -> Result<()> {
    write_value(RUN_KEY, VALUE_NAME, command)
}

fn write_value(subkey: &str, name: &str, value: Option<&str>) -> Result<()> {
    let key = Key::open(subkey)?;
    match value {
        Some(value) => key.set(name, value),
        None => key.delete(name),
    }
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Key {
    fn open(subkey: &str) -> Result<Self> {
        let mut key = HKEY::default();
        // SAFETY: the string outlives the call and `key` is a valid output.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(subkey),
                None,
                KEY_SET_VALUE,
                &mut key,
            )
        };
        status
            .ok()
            .with_context(|| format!("opening HKCU\\{subkey}"))?;
        Ok(Self(key))
    }

    fn set(&self, name: &str, value: &str) -> Result<()> {
        let wide: Vec<u8> = value
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: the key is open and `wide` is a NUL-terminated UTF-16 string.
        unsafe { RegSetValueExW(self.0, &HSTRING::from(name), None, REG_SZ, Some(&wide)) }
            .ok()
            .with_context(|| format!("writing the value {name}"))
    }

    /// A value that is already absent is the goal reached.
    fn delete(&self, name: &str) -> Result<()> {
        // SAFETY: the key is open and the name outlives the call.
        let status: WIN32_ERROR = unsafe { RegDeleteValueW(self.0, &HSTRING::from(name)) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        status
            .ok()
            .with_context(|| format!("removing the value {name}"))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened by `open` and is closed only here.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use windows::Win32::System::Registry::{
        RRF_RT_REG_SZ, RegCreateKeyW, RegDeleteKeyW, RegGetValueW,
    };

    #[test]
    fn the_command_quotes_the_path_and_adds_the_flag_on_request() {
        let exe = PathBuf::from(r"C:\Program Files\wshare\wshare.exe");
        assert_eq!(
            run_command(&exe, false),
            r#""C:\Program Files\wshare\wshare.exe""#
        );
        assert_eq!(
            run_command(&exe, true),
            r#""C:\Program Files\wshare\wshare.exe" --minimized"#
        );
    }

    /// A scratch key under HKCU, deleted at the end of the test.
    struct Scratch(String);

    impl Scratch {
        fn new() -> Self {
            let subkey = format!(r"Software\wshare-test-{}", std::process::id());
            let mut key = HKEY::default();
            // SAFETY: valid strings and output.
            unsafe {
                RegCreateKeyW(HKEY_CURRENT_USER, &HSTRING::from(subkey.as_str()), &mut key)
                    .ok()
                    .unwrap();
                let _ = RegCloseKey(key);
            }
            Self(subkey)
        }

        fn read(&self, name: &str) -> Option<String> {
            let mut buffer = [0u16; 256];
            let mut bytes = (buffer.len() * 2) as u32;
            // SAFETY: the buffer and its byte size are consistent.
            let status = unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    &HSTRING::from(self.0.as_str()),
                    &HSTRING::from(name),
                    RRF_RT_REG_SZ,
                    None,
                    Some(buffer.as_mut_ptr().cast()),
                    Some(&mut bytes),
                )
            };
            status.ok().ok()?;
            let len = (bytes as usize / 2).saturating_sub(1);
            Some(String::from_utf16_lossy(&buffer[..len]))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // SAFETY: valid string.
            let _ = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(self.0.as_str())) };
        }
    }

    #[test]
    fn a_value_is_written_replaced_and_removed() {
        let scratch = Scratch::new();
        assert_eq!(scratch.read("wshare"), None);

        write_value(
            &scratch.0,
            "wshare",
            Some(r#""C:\a b\wshare.exe" --minimized"#),
        )
        .unwrap();
        assert_eq!(
            scratch.read("wshare").as_deref(),
            Some(r#""C:\a b\wshare.exe" --minimized"#)
        );
        write_value(&scratch.0, "wshare", Some(r#""C:\a b\wshare.exe""#)).unwrap();
        assert_eq!(
            scratch.read("wshare").as_deref(),
            Some(r#""C:\a b\wshare.exe""#)
        );

        write_value(&scratch.0, "wshare", None).unwrap();
        assert_eq!(scratch.read("wshare"), None);
        // Removing what is not there is not an error.
        write_value(&scratch.0, "wshare", None).unwrap();
    }

    #[test]
    fn a_missing_key_is_reported() {
        assert!(write_value(r"Software\wshare-test-missing\nothing", "wshare", None).is_err());
    }
}
