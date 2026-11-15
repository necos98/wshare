//! Pure overlay geometry: where the box goes and how big it is, from the
//! text's measured size and the window's DPI. No Direct2D here.

/// DPI at which the style below is specified (100 % scaling).
const BASE_DPI: f32 = 96.0;

/// Where in the window an overlay sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// Statistics: out of the way of the picture's centre.
    TopLeft,
    /// Short notices: where the eye rests, above the window's bottom edge.
    BottomCenter,
}

/// Overlay look at 100 % scaling, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Style {
    pub font_size: f32,
    /// Gap between the window corner and the box.
    pub margin: f32,
    /// Gap between the box edge and the text.
    pub padding: f32,
    pub corner_radius: f32,
}

pub(crate) const STYLE: Style = Style {
    font_size: 14.0,
    margin: 8.0,
    padding: 6.0,
    corner_radius: 5.0,
};

impl Style {
    /// The style for a window at `dpi` (0, i.e. unknown, means 96).
    pub(crate) fn for_dpi(&self, dpi: u32) -> Style {
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / BASE_DPI };
        Style {
            font_size: self.font_size * scale,
            margin: (self.margin * scale).round(),
            padding: (self.padding * scale).round(),
            corner_radius: self.corner_radius * scale,
        }
    }

    /// Room left for the text in a `target` (width, height) window, at
    /// least one pixel each way so text layout never gets a zero box.
    pub(crate) fn text_space(&self, target: (u32, u32)) -> (f32, f32) {
        let inset = 2.0 * (self.margin + self.padding);
        (
            (target.0 as f32 - inset).max(1.0),
            (target.1 as f32 - inset).max(1.0),
        )
    }

    /// Places a box around text measuring `text` (width, height) pixels at
    /// `anchor` of a `target` window. The box is snapped to whole pixels
    /// (crisp edges) and never extends past the window.
    pub(crate) fn place(&self, text: (f32, f32), target: (u32, u32), anchor: Anchor) -> OverlayBox {
        let (tw, th) = (target.0 as f32, target.1 as f32);
        let width = (text.0.max(0.0) + 2.0 * self.padding).ceil().min(tw);
        let height = (text.1.max(0.0) + 2.0 * self.padding).ceil().min(th);
        let (left, top) = match anchor {
            Anchor::TopLeft => (self.margin.min(tw), self.margin.min(th)),
            Anchor::BottomCenter => (
                ((tw - width) / 2.0).floor(),
                (th - height - self.margin).max(0.0),
            ),
        };
        let right = (left + width).min(tw);
        let bottom = (top + height).min(th);
        OverlayBox {
            rect: RectF {
                left,
                top,
                right,
                bottom,
            },
            text_origin: (left + self.padding, top + self.padding),
            corner_radius: self.corner_radius,
        }
    }
}

/// A floating-point rectangle in pixels (right/bottom exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[cfg(test)]
impl RectF {
    pub(crate) fn width(&self) -> f32 {
        self.right - self.left
    }

    pub(crate) fn height(&self) -> f32 {
        self.bottom - self.top
    }
}

