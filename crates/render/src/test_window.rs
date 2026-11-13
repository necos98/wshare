//! An invisible top-level window for GPU tests (swap chains need an HWND).

use anyhow::Result;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WINDOW_EX_STYLE, WNDCLASSW,
    WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

pub(crate) struct HiddenWindow(HWND);

impl HiddenWindow {
    pub(crate) fn new(width: i32, height: i32) -> Result<Self> {
        unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            unsafe { DefWindowProcW(h, m, w, l) }
        }
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: w!("wshare-render-test"),
                ..Default::default()
            };
            // Fails harmlessly when another test registered it already.
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("wshare-render-test"),
                w!("render test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                width,
                height,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            Ok(Self(hwnd))
        }
    }

    pub(crate) fn hwnd(&self) -> HWND {
        self.0
    }
}

impl Drop for HiddenWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}
