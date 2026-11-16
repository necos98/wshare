//! What the clipboard needs of the session's transfers to share files:
//! offer what was copied here, withdraw it, and fetch what the other PC
//! copied. Behind a trait so the clipboard's logic is tested without a
//! connection; the session's [`TransferHub`] is the real one.

use std::path::PathBuf;

use crate::transfer::{PasteLink, TransferHub};

pub trait FileSharing: Sync {
    /// Files were copied here: announce them to the other PC. `on_error`
    /// hears, in English, why they cannot be (too many, unreadable...).
    fn offer(&self, picked: Vec<PathBuf>, on_error: Box<dyn FnOnce(String) + Send>);
    /// Something else was copied: the files offered can no longer be
    /// pasted on the other PC.
    fn withdraw(&self);
    /// How the other PC's files are fetched when pasted here.
    fn paste_link(&self) -> PasteLink;
}

impl FileSharing for TransferHub {
    fn offer(&self, picked: Vec<PathBuf>, on_error: Box<dyn FnOnce(String) + Send>) {
        self.offer_clipboard_files(picked, move |error| on_error(error.to_string()));
    }

    fn withdraw(&self) {
        self.withdraw_clipboard();
    }

    fn paste_link(&self) -> PasteLink {
        TransferHub::paste_link(self)
    }
}

#[cfg(test)]
pub mod fake {
    //! Records what the clipboard asked of the transfers.

    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::FileSharing;
    use crate::transfer::PasteLink;

    #[derive(Default)]
    pub struct FakeSharing {
        pub offered: Mutex<Vec<Vec<PathBuf>>>,
        pub withdrawn: Mutex<u32>,
    }

    impl FileSharing for FakeSharing {
        fn offer(&self, picked: Vec<PathBuf>, _on_error: Box<dyn FnOnce(String) + Send>) {
            self.offered.lock().unwrap().push(picked);
        }

        fn withdraw(&self) {
            *self.withdrawn.lock().unwrap() += 1;
        }

        fn paste_link(&self) -> PasteLink {
            PasteLink::new().0
        }
    }
}
