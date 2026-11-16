//! The shared clipboard: text copied on one PC can be pasted on the other,
//! both ways, while a session lasts.
//!
//! Layers, from the pure to the Win32 ones:
//! - [`rules`]: who may send or accept text, which text is worth sending,
//!   and the guard that keeps a text from bouncing between the two PCs.
//! - [`access`]: the clipboard behind a trait, and the retries for a
//!   clipboard another program holds open.
//! - [`sync`]: rules and access together: what to send when the clipboard
//!   changed, and what to do with the text the other PC sent.
//! - [`encoding`]: the clipboard's UTF-16 text format.
//! - [`window`] and [`system`]: the only code that calls Win32 (the
//!   message-only window that listens for changes, and the real access).
//!
//! Only text (`CF_UNICODETEXT`) is shared, only while a session is open and
//! only when the clipboard changes: nothing is synchronised on connection.

mod access;
mod encoding;
mod rules;
mod sync;
mod system;
mod window;

pub use access::{AccessError, ClipboardAccess};
pub use rules::{HostRules, Outgoing, ViewerRules};
pub use sync::{ClipboardSync, Received};
pub use system::SystemClipboard;

#[cfg(test)]
pub(crate) use rules::ClipboardContent;
#[cfg(test)]
pub(crate) use sync::fake;
