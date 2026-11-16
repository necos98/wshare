//! Streaming orchestration, independent of any concrete platform API.
//!
//! High-level policy (pacing, packetization, loss recovery, bitrate
//! adaptation) lives here and depends only on the abstractions in
//! [`traits`]. Concrete adapters (DXGI capture, Media Foundation codecs,
//! D3D11 renderer, QUIC transport) live in other crates and implement these
//! traits (Dependency Inversion).

pub mod backlog;
pub mod bitrate;
pub mod burst;
pub mod host;
pub mod keyframe_gate;
pub mod pacer;
pub mod recovery;
pub mod retransmit;
pub mod traits;
pub mod viewer;

pub use bitrate::{BitrateController, NetworkSample};
pub use host::{HostStats, HostStreamer, StreamContinuation};
pub use pacer::FramePacer;
pub use retransmit::RecentFrames;
pub use traits::{DatagramSink, EncodedFrame, FrameSink, FrameSource, VideoDecoder, VideoEncoder};
pub use viewer::{ViewerAction, ViewerStats, ViewerStreamer};
