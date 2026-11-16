//! Paints the drop-down list of a control-bar button with Direct2D, using
//! exactly the rectangles of [`menu_geometry`]. Called by the toolbar
//! painter, inside its drawing pass.

use anyhow::Result;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ROUNDED_RECT, ID2D1DeviceContext, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    IDWriteTextFormat,
};

use crate::canvas::Canvas;
use crate::layout::Rect;
use crate::menu_geometry::MenuGeometry;
use crate::toolbar::MenuView;
use crate::toolbar_painter::{create_text_format, to_f};

/// Drawn in front of the row chosen now.
const CHECK_MARK: &str = "\u{2713}";
/// Corner radius of the list and of a highlighted row, at 96 DPI.
const RADIUS: f32 = 6.0;

/// The brushes a list is painted with (owned by the toolbar painter).
pub(crate) struct MenuBrushes<'a> {
    pub(crate) panel: &'a ID2D1SolidColorBrush,
    pub(crate) hover: &'a ID2D1SolidColorBrush,
    pub(crate) label: &'a ID2D1SolidColorBrush,
}

/// The text formats of the rows, for the DPI they were made for.
#[derive(Default)]
pub(crate) struct MenuPainter {
    formats: Option<(u32, MenuFormats)>,
}

/// Row labels (left-aligned) and check marks (centred in their column).
#[derive(Clone)]
pub(crate) struct MenuFormats {
    label: IDWriteTextFormat,
    check: IDWriteTextFormat,
}

impl MenuPainter {
    /// The formats for `dpi`, made once per DPI. Call before painting:
    /// creating them is not allowed inside a drawing pass.
    pub(crate) fn formats(&mut self, canvas: &Canvas, dpi: u32, scale: f32) -> Result<MenuFormats> {
        if let Some((cached, formats)) = &self.formats
            && *cached == dpi
        {
            return Ok(formats.clone());
        }
        let formats = MenuFormats {
            label: create_text_format(canvas, scale, DWRITE_TEXT_ALIGNMENT_LEADING)?,
            check: create_text_format(canvas, scale, DWRITE_TEXT_ALIGNMENT_CENTER)?,
        };
        self.formats = Some((dpi, formats.clone()));
        Ok(formats)
    }
}

/// Paints the list `view` laid out as `geometry`.
///
/// # Safety
/// `ctx` must be inside a `BeginDraw`/`EndDraw` pass (the canvas's paint).
pub(crate) unsafe fn paint_menu(
    ctx: &ID2D1DeviceContext,
    geometry: &MenuGeometry,
    view: &MenuView,
    formats: &MenuFormats,
    brushes: &MenuBrushes<'_>,
    scale: f32,
) {
    let radius = RADIUS * scale;
    let rounded = |rect: &Rect| D2D1_ROUNDED_RECT {
        rect: to_f(rect),
        radiusX: radius,
        radiusY: radius,
    };
    unsafe {
        ctx.FillRoundedRectangle(&rounded(&geometry.panel), brushes.panel);
        for (i, (row, text)) in geometry.items.iter().zip(&view.items).enumerate() {
            if view.hovered == Some(i) {
                ctx.FillRoundedRectangle(&rounded(row), brushes.hover);
            }
            let (check, label) = split_row(row);
            if view.checked == Some(i) {
                draw(ctx, CHECK_MARK, &formats.check, &check, brushes.label);
            }
            draw(ctx, text, &formats.label, &label, brushes.label);
        }
    }
}

/// A row's check-mark column (as wide as the row is tall) and the rest,
/// for the label.
fn split_row(row: &Rect) -> (Rect, Rect) {
    let column = row.height().min(row.width()).max(0);
    let check = Rect {
        right: row.left + column,
        ..*row
    };
    let label = Rect {
        left: check.right,
        ..*row
    };
    (check, label)
}

unsafe fn draw(
    ctx: &ID2D1DeviceContext,
    text: &str,
    format: &IDWriteTextFormat,
    rect: &Rect,
    brush: &ID2D1SolidColorBrush,
) {
    let text: Vec<u16> = text.encode_utf16().collect();
    let rect: D2D_RECT_F = to_f(rect);
    unsafe {
        ctx.DrawText(
            &text,
            format,
            &rect,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_keeps_a_square_column_for_the_check_mark() {
        let row = Rect {
            left: 10,
            top: 20,
            right: 210,
            bottom: 48,
        };
        let (check, label) = split_row(&row);
        assert_eq!((check.left, check.right), (10, 38));
        assert_eq!((label.left, label.right), (38, 210));
        assert_eq!((label.top, label.bottom), (20, 48));
    }

    #[test]
    fn a_row_narrower_than_tall_is_all_check_column() {
        let row = Rect {
            left: 0,
            top: 0,
            right: 5,
            bottom: 28,
        };
        let (check, label) = split_row(&row);
        assert_eq!(check.width(), 5);
        assert_eq!(label.width(), 0);
    }
}
