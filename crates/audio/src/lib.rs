//! The sound card, through WASAPI in shared mode.
//!
//! - [`LoopbackCapture`]: what the host's default playback device plays
//!   (loopback), for the viewer to hear.
//! - [`Playback`]: the viewer's default playback device.
//! - [`DefaultDevice`]: which device is the default now, so both sides can
//!   follow the user switching it (headphones plugged in, ...).
//!
//! Both streams use the wire format of [`protocol::audio`] (48 kHz,
//! stereo, 16-bit): Windows converts from and to the device's own format
//! (`AUTOCONVERTPCM`), so no resampler is needed here.
//!
//! Every call needs COM on the calling thread: hold a [`ComApartment`]
//! there first. The objects are not `Send`: open, use and drop them on one
//! thread.

mod capture;
mod com;
mod device;
mod playback;

pub use capture::LoopbackCapture;
pub use com::ComApartment;
pub use device::DefaultDevice;
pub use playback::Playback;
