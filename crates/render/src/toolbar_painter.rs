//! Paints the control bar with Direct2D through the shared [`Canvas`],
//! using exactly the rectangles of [`toolbar_geometry`], and the drop-down
//! list of a button when one is open (see `menu_painter`).

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ROUNDED_RECT,
    ID2D1DeviceContext, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
    IDWriteTextFormat, IDWriteTextLayout,
};
use windows::core::w;

use crate::canvas::Canvas;
use crate::layout::Rect;
use crate::menu_geometry::menu_geometry;
use crate::menu_painter::{MenuBrushes, MenuPainter, paint_menu};
use crate::toolbar::ToolbarView;
use crate::toolbar_geometry::{ToolbarGeometry, toolbar_geometry, tooltip_rect};

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
    menu: MenuPainter,
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
            menu: MenuPainter::default(),
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
        let menu = self
            .view
            .as_ref()
            .and_then(|v| v.menu.as_ref())
            .and_then(|menu| {
                menu_geometry(&geometry, menu.button, menu.items.len(), size, scale)
                    .map(|layout| (menu, layout))
            });
        let menu_formats = match menu {
            Some(_) => Some(self.menu.formats(canvas, dpi, scale)?),
            None => None,
        };
        // An open list covers where a hover text would go.
        let tooltip = match menu {
            Some(_) => None,
            None => self.tooltip(canvas, &geometry, size, scale, format.as_ref())?,
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
            if let Some(tooltip) = &tooltip {
                ctx.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: to_f(&tooltip.rect),
                        radiusX: radius,
                        radiusY: radius,
                    },
                    &this.bar,
                );
                ctx.DrawTextLayout(
                    tooltip.text_origin,
                    &tooltip.layout,
                    &this.label,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                );
            }
            if let (Some((menu, layout)), Some(formats)) = (menu, &menu_formats) {
                let brushes = MenuBrushes {
                    panel: &this.bar,
                    hover: &this.hover,
                    label: &this.label,
                };
                paint_menu(ctx, &layout, menu, formats, &brushes, scale);
            }
        })
    }

    /// The hover text of the hovered button, laid out and placed; `None`
    /// when no button is hovered or it has no text.
    fn tooltip(
        &self,
        canvas: &Canvas,
        geometry: &ToolbarGeometry,
        size: (u32, u32),
        scale: f32,
        format: Option<&IDWriteTextFormat>,
    ) -> Result<Option<Tooltip>> {
        let (Some(view), Some(format)) = (&self.view, format) else {
            return Ok(None);
        };
        let Some(button) = view.hovered else {
            return Ok(None);
        };
        let Some(text) = view.tooltips.get(button).filter(|text| !text.is_empty()) else {
            return Ok(None);
        };
        let text: Vec<u16> = text.encode_utf16().collect();
        let layout = unsafe {
            let layout = canvas
                .dwrite()
                .CreateTextLayout(&text, format, size.0 as f32, size.1 as f32)
                .context("CreateTextLayout")?;
            layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            layout
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
        let Some(rect) = tooltip_rect(
            geometry,
            button,
            (metrics.width, metrics.height),
            size,
            scale,
        ) else {
            return Ok(None);
        };
        let text_origin = windows_numerics::Vector2 {
            X: rect.left as f32 + (rect.width() as f32 - metrics.width) / 2.0,
            Y: rect.top as f32 + (rect.height() as f32 - metrics.height) / 2.0,
        };
        Ok(Some(Tooltip {
            rect,
            layout,
            text_origin,
        }))
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
        let format = create_text_format(canvas, scale, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        self.format = Some((dpi, format.clone()));
        Ok(format)
    }
}

/// The bar's font at `scale`, on one line, vertically centred in its box
/// and aligned horizontally as `alignment` says.
pub(crate) fn create_text_format(
    canvas: &Canvas,
    scale: f32,
    alignment: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    unsafe {
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
        format.SetTextAlignment(alignment)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(format)
    }
}

/// The hover text of a button, ready to draw.
struct Tooltip {
    rect: Rect,
    layout: IDWriteTextLayout,
    text_origin: windows_numerics::Vector2,
}

pub(crate) fn to_f(r: &Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.left as f32,
        top: r.top as f32,
        right: r.right as f32,
        bottom: r.bottom as f32,
    }
}
