//! Paints the control bar with Direct2D through the shared [`Canvas`],
//! using exactly the rectangles of [`toolbar_geometry`].

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ROUNDED_RECT,
    ID2D1DeviceContext, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, IDWriteTextFormat,
};
use windows::core::w;

use crate::canvas::Canvas;
use crate::layout::Rect;
use crate::toolbar::ToolbarView;
use crate::toolbar_geometry::toolbar_geometry;

const fn colour(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

const HANDLE: D2D1_COLOR_F = colour(1.0, 1.0, 1.0, 0.35);
const BAR: D2D1_COLOR_F = colour(0.07, 0.07, 0.09, 0.82);
const BUTTON: D2D1_COLOR_F = colour(1.0, 1.0, 1.0, 0.10);
const BUTTON_HOVER: D2D1_COLOR_F = colour(0.20, 0.50, 0.95, 0.90);
const LABEL: D2D1_COLOR_F = colour(1.0, 1.0, 1.0, 1.0);

/// Font size (px at 100 % scaling) and corner radii.
const FONT_SIZE: f32 = 13.0;
const BAR_RADIUS: f32 = 12.0;
const BUTTON_RADIUS: f32 = 6.0;

/// Draws the bar described by an optional [`ToolbarView`]; `None` draws
/// nothing and costs nothing.
pub(crate) struct ToolbarPainter {
    view: Option<ToolbarView>,
    handle: ID2D1SolidColorBrush,
    bar: ID2D1SolidColorBrush,
    button: ID2D1SolidColorBrush,
    hover: ID2D1SolidColorBrush,
    label: ID2D1SolidColorBrush,
    /// Label format with the DPI it was made for.
    format: Option<(u32, IDWriteTextFormat)>,
}

impl ToolbarPainter {
    pub(crate) fn new(canvas: &Canvas) -> Result<Self> {
        let ctx = canvas.context();
        let brush = |c: D2D1_COLOR_F| unsafe { ctx.CreateSolidColorBrush(&c, None) };
        Ok(Self {
            view: None,
            handle: brush(HANDLE)?,
            bar: brush(BAR)?,
            button: brush(BUTTON)?,
            hover: brush(BUTTON_HOVER)?,
            label: brush(LABEL)?,
            format: None,
        })
    }

    pub(crate) fn set_view(&mut self, view: Option<ToolbarView>) {
        self.view = view;
    }

    pub(crate) fn draw(
        &mut self,
        canvas: &mut Canvas,
        back_buffer: &ID3D11Texture2D,
        size: (u32, u32),
    ) -> Result<()> {
        let Some((expanded, count)) = self.view.as_ref().map(|v| (v.expanded, v.buttons.len()))
        else {
            return Ok(());
        };
        let dpi = canvas.dpi();
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let geometry = toolbar_geometry(size, scale, count, expanded);
        let format = if expanded {
            Some(self.format(canvas, dpi, scale)?)
        } else {
            None
        };
        let this = &*self;
        let Some(view) = &this.view else {
            return Ok(());
        };
        canvas.paint(back_buffer, |ctx| unsafe {
            if !geometry.expanded {
                this.fill_hanging(ctx, &geometry.handle, scale, &this.handle);
                return;
            }
            this.fill_hanging(ctx, &geometry.bar, scale, &this.bar);
            let radius = BUTTON_RADIUS * scale;
            for (i, (rect, text)) in geometry.buttons.iter().zip(&view.buttons).enumerate() {
                let fill = if view.hovered == Some(i) {
                    &this.hover
                } else {
                    &this.button
                };
                let rect = to_f(rect);
                ctx.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect,
                        radiusX: radius,
                        radiusY: radius,
                    },
                    fill,
                );
                let text: Vec<u16> = text.encode_utf16().collect();
                if let Some(format) = &format {
                    ctx.DrawText(
                        &text,
                        format,
                        &rect,
                        &this.label,
                        D2D1_DRAW_TEXT_OPTIONS_CLIP,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }
        })
    }

    /// Fills `rect` with only its bottom corners rounded (the top edge
    /// touches the window edge): a rounded rect starting above the window,
    /// clipped to `rect`.
    unsafe fn fill_hanging(
        &self,
        ctx: &ID2D1DeviceContext,
        rect: &Rect,
        scale: f32,
        brush: &ID2D1SolidColorBrush,
    ) {
        let radius = (BAR_RADIUS * scale).min(rect.height() as f32);
        let clip = to_f(rect);
        let mut rounded = clip;
        rounded.top -= radius;
        unsafe {
            ctx.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            ctx.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: rounded,
                    radiusX: radius,
                    radiusY: radius,
                },
                brush,
            );
            ctx.PopAxisAlignedClip();
        }
    }

    fn format(&mut self, canvas: &Canvas, dpi: u32, scale: f32) -> Result<IDWriteTextFormat> {
        if let Some((cached, format)) = &self.format
            && *cached == dpi
        {
            return Ok(format.clone());
        }
        let format = unsafe {
            let format = canvas
                .dwrite()
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    FONT_SIZE * scale,
                    w!(""),
                )
                .context("CreateTextFormat")?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            format
        };
        self.format = Some((dpi, format.clone()));
        Ok(format)
    }
}

fn to_f(r: &Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.left as f32,
        top: r.top as f32,
        right: r.right as f32,
        bottom: r.bottom as f32,
    }
}
