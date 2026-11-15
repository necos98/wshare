//! Viewer-side presentation: shows decoded NV12 [`gpu::GpuFrame`]s in an
//! existing window with minimal latency.
//!
//! - [`Renderer`], the [`FrameSink`] adapter (flip-model swap chain + video
//!   processor colour conversion, letterboxed).
//! - [`fit_rect`], the letterbox geometry, exposed for input mapping (the
//!   app needs it to translate mouse positions into host coordinates).
//! - [`toolbar_geometry`] / [`ToolbarView`]: the full-screen control bar,
//!   drawn by the renderer and hit-tested by the app with the same geometry.
//!
//! Depends on `gpu` (frame type) and `pipeline` (the [`FrameSink`] trait it
//! implements), not on the decoder that produces the frames.

mod cache;
mod canvas;
mod layout;
mod overlay;
mod processor;
mod renderer;
mod swapchain;
#[cfg(test)]
mod test_window;
mod toolbar;
mod toolbar_geometry;
mod toolbar_painter;

pub use layout::{Rect, fit_rect};
pub use toolbar::ToolbarView;
pub use toolbar_geometry::{ToolbarGeometry, ToolbarHit, toolbar_geometry};
pub use pipeline::FrameSink;
pub use renderer::Renderer;
