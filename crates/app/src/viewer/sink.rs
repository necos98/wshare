//! The viewer's frame sink: the renderer plus the last picture, so a resized
//! window can be redrawn without waiting for the host to send a new frame
//! (a static screen produces no frames at all).

use anyhow::Result;
use gpu::GpuFrame;
use pipeline::FrameSink;
use render::Renderer;

pub struct ViewSink {
    renderer: Renderer,
    last: Option<GpuFrame>,
}

impl ViewSink {
    pub fn new(renderer: Renderer) -> Self {
        Self { renderer, last: None }
    }

    /// Resizes the swap chain and redraws the last picture, if any.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.renderer.resize(width, height)?;
        match &self.last {
            Some(frame) => self.renderer.present(frame),
            None => Ok(()),
        }
    }
}

impl FrameSink for ViewSink {
    type Frame = GpuFrame;

    fn present(&mut self, frame: &GpuFrame) -> Result<()> {
        self.renderer.present(frame)?;
        // Holding the frame keeps one decoder surface busy; decoders have
        // several, so this is cheap.
        self.last = Some(frame.clone());
        Ok(())
    }
}
