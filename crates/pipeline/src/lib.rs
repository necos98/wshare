//! Streaming orchestration, independent of any concrete platform API.
//!
//! High-level policy (pacing, packetization, loss handling) lives here and
//! depends only on the abstractions in [`traits`]. Concrete adapters (DXGI
//! capture, Media Foundation codecs, D3D11 renderer, QUIC transport) live in
//! other crates and implement these traits (Dependency Inversion).

pub mod backlog;
pub mod host;
pub mod pacer;
pub mod traits;
pub mod viewer;

pub use host::{HostStats, HostStreamer};
pub use pacer::FramePacer;
pub use traits::{DatagramSink, EncodedFrame, FrameSink, FrameSource, VideoDecoder, VideoEncoder};
pub use viewer::{ViewerAction, ViewerStats, ViewerStreamer};
