//! Text overlay (e.g. streaming statistics) painted onto the swap chain's
//! back buffer, after the video blit, through the shared [`Canvas`].

mod geometry;

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ROUNDED_RECT, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP, IDWriteTextFormat,
};
use windows::core::w;

use crate::canvas::Canvas;
use geometry::{STYLE, Style};

const BACKGROUND: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.6,
};
const FOREGROUND: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// Lines of monospace text in a dark rounded box, top-left of the window.
///
/// Brushes are created once; the text format only when the window's DPI
/// changes. While hidden it does nothing.
pub(crate) struct Overlay {
    lines: Option<Vec<String>>,
    background: ID2D1SolidColorBrush,
    foreground: ID2D1SolidColorBrush,
    /// Text format with the DPI it was made for.
    format: Option<(u32, IDWriteTextFormat)>,
}

impl Overlay {
    pub(crate) fn new(canvas: &Canvas) -> Result<Self> {
        let ctx = canvas.context();
        unsafe {
            Ok(Self {
                lines: None,
                background: ctx.CreateSolidColorBrush(&BACKGROUND, None)?,
                foreground: ctx.CreateSolidColorBrush(&FOREGROUND, None)?,
                format: None,
            })
        }
    }

    /// `None` hides the overlay; otherwise `lines` are drawn by every
    /// [`Overlay::draw`] until changed.
    pub(crate) fn set_lines(&mut self, lines: Option<Vec<String>>) {
        self.lines = lines;
    }

    /// Draws the overlay onto `back_buffer` (`size` pixels), on top of
    /// whatever the immediate context has drawn there so far. No-op while
    /// hidden.
    pub(crate) fn draw(
        &mut self,
        canvas: &mut Canvas,
        back_buffer: &ID3D11Texture2D,
        size: (u32, u32),
    ) -> Result<()> {
        let Some(lines) = &self.lines else {
            return Ok(());
        };
        let text: Vec<u16> = lines.join("\n").encode_utf16().collect();
        let dpi = canvas.dpi();
        let style = STYLE.for_dpi(dpi);
        let format = self.format(canvas, dpi, &style)?;
        let (max_width, max_height) = style.text_space(size);
        let layout = unsafe {
            canvas
                .dwrite()
                .CreateTextLayout(&text, &format, max_width, max_height)
                .context("CreateTextLayout")?
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        let placed = style.place((metrics.width, metrics.height), size);

        let rounded = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: placed.rect.left,
                top: placed.rect.top,
                right: placed.rect.right,
                bottom: placed.rect.bottom,
            },
            radiusX: placed.corner_radius,
            radiusY: placed.corner_radius,
        };
        let origin = windows_numerics::Vector2 {
            X: placed.text_origin.0,
            Y: placed.text_origin.1,
        };
        canvas.paint(back_buffer, |ctx| unsafe {
            ctx.FillRoundedRectangle(&rounded, &self.background);
            ctx.DrawTextLayout(
                origin,
                &layout,
                &self.foreground,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
            );
        })
    }

    fn format(&mut self, canvas: &Canvas, dpi: u32, style: &Style) -> Result<IDWriteTextFormat> {
        if let Some((cached, format)) = &self.format
            && *cached == dpi
        {
            return Ok(format.clone());
        }
        let format = unsafe {
            let format = canvas
                .dwrite()
                .CreateTextFormat(
                    w!("Consolas"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    style.font_size,
                    w!(""),
                )
                .context("CreateTextFormat")?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            format
        };
        self.format = Some((dpi, format.clone()));
        Ok(format)
    }
}
