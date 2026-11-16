//! The shared clipboard: text and files copied on one PC can be pasted on
//! the other, both ways, while a session lasts.
//!
//! Layers, from the pure to the Win32 ones:
//! - [`rules`]: who may send or accept text and files, which text is worth
//!   sending, and the guard that keeps it from bouncing between the PCs.
//! - [`access`]: the clipboard behind a trait, and the retries for a
//!   clipboard another program holds open.
//! - [`sync`]: rules and access together: what to send when the clipboard
//!   changed, and what to do with the text the other PC sent.
//! - [`encoding`]: the clipboard's UTF-16 text format.
//! - [`window`], [`system`], [`global`] and [`virtual_files`]: the only code
//!   that calls Win32 and OLE (the message-only window that listens for
//!   changes and owns the OLE clipboard, the real access, and the other
//!   PC's files as virtual files Explorer can paste).
//!
//! Text (`CF_UNICODETEXT`) and files (`CF_HDROP`, offered to the other PC
//! as virtual files) are shared, only while a session is open and only
//! when the clipboard changes: nothing is synchronised on connection.

mod access;
mod encoding;
mod file_sharing;
mod global;
mod rules;
mod sync;
mod system;
mod virtual_files;
mod window;

pub use access::{AccessError, ClipboardAccess};
pub use file_sharing::FileSharing;
pub use rules::{HostRules, Outgoing, ViewerRules};
pub use sync::{ClipboardSync, Received, share_copied_files};
pub use system::SystemClipboard;

#[cfg(test)]
pub(crate) use file_sharing::fake as fake_sharing;
#[cfg(test)]
pub(crate) use rules::ClipboardContent;
#[cfg(test)]
pub(crate) use sync::fake;
