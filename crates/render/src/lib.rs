//! Viewer-side presentation: shows decoded NV12 [`gpu::GpuFrame`]s in an
//! existing window with minimal latency.
//!
//! - [`Renderer`], the [`FrameSink`] adapter (flip-model swap chain + video
//!   processor colour conversion, letterboxed).
//! - [`fit_rect`], the letterbox geometry, exposed for input mapping (the
//!   app needs it to translate mouse positions into host coordinates).
//!
//! Depends only on `gpu`, not on the decoder that produces the frames.

mod cache;
mod layout;
mod processor;
mod renderer;
mod swapchain;
#[cfg(test)]
mod test_window;

pub use layout::{Rect, fit_rect};
pub use pipeline::FrameSink;
pub use renderer::Renderer;
