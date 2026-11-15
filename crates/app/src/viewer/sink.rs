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
        Self {
            renderer,
            last: None,
        }
    }

    /// Resizes the swap chain and redraws the last picture, if any.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.renderer.resize(width, height)?;
        self.redraw()
    }

    /// Shows (`Some`) or hides the text overlay, redrawing right away: on a
    /// static screen no new frame would come to carry the change.
    pub fn set_overlay(&mut self, lines: Option<Vec<String>>) -> Result<()> {
        self.renderer.set_overlay(lines);
        self.redraw()
    }

    /// Shows (`Some`) or hides the notice box, redrawing right away.
    pub fn set_notice(&mut self, lines: Option<Vec<String>>) -> Result<()> {
        self.renderer.set_notice(lines);
        self.redraw()
    }

    /// Sets the control bar (`None`: no bar), redrawing at once.
    pub fn set_toolbar(&mut self, view: Option<render::ToolbarView>) -> Result<()> {
        self.renderer.set_toolbar(view);
        self.redraw()
    }

    fn redraw(&mut self) -> Result<()> {
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
