//! GPU colour conversion and hardware video encoding/decoding (Media
//! Foundation), exposed as adapters of the `pipeline` traits:
//!
//! - [`DesktopSource`] implements [`FrameSource`] (desktop capture -> NV12 texture)
//! - [`HwEncoder`] implements [`VideoEncoder`] (NV12 texture -> Annex-B)
//! - [`HwDecoder`] implements [`VideoDecoder`] (Annex-B -> [`gpu::GpuFrame`])
//!
//! Call [`MediaFoundation::start`] (or `MFStartup` yourself) on each thread
//! using an encoder/decoder.

mod convert;
mod decoder;
mod encoder;
mod format;
mod mf;
mod source;

pub use convert::BgraToNv12;
pub use decoder::HwDecoder;
pub use encoder::{EncoderConfig, HwEncoder};
pub use format::Codec;
pub use mf::MediaFoundation;
/// Re-exported so users of the adapters need not depend on `pipeline` just
/// to call the trait methods.
pub use pipeline::{EncodedFrame, FrameSource, VideoDecoder, VideoEncoder};
pub use source::DesktopSource;
