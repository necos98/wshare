//! Small Media Foundation helpers shared by the encoder and the decoder:
//! runtime start-up, MFT discovery, D3D11 hand-off and attribute values.
//!
//! Nothing here knows about encoding or decoding policy; it only wraps the
//! verbose COM plumbing so the codec modules read as policy.

mod device;
mod enumerate;
mod runtime;
mod transform;
mod values;

pub(crate) use device::attach_d3d11_device;
pub(crate) use enumerate::{
    adapter_luid, enumerate_transforms, friendly_name, luid_key, prefer_adapter,
};
pub use runtime::MediaFoundation;
pub(crate) use transform::{process_output, shutdown};
pub(crate) use values::{pack_pair, unpack_pair, variant_bool, variant_u32};