/// Where to draw the overlay, in back-buffer pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct OverlayBox {
    pub rect: RectF,
    /// Top-left of the text layout.
    pub text_origin: (f32, f32),
    pub corner_radius: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_dpi_keeps_style() {
        assert_eq!(STYLE.for_dpi(96), STYLE);
        assert_eq!(STYLE.for_dpi(0), STYLE, "unknown DPI means 100 %");
    }

    #[test]
    fn style_scales_with_dpi() {
        let s = STYLE.for_dpi(144); // 150 %
        assert_eq!(s.font_size, 21.0);
        assert_eq!(s.margin, 12.0);
        assert_eq!(s.padding, 9.0);
        assert_eq!(s.corner_radius, 7.5);
    }

    #[test]
    fn spacing_is_whole_pixels_at_fractional_scales() {
        let s = STYLE.for_dpi(120); // 125 %: 8 * 1.25 = 10, 6 * 1.25 = 7.5
        assert_eq!(s.margin, 10.0);
        assert_eq!(s.padding.fract(), 0.0);
    }

    #[test]
    fn box_wraps_text_with_padding_in_top_left_corner() {
        let b = STYLE.place((100.0, 40.0), (800, 600), Anchor::TopLeft);
        assert_eq!(
            b.rect,
            RectF {
                left: 8.0,
                top: 8.0,
                right: 8.0 + 100.0 + 12.0,
                bottom: 8.0 + 40.0 + 12.0,
            }
        );
        assert_eq!(b.text_origin, (14.0, 14.0));
        assert_eq!(b.corner_radius, 5.0);
    }

    #[test]
    fn box_snaps_outwards_to_whole_pixels() {
        let b = STYLE.place((100.2, 40.7), (800, 600), Anchor::TopLeft);
        assert_eq!(b.rect.right, 121.0);
        assert_eq!(b.rect.bottom, 61.0);
        assert!(b.rect.width() >= 100.2 + 12.0);
    }

    #[test]
    fn box_scales_with_dpi() {
        let s = STYLE.for_dpi(192); // 200 %
        let b = s.place((200.0, 80.0), (1600, 1200), Anchor::TopLeft);
        assert_eq!(b.rect.left, 16.0);
        assert_eq!(b.rect.width(), 200.0 + 24.0);
        assert_eq!(b.rect.height(), 80.0 + 24.0);
        assert_eq!(b.text_origin, (28.0, 28.0));
    }

    #[test]
    fn box_never_leaves_the_window() {
        let b = STYLE.place((1000.0, 500.0), (300, 200), Anchor::TopLeft);
        assert_eq!(b.rect.right, 300.0);
        assert_eq!(b.rect.bottom, 200.0);
        let tiny = STYLE.place((10.0, 10.0), (4, 4), Anchor::TopLeft);
        assert!(tiny.rect.left <= 4.0 && tiny.rect.right <= 4.0);
        assert!(tiny.rect.width() >= 0.0 && tiny.rect.height() >= 0.0);
    }

    #[test]
    fn negative_metrics_are_treated_as_empty() {
        let b = STYLE.place((-5.0, -5.0), (800, 600), Anchor::TopLeft);
        assert_eq!(b.rect.width(), 12.0);
        assert_eq!(b.rect.height(), 12.0);
    }

    #[test]
    fn a_notice_is_centred_above_the_bottom_edge() {
        let b = STYLE.place((200.0, 40.0), (800, 600), Anchor::BottomCenter);
        assert_eq!(b.rect.left, 294.0);
        assert_eq!(b.rect.right, 506.0);
        assert_eq!(b.rect.bottom, 600.0 - 8.0);
        assert_eq!(b.rect.height(), 40.0 + 12.0);
        assert_eq!(b.text_origin, (b.rect.left + 6.0, b.rect.top + 6.0));
    }

    #[test]
    fn a_notice_never_leaves_the_window() {
        let wide = STYLE.place((2000.0, 500.0), (300, 200), Anchor::BottomCenter);
        assert_eq!((wide.rect.left, wide.rect.right), (0.0, 300.0));
        assert!(wide.rect.top >= 0.0 && wide.rect.bottom <= 200.0);
        let tiny = STYLE.place((10.0, 10.0), (4, 4), Anchor::BottomCenter);
        assert!(tiny.rect.left >= 0.0 && tiny.rect.right <= 4.0);
        assert!(tiny.rect.top >= 0.0 && tiny.rect.bottom <= 4.0);
    }

    #[test]
    fn text_space_leaves_margins_and_padding() {
        assert_eq!(STYLE.text_space((800, 600)), (772.0, 572.0));
        assert_eq!(STYLE.text_space((10, 0)), (1.0, 1.0));
    }
}
