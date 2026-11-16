//! Balloon notifications from the tray icon (`Shell_NotifyIconW` with
//! `NIF_INFO`): the lightest native way to tell the user something, with
//! no installer or app identity needed.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    NIF_INFO, NIIF_INFO, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};

/// The id `tray-icon` gives the first (and here only) icon it creates: it
/// numbers them from 1 and does not publish the number.
pub const TRAY_ICON_ID: u32 = 1;

/// Shows a balloon on the tray icon whose message window is `window`.
/// Best effort: Windows may hide balloons (Focus assist), and there is
/// nothing to do about it.
pub fn show(window: HWND, title: &str, body: &str) {
    let data = balloon_data(window, title, body);
    // SAFETY: `data` is fully initialised and outlives the call.
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

fn balloon_data(window: HWND, title: &str, body: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: TRAY_ICON_ID,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    copy_wide(&mut data.szInfoTitle, title);
    copy_wide(&mut data.szInfo, body);
    data
}

/// `text` as UTF-16 into `buffer`, cut to fit and always NUL-terminated.
fn copy_wide(buffer: &mut [u16], text: &str) {
    let room = buffer.len().saturating_sub(1);
    let mut written = 0;
    for unit in text.encode_utf16().take(room) {
        buffer[written] = unit;
        written += 1;
    }
    // Do not leave half of a surrogate pair at the end.
    if written > 0 && (0xD800..0xDC00).contains(&buffer[written - 1]) {
        written -= 1;
    }
    buffer[written..].fill(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(buffer: &[u16]) -> String {
        let len = buffer.iter().position(|&u| u == 0).unwrap();
        String::from_utf16(&buffer[..len]).unwrap()
    }

    #[test]
    fn text_is_copied_with_its_terminator() {
        let mut buffer = [0xFFFF; 8];
        copy_wide(&mut buffer, "wshare");
        assert_eq!(text_of(&buffer), "wshare");
        assert_eq!(buffer[6..], [0, 0]);
    }

    #[test]
    fn long_text_is_cut_to_fit() {
        let mut buffer = [0xFFFF; 4];
        copy_wide(&mut buffer, "abcdef");
        assert_eq!(text_of(&buffer), "abc");
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        // U+1F5A5 takes two UTF-16 units; only one would fit.
        let mut buffer = [0xFFFF; 3];
        copy_wide(&mut buffer, "a\u{1F5A5}");
        assert_eq!(text_of(&buffer), "a");
        let mut accented = [0xFFFF; 8];
        copy_wide(&mut accented, "è connesso");
        assert_eq!(text_of(&accented), "è conne");
    }

    #[test]
    fn the_request_names_the_tray_icon_and_both_texts() {
        let data = balloon_data(
            HWND(std::ptr::dangling_mut()),
            "wshare",
            "PORTATILE si è connesso",
        );
        assert_eq!(data.uID, TRAY_ICON_ID);
        assert_eq!(data.uFlags, NIF_INFO);
        assert_eq!(text_of(&data.szInfoTitle), "wshare");
        assert_eq!(text_of(&data.szInfo), "PORTATILE si è connesso");
    }
}
