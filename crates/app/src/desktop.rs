//! The Windows shell for both front-ends (the viewer window and the desktop
//! interface): choosing files or a folder, opening a folder in Explorer.
//!
//! A choice is a modal dialog: it runs on a thread of its own (an STA, as
//! the dialog requires) so neither the viewer's window nor the interface
//! stops while it is open; the answer comes back through a callback.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    FILEOPENDIALOGOPTIONS, FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS,
    FileOpenDialog, IFileOpenDialog, IShellItem, SIGDN_FILESYSPATH,
};
use windows::core::HSTRING;

/// What a dialog chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// One or more files.
    Files,
    /// One folder.
    Folder,
}

/// Shows the dialog for `choice` on a thread of its own and hands what was
/// chosen to `done` (nothing if the user cancelled or the dialog failed).
pub fn choose(choice: Choice, title: &str, done: impl FnOnce(Vec<PathBuf>) + Send + 'static) {
    let title = title.to_string();
    let spawned = std::thread::Builder::new()
        .name("wshare-dialog".into())
        .spawn(move || {
            let chosen = show(choice, &title).unwrap_or_else(|e| {
                eprintln!("warning: the file dialog failed: {e:#}");
                Vec::new()
            });
            done(chosen);
        });
    if let Err(e) = spawned {
        eprintln!("warning: cannot open the file dialog: {e}");
    }
}

/// Opens `folder` in Explorer.
pub fn open_folder(folder: &Path) -> Result<()> {
    // The path is one argument of its own: never parsed by a shell.
    Command::new("explorer.exe")
        .arg(folder)
        .spawn()
        .map(|_| ())
        .with_context(|| format!("opening {}", folder.display()))
}

/// COM is set up on this thread while it exists.
struct Apartment;

impl Apartment {
    fn enter() -> Result<Self> {
        // SAFETY: called on a fresh thread; paired with `CoUninitialize`.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .context("starting COM")?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: pairs the successful `CoInitializeEx` of `enter`.
        unsafe { CoUninitialize() };
    }
}

/// Shows the dialog on this thread; blocks until it closes.
fn show(choice: Choice, title: &str) -> Result<Vec<PathBuf>> {
    let _apartment = Apartment::enter()?;
    // SAFETY: COM is initialised on this thread; every interface is used
    // and released here, before the apartment ends.
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
            .context("creating the file dialog")?;
        let options = dialog
            .GetOptions()
            .context("reading the dialog's options")?;
        dialog
            .SetOptions(options | FOS_FORCEFILESYSTEM | extra_options(choice))
            .context("setting the dialog's options")?;
        dialog
            .SetTitle(&HSTRING::from(title))
            .context("titling the dialog")?;
        if let Err(e) = dialog.Show(None) {
            if e.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(Vec::new());
            }
            return Err(e).context("showing the dialog");
        }
        let results = dialog.GetResults().context("reading the choice")?;
        let count = results.GetCount().context("counting the choice")?;
        (0..count)
            .map(|index| results.GetItemAt(index).context("reading the choice"))
            .map(|item| item.and_then(|item| file_system_path(&item)))
            .collect()
    }
}

fn extra_options(choice: Choice) -> FILEOPENDIALOGOPTIONS {
    match choice {
        Choice::Files => FOS_ALLOWMULTISELECT,
        Choice::Folder => FOS_PICKFOLDERS,
    }
}

/// The path of a chosen item.
///
/// # Safety
/// COM must be initialised on this thread.
unsafe fn file_system_path(item: &IShellItem) -> Result<PathBuf> {
    // SAFETY: per the contract; the string is freed with the allocator the
    // shell used for it, as documented.
    unsafe {
        let name = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .context("reading a chosen path")?;
        let path = name.to_string();
        CoTaskMemFree(Some(name.0.cast_const().cast()));
        Ok(PathBuf::from(path.context("a chosen path is not text")?))
    }
}
