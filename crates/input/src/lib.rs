//! Host-side injection of the viewer's remote-control input.
//!
//! The viewer sends [`protocol::input::InputEvent`]s; [`RemoteInput`] turns
//! each one into Win32 `SendInput` events on the host's desktop.
//!
//! Layout (policy is pure and tested; the OS sits behind two traits):
//! - [`geometry`]: normalized monitor coordinates -> `SendInput` absolute
//!   virtual-desktop coordinates.
//! - [`keys`]: validation of untrusted scan codes.
//! - [`synth`]: the events to inject ([`SyntheticInput`]), built per protocol
//!   event, and their conversion to Win32 `INPUT`.
//! - [`pressed`]: what the remote user holds down, so it can always be
//!   released (focus loss, session end).
//! - [`backend`] / [`metrics`]: the only code that calls Win32.
//! - [`remote`]: the facade wiring them together.
//!
//! Windows limits (documented, handled as non-fatal):
//! - UIPI: a non-elevated host cannot inject into elevated windows, and
//!   only a SYSTEM process attached to it can inject into the secure
//!   desktop (UAC prompt, lock screen, Ctrl+Alt+Del screen). `SendInput`
//!   then injects fewer events than asked, without a reliable error code;
//!   this surfaces as [`Outcome::Blocked`] with a `first` flag so the app
//!   logs it once. The unattended-access agent is such a process: its
//!   backend moves to the input desktop and retries (see `input_desktop`).
//! - Ctrl+Alt+Del (the secure attention sequence) is never honored when
//!   injected: those keys are just pressed like any other. The
//!   unattended-access service generates it with `SendSAS` instead.
//! - Injected keys do not auto-repeat: the viewer forwards its own repeats
//!   as repeated presses, which are injected one by one.

pub mod backend;
pub mod geometry;
pub mod keys;
pub mod metrics;
pub mod pressed;
pub mod remote;
pub mod synth;

pub use backend::{InputBackend, SendInputBackend};
pub use geometry::{AbsolutePoint, ScreenArea};
pub use metrics::{DesktopMetrics, SystemMetrics};
pub use remote::{Ignored, Outcome, RemoteInput};
pub use synth::SyntheticInput;
