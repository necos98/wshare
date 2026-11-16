//! The audio path, without any audio API: the host cuts its capture into
//! chunks ([`Chunker`]), the viewer smooths their arrival before playing
//! them ([`JitterBuffer`]). Formats and constants are those of
//! [`protocol::audio`].

mod chunker;
mod jitter;

pub use chunker::Chunker;
pub use jitter::{JitterBuffer, JitterStats, MAX_DELAY, MIN_DELAY};
