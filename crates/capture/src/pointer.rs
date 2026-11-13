//! Mouse pointer shapes: conversion from what DXGI Desktop Duplication
//! reports to a plain straight-alpha RGBA image, plus change tracking.
//!
//! Desktop Duplication never draws the pointer into the captured frame; it
//! hands out the pointer's *shape* separately. The host forwards that shape so
//! the viewer can use it as its own local cursor (zero-latency pointer).
//!
//! Everything here is pure (no Windows calls), so it is unit-tested with
//! hand-built buffers; `lib.rs` only does the DXGI plumbing.
//!
//! # Formats
//! * **Colour**: 32bpp BGRA with an alpha channel, in practice
//!   *premultiplied* (the docs do not say). Evidence from a live capture of
//!   the Windows 11 arrow (`live_pointer_shape` in `lib.rs`): along its soft
//!   edge the colour grows in proportion to alpha (B/A ~= 0.077 at A = 26,
//!   49, 77) instead of staying constant, and no channel ever exceeds alpha.
//!   It is therefore un-premultiplied, unless some channel exceeds its alpha:
//!   such data cannot be premultiplied, so it is kept as straight alpha
//!   (this guards against drivers/cursors that do send straight alpha).
//! * **Masked colour**: 32bpp BGRX where the alpha byte is a mask: `0x00`
//!   means "replace the screen with this RGB" (opaque), `0xFF` means "XOR this
//!   RGB with the screen".
//! * **Monochrome**: a 1bpp AND mask followed by a 1bpp XOR mask, both with
//!   the reported pitch; the reported height covers *both* masks, so the
//!   cursor is half as tall. Per pixel (AND, XOR): (0,0) black, (0,1) white,
//!   (1,0) transparent, (1,1) invert the screen.
//!
//! # XOR approximation
//! An RGBA image cannot express "XOR with whatever is underneath". Pixels
//! that would change the screen by XOR (masked colour with non-zero RGB,
//! monochrome invert) become **opaque black**, and every fully transparent
//! pixel touching one of them (8-neighbourhood) becomes **opaque white**. The
//! classic inverting cursors (I-beam, crosshair) thus render as black strokes
//! with a white outline, readable on both light and dark backgrounds, much
//! like the macOS I-beam. XOR with black (RGB 0) changes nothing and is
//! therefore transparent.
//!
//! # Size limit
//! Shapes larger than [`MAX_POINTER_SIDE`] on either axis are *clipped*, not
//! scaled (scaling would blur 1px strokes; such cursors are vanishingly rare,
//! Windows' largest accessibility cursor is exactly 256). The kept window is
//! anchored at the top-left, shifted only as far as needed to keep the
//! hotspot inside, so the click point stays exact.

use std::fmt;

/// Largest width/height of a [`PointerImage`] (Windows' biggest cursor size).
pub const MAX_POINTER_SIDE: u32 = 256;

/// The three `DXGI_OUTDUPL_POINTER_SHAPE_TYPE` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    Monochrome,
    Color,
    MaskedColor,
}

impl ShapeKind {
    /// Maps the raw `DXGI_OUTDUPL_POINTER_SHAPE_INFO::Type` value.
    pub fn from_dxgi(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::Monochrome),
            2 => Some(Self::Color),
            4 => Some(Self::MaskedColor),
            _ => None,
        }
    }
}

/// A pointer shape exactly as DXGI reports it (borrowed buffer).
#[derive(Debug, Clone, Copy)]
pub struct RawPointerShape<'a> {
    pub kind: ShapeKind,
    pub width: u32,
    /// For [`ShapeKind::Monochrome`] this is twice the cursor height (AND
    /// mask + XOR mask), as in `DXGI_OUTDUPL_POINTER_SHAPE_INFO`.
    pub height: u32,
    /// Bytes per row.
    pub pitch: u32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    pub data: &'a [u8],
}

/// A pointer shape ready to be shown as a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerImage {
    /// Content hash (FNV-1a over dimensions, hotspot and pixels): equal
    /// shapes get equal ids, so consumers can dedupe and cache.
    pub id: u64,
    /// 1..=[`MAX_POINTER_SIDE`].
    pub width: u16,
    /// 1..=[`MAX_POINTER_SIDE`].
    pub height: u16,
    /// Always `< width`.
    pub hotspot_x: u16,
    /// Always `< height`.
    pub hotspot_y: u16,
    /// Straight (non-premultiplied) RGBA, row-major, `width * height * 4`.
    pub rgba: Vec<u8>,
}

/// Why a raw shape could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerShapeError {
    /// Zero width or height.
    Empty,
    /// Monochrome shape whose height is not an even number of rows.
    OddMonochromeHeight(u32),
    /// Pitch smaller than one row of pixels.
    PitchTooSmall { pitch: u32, row_bytes: u64 },
    /// The buffer holds fewer bytes than the dimensions require.
    BufferTooShort { needed: u64, len: usize },
}

impl fmt::Display for PointerShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "pointer shape has zero size"),
            Self::OddMonochromeHeight(h) => {
                write!(f, "monochrome pointer height {h} is not even")
            }
            Self::PitchTooSmall { pitch, row_bytes } => {
                write!(f, "pointer pitch {pitch} < row size {row_bytes}")
            }
            Self::BufferTooShort { needed, len } => {
                write!(f, "pointer buffer has {len} bytes, needs {needed}")
            }
        }
    }
}

impl std::error::Error for PointerShapeError {}

/// What one pixel does to the screen, before approximating XOR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Px {
    /// Straight RGBA (alpha 0 = transparent).
    Rgba([u8; 4]),
    /// XORs the screen with something non-zero.
    Xor,
}

const TRANSPARENT: Px = Px::Rgba([0; 4]);
const BLACK: [u8; 4] = [0, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// Converts a DXGI pointer shape into a straight-alpha RGBA image (see the
/// module docs for the per-format rules). Never panics or reads out of
/// bounds: malformed input yields an error.
pub fn convert(raw: &RawPointerShape) -> Result<PointerImage, PointerShapeError> {
    let (width, height) = (raw.width, raw.height);
    if width == 0 || height == 0 {
        return Err(PointerShapeError::Empty);
    }
    let (cursor_height, row_bytes) = match raw.kind {
        ShapeKind::Monochrome => {
            if height % 2 != 0 {
                return Err(PointerShapeError::OddMonochromeHeight(height));
            }
            (height / 2, u64::from(width).div_ceil(8))
        }
        ShapeKind::Color | ShapeKind::MaskedColor => (height, u64::from(width) * 4),
    };
    if u64::from(raw.pitch) < row_bytes {
        return Err(PointerShapeError::PitchTooSmall {
            pitch: raw.pitch,
            row_bytes,
        });
    }
    // The last row need not be padded to the full pitch.
    let needed = u64::from(raw.pitch) * u64::from(height - 1) + row_bytes;
    if (raw.data.len() as u64) < needed {
        return Err(PointerShapeError::BufferTooShort {
            needed,
            len: raw.data.len(),
        });
    }

    let (x0, out_w, hotspot_x) = clip_axis(width, raw.hotspot_x);
    let (y0, out_h, hotspot_y) = clip_axis(cursor_height, raw.hotspot_y);
    let mut pixels: Vec<Px> = Vec::with_capacity(out_w as usize * out_h as usize);
    for y in y0..y0 + out_h {
        for x in x0..x0 + out_w {
            // In bounds: x < width, y < cursor_height <= height, checked above.
            pixels.push(match raw.kind {
                ShapeKind::Color => color_px(raw, x, y),
                ShapeKind::MaskedColor => masked_color_px(raw, x, y),
                ShapeKind::Monochrome => monochrome_px(raw, x, y, cursor_height),
            });
        }
    }
    if raw.kind == ShapeKind::Color && looks_premultiplied(&pixels) {
        unpremultiply(&mut pixels);
    }
    let rgba = approximate_xor(&pixels, out_w as usize, out_h as usize);
    // Each value is <= MAX_POINTER_SIDE (256), so the casts are lossless.
    Ok(PointerImage::new(
        out_w as u16,
        out_h as u16,
        hotspot_x as u16,
        hotspot_y as u16,
        rgba,
    ))
}

/// For one axis of length `len` (> 0) with the hotspot at `hotspot`, returns
/// `(first kept index, kept length, hotspot within the kept range)`. The
/// hotspot is clamped into the shape first.
fn clip_axis(len: u32, hotspot: i32) -> (u32, u32, u32) {
    let hotspot = u32::try_from(hotspot).unwrap_or(0).min(len - 1);
    let kept = len.min(MAX_POINTER_SIDE);
    let start = hotspot.saturating_sub(kept - 1).min(len - kept);
    (start, kept, hotspot - start)
}

fn bgra_at(raw: &RawPointerShape, x: u32, y: u32) -> [u8; 4] {
    let i = y as usize * raw.pitch as usize + x as usize * 4;
    let p = &raw.data[i..i + 4];
    [p[0], p[1], p[2], p[3]]
}

fn color_px(raw: &RawPointerShape, x: u32, y: u32) -> Px {
    let [b, g, r, a] = bgra_at(raw, x, y);
    Px::Rgba([r, g, b, a])
}

/// False if some colour channel exceeds its alpha, which premultiplied data
/// cannot contain.
fn looks_premultiplied(pixels: &[Px]) -> bool {
    pixels.iter().all(|px| match *px {
        Px::Rgba([r, g, b, a]) => r <= a && g <= a && b <= a,
        Px::Xor => true,
    })
}

fn unpremultiply(pixels: &mut [Px]) {
    for px in pixels {
        if let Px::Rgba(c) = px
            && c[3] != 0
            && c[3] != 255
        {
            let a = u32::from(c[3]);
            for ch in &mut c[..3] {
                // Rounded; <= 255 because ch <= a was checked.
                *ch = ((u32::from(*ch) * 255 + a / 2) / a) as u8;
            }
        }
    }
}

fn masked_color_px(raw: &RawPointerShape, x: u32, y: u32) -> Px {
    let [b, g, r, mask] = bgra_at(raw, x, y);
    match (mask, [r, g, b]) {
        (0, _) => Px::Rgba([r, g, b, 255]),
        // XOR with zero leaves the screen untouched.
        (_, [0, 0, 0]) => TRANSPARENT,
        _ => Px::Xor,
    }
}

fn monochrome_px(raw: &RawPointerShape, x: u32, y: u32, cursor_height: u32) -> Px {
    let bit = |row: u32| {
        let byte = raw.data[row as usize * raw.pitch as usize + x as usize / 8];
        (byte >> (7 - x % 8)) & 1 == 1
    };
    match (bit(y), bit(y + cursor_height)) {
        (false, false) => Px::Rgba(BLACK),
        (false, true) => Px::Rgba(WHITE),
        (true, false) => TRANSPARENT,
        (true, true) => Px::Xor,
    }
}

/// Renders XOR pixels as opaque black and outlines them in opaque white
/// (transparent 8-neighbours only), see the module docs.
fn approximate_xor(pixels: &[Px], width: usize, height: usize) -> Vec<u8> {
    let is_xor = |x: usize, y: usize| pixels[y * width + x] == Px::Xor;
    let touches_xor = |x: usize, y: usize| {
        (y.saturating_sub(1)..=(y + 1).min(height - 1))
            .any(|ny| (x.saturating_sub(1)..=(x + 1).min(width - 1)).any(|nx| is_xor(nx, ny)))
    };
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for (i, px) in pixels.iter().enumerate() {
        let (x, y) = (i % width, i / width);
        rgba.extend_from_slice(&match *px {
            Px::Xor => BLACK,
            Px::Rgba([.., 0]) if touches_xor(x, y) => WHITE,
            Px::Rgba(c) => c,
        });
    }
    rgba
}

impl PointerImage {
    fn new(width: u16, height: u16, hotspot_x: u16, hotspot_y: u16, rgba: Vec<u8>) -> Self {
        let mut hash = Fnv1a::new();
        for v in [width, height, hotspot_x, hotspot_y] {
            hash.write(&v.to_le_bytes());
        }
        hash.write(&rgba);
        Self {
            id: hash.finish(),
            width,
            height,
            hotspot_x,
            hotspot_y,
            rgba,
        }
    }
}

/// 64-bit FNV-1a: stable across runs and builds (unlike `DefaultHasher`),
/// which matters because the id is sent to other machines.
struct Fnv1a(u64);

impl Fnv1a {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// What changed about the pointer since the previous
/// [`PointerTracker::take_changes`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PointerChanges {
    /// A shape different from the last one reported.
    pub shape: Option<PointerImage>,
    /// The new visibility, if it differs from the last one reported. `false`
    /// also when the pointer is on another monitor.
    pub visible: Option<bool>,
}

impl PointerChanges {
    pub fn is_empty(&self) -> bool {
        self.shape.is_none() && self.visible.is_none()
    }
}

/// Accumulates pointer observations between polls and reports only real
/// changes, so the consumer can forward them without its own bookkeeping.
#[derive(Debug, Default)]
pub struct PointerTracker {
    pending_shape: Option<PointerImage>,
    reported_shape_id: Option<u64>,
    visible: Option<bool>,
    reported_visible: Option<bool>,
}

impl PointerTracker {
    pub fn observe_shape(&mut self, shape: PointerImage) {
        self.pending_shape = Some(shape);
    }

    pub fn observe_visibility(&mut self, visible: bool) {
        self.visible = Some(visible);
    }

    /// Returns what changed since the last call. A shape that ends up equal
    /// (same id) to the last reported one is dropped, e.g. the shape DXGI
    /// re-sends after the duplication is recreated.
    pub fn take_changes(&mut self) -> PointerChanges {
        let shape = self
            .pending_shape
            .take()
            .filter(|s| Some(s.id) != self.reported_shape_id);
        if let Some(s) = &shape {
            self.reported_shape_id = Some(s.id);
        }
        let visible = self.visible.filter(|&v| Some(v) != self.reported_visible);
        if visible.is_some() {
            self.reported_visible = visible;
        }
        PointerChanges { shape, visible }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(
        kind: ShapeKind,
        width: u32,
        height: u32,
        pitch: u32,
        data: &[u8],
    ) -> RawPointerShape<'_> {
        RawPointerShape {
            kind,
            width,
            height,
            pitch,
            hotspot_x: 0,
            hotspot_y: 0,
            data,
        }
    }

    /// Builds a 32bpp buffer from BGRA pixels, padding each row to `pitch`.
    fn bgra_buffer(rows: &[&[[u8; 4]]], pitch: usize) -> Vec<u8> {
        let mut data = Vec::new();
        for row in rows {
            let start = data.len();
            for p in *row {
                data.extend_from_slice(p);
            }
            data.resize(start + pitch, 0xAB); // garbage padding
        }
        data
    }

    fn pixel(img: &PointerImage, x: usize, y: usize) -> [u8; 4] {
        let i = (y * img.width as usize + x) * 4;
        img.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn color_swizzles_and_unpremultiplies() {
        let data = bgra_buffer(&[&[[1, 2, 3, 255], [10, 20, 30, 128], [0, 0, 0, 0]]], 12);
        let img = convert(&raw(ShapeKind::Color, 3, 1, 12, &data)).unwrap();
        assert_eq!((img.width, img.height), (3, 1));
        // 30 * 255 / 128 = 59.8 -> 60, etc. (rounded).
        assert_eq!(img.rgba, vec![3, 2, 1, 255, 60, 40, 20, 128, 0, 0, 0, 0]);
    }

    #[test]
    fn color_unpremultiply_saturates_at_full_intensity() {
        let data = bgra_buffer(&[&[[7, 0, 7, 7]]], 4);
        let img = convert(&raw(ShapeKind::Color, 1, 1, 4, &data)).unwrap();
        assert_eq!(img.rgba, vec![255, 0, 255, 7]);
    }

    #[test]
    fn color_that_cannot_be_premultiplied_is_kept_straight() {
        // 200 > alpha 100 somewhere: the whole image is straight alpha.
        let data = bgra_buffer(&[&[[10, 20, 30, 128], [200, 0, 0, 100]]], 8);
        let img = convert(&raw(ShapeKind::Color, 2, 1, 8, &data)).unwrap();
        assert_eq!(img.rgba, vec![30, 20, 10, 128, 0, 0, 200, 100]);
    }

    #[test]
    fn masked_color_is_never_unpremultiplied() {
        let data = bgra_buffer(&[&[[10, 20, 30, 0x00]]], 4);
        let img = convert(&raw(ShapeKind::MaskedColor, 1, 1, 4, &data)).unwrap();
        assert_eq!(img.rgba, vec![30, 20, 10, 255]);
    }

    #[test]
    fn color_skips_pitch_padding() {
        let data = bgra_buffer(
            &[
                &[[1, 1, 1, 255], [2, 2, 2, 255]],
                &[[3, 3, 3, 255], [4, 4, 4, 255]],
            ],
            12,
        );
        // Last row unpadded is accepted.
        let img = convert(&raw(ShapeKind::Color, 2, 2, 12, &data[..20])).unwrap();
        assert_eq!(pixel(&img, 0, 1), [3, 3, 3, 255]);
        assert_eq!(pixel(&img, 1, 1), [4, 4, 4, 255]);
        assert_eq!(img.rgba.len(), 16);
    }

    #[test]
    fn masked_color_replace_and_xor() {
        let data = bgra_buffer(
            &[&[
                [10, 20, 30, 0x00],    // replace -> opaque colour
                [0, 0, 0, 0xFF],       // XOR black -> no-op -> transparent
                [255, 255, 255, 0xFF], // invert -> black
                [0, 0, 0, 0xFF],       // no-op, but next to the invert -> outline
                [0, 0, 0, 0xFF],       // no-op, far -> transparent
            ]],
            20,
        );
        let img = convert(&raw(ShapeKind::MaskedColor, 5, 1, 20, &data)).unwrap();
        assert_eq!(pixel(&img, 0, 0), [30, 20, 10, 255]);
        assert_eq!(pixel(&img, 1, 0), WHITE, "transparent neighbour of XOR");
        assert_eq!(pixel(&img, 2, 0), BLACK);
        assert_eq!(pixel(&img, 3, 0), WHITE);
        assert_eq!(pixel(&img, 4, 0), [0; 4]);
    }

    /// Monochrome masks for a 16-wide cursor (2 bytes per row, pitch 4).
    fn mono_buffer(and: &[u16], xor: &[u16]) -> Vec<u8> {
        and.iter()
            .chain(xor)
            .flat_map(|row| {
                let [hi, lo] = row.to_be_bytes();
                [hi, lo, 0xEE, 0xEE] // padded pitch
            })
            .collect()
    }

    #[test]
    fn monochrome_truth_table() {
        // Columns 0..4: (AND,XOR) = (0,0) (0,1) (1,0) (1,1); rest transparent.
        let and = [0b0011_1111_1111_1111];
        let xor = [0b0101_0000_0000_0000];
        let data = mono_buffer(&and, &xor);
        let img = convert(&raw(ShapeKind::Monochrome, 16, 2, 4, &data)).unwrap();
        assert_eq!((img.width, img.height), (16, 1), "height is halved");
        assert_eq!(pixel(&img, 0, 0), BLACK);
        assert_eq!(pixel(&img, 1, 0), WHITE);
        assert_eq!(pixel(&img, 2, 0), WHITE, "outline of the inverted pixel");
        assert_eq!(pixel(&img, 3, 0), BLACK, "inverted pixel");
        assert_eq!(pixel(&img, 4, 0), WHITE, "outline");
        assert_eq!(pixel(&img, 5, 0), [0; 4]);
    }

    #[test]
    fn monochrome_i_beam_gets_white_outline_on_all_sides() {
        // 3 rows; the inverting stroke is the middle pixel of column 5.
        let and = [0xFFFF; 3];
        let xor = [0, 1 << (15 - 5), 0];
        let data = mono_buffer(&and, &xor);
        let img = convert(&raw(ShapeKind::Monochrome, 16, 6, 4, &data)).unwrap();
        assert_eq!(pixel(&img, 5, 1), BLACK);
        for (x, y) in [
            (4, 0),
            (5, 0),
            (6, 0),
            (4, 1),
            (6, 1),
            (4, 2),
            (5, 2),
            (6, 2),
        ] {
            assert_eq!(pixel(&img, x, y), WHITE, "({x},{y})");
        }
        assert_eq!(pixel(&img, 7, 1), [0; 4]);
        assert_eq!(pixel(&img, 3, 1), [0; 4]);
    }

    #[test]
    fn outline_never_covers_opaque_pixels() {
        let data = bgra_buffer(&[&[[9, 9, 9, 0x00], [255, 255, 255, 0xFF]]], 8);
        let img = convert(&raw(ShapeKind::MaskedColor, 2, 1, 8, &data)).unwrap();
        assert_eq!(pixel(&img, 0, 0), [9, 9, 9, 255]);
        assert_eq!(pixel(&img, 1, 0), BLACK);
    }

    #[test]
    fn monochrome_width_not_multiple_of_8() {
        // 3 pixels wide: one byte per row, pitch 1.
        let data = [0b0000_0000, 0b0110_0000];
        let img = convert(&raw(ShapeKind::Monochrome, 3, 2, 1, &data)).unwrap();
        assert_eq!(img.rgba, [BLACK, WHITE, WHITE].concat());
    }

    #[test]
    fn hotspot_is_kept_and_clamped() {
        let data = vec![0; 4 * 4 * 4];
        let mut shape = raw(ShapeKind::Color, 4, 4, 16, &data);
        shape.hotspot_x = 2;
        shape.hotspot_y = 3;
        let img = convert(&shape).unwrap();
        assert_eq!((img.hotspot_x, img.hotspot_y), (2, 3));
        shape.hotspot_x = 99;
        shape.hotspot_y = -5;
        let img = convert(&shape).unwrap();
        assert_eq!((img.hotspot_x, img.hotspot_y), (3, 0));
    }

    #[test]
    fn oversized_shape_is_clipped_around_the_hotspot() {
        let (w, h) = (300u32, 260u32);
        let mut data = vec![0u8; (w * h * 4) as usize];
        // Mark pixel (x, y) with its coordinates to check the window.
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                data[i..i + 4].copy_from_slice(&[(x % 256) as u8, (y % 256) as u8, 0, 255]);
            }
        }
        let mut shape = raw(ShapeKind::Color, w, h, w * 4, &data);
        let img = convert(&shape).unwrap();
        assert_eq!((img.width, img.height), (256, 256));
        assert_eq!(pixel(&img, 0, 0), [0, 0, 0, 255], "top-left anchored");
        assert_eq!(img.rgba.len(), 256 * 256 * 4);

        shape.hotspot_x = 290;
        shape.hotspot_y = 10;
        let img = convert(&shape).unwrap();
        // Window x = 35..291 keeps the hotspot on its last column.
        assert_eq!((img.hotspot_x, img.hotspot_y), (255, 10));
        assert_eq!(pixel(&img, 0, 0), [0, 0, 35, 255]);
    }

    #[test]
    fn oversized_monochrome_clips_each_mask_separately() {
        // 8 wide, 300 tall cursor (600 mask rows); row 299 is white.
        let mut data = vec![0u8; 600];
        data[300 + 299] = 0xFF;
        let mut shape = raw(ShapeKind::Monochrome, 8, 600, 1, &data);
        shape.hotspot_y = 299;
        let img = convert(&shape).unwrap();
        assert_eq!((img.height, img.hotspot_y), (256, 255));
        assert_eq!(pixel(&img, 0, 255), WHITE);
        assert_eq!(pixel(&img, 0, 254), BLACK);
    }

    #[test]
    fn malformed_inputs_are_errors() {
        let data = [0u8; 64];
        let err = |kind, w, h, pitch, len: usize| convert(&raw(kind, w, h, pitch, &data[..len]));
        assert_eq!(
            err(ShapeKind::Color, 0, 4, 16, 64),
            Err(PointerShapeError::Empty)
        );
        assert_eq!(
            err(ShapeKind::Color, 4, 0, 16, 64),
            Err(PointerShapeError::Empty)
        );
        assert!(matches!(
            err(ShapeKind::Color, 4, 1, 15, 64),
            Err(PointerShapeError::PitchTooSmall { .. })
        ));
        assert!(matches!(
            err(ShapeKind::Color, 4, 4, 16, 63),
            Err(PointerShapeError::BufferTooShort {
                needed: 64,
                len: 63
            })
        ));
        assert!(matches!(
            err(ShapeKind::Monochrome, 8, 3, 1, 64),
            Err(PointerShapeError::OddMonochromeHeight(3))
        ));
        assert!(matches!(
            err(ShapeKind::Monochrome, 9, 2, 1, 64),
            Err(PointerShapeError::PitchTooSmall { .. })
        ));
        assert!(matches!(
            err(ShapeKind::MaskedColor, 1, 1, 4, 3),
            Err(PointerShapeError::BufferTooShort { .. })
        ));
        // Huge dimensions must not overflow or allocate.
        assert!(matches!(
            err(ShapeKind::Color, u32::MAX, u32::MAX, u32::MAX, 64),
            Err(PointerShapeError::PitchTooSmall { .. })
        ));
        assert!(matches!(
            err(ShapeKind::Monochrome, 8, u32::MAX - 1, u32::MAX, 64),
            Err(PointerShapeError::BufferTooShort { .. })
        ));
    }

    #[test]
    fn shape_kind_from_dxgi() {
        assert_eq!(ShapeKind::from_dxgi(1), Some(ShapeKind::Monochrome));
        assert_eq!(ShapeKind::from_dxgi(2), Some(ShapeKind::Color));
        assert_eq!(ShapeKind::from_dxgi(4), Some(ShapeKind::MaskedColor));
        assert_eq!(ShapeKind::from_dxgi(3), None);
    }

    #[test]
    fn id_is_a_stable_content_hash() {
        let a = PointerImage::new(1, 1, 0, 0, vec![1, 2, 3, 4]);
        assert_eq!(a.id, PointerImage::new(1, 1, 0, 0, vec![1, 2, 3, 4]).id);
        assert_ne!(a.id, PointerImage::new(1, 1, 0, 0, vec![1, 2, 3, 5]).id);
        // Hotspot is part of the identity.
        let b = PointerImage::new(2, 1, 0, 0, vec![0; 8]);
        assert_ne!(b.id, PointerImage::new(2, 1, 1, 0, vec![0; 8]).id);
        // The id crosses the network, so the hash must be the real FNV-1a.
        let mut h = Fnv1a::new();
        h.write(b"a");
        assert_eq!(h.finish(), 0xaf63_dc4c_8601_ec8c, "FNV-1a test vector");
    }

    #[test]
    fn tracker_reports_only_changes() {
        let a = PointerImage::new(1, 1, 0, 0, vec![0; 4]);
        let b = PointerImage::new(1, 1, 0, 0, vec![255; 4]);
        let mut t = PointerTracker::default();
        assert!(t.take_changes().is_empty());

        t.observe_shape(a.clone());
        t.observe_visibility(true);
        assert_eq!(
            t.take_changes(),
            PointerChanges {
                shape: Some(a.clone()),
                visible: Some(true)
            }
        );
        assert!(t.take_changes().is_empty());

        // Same shape re-sent (e.g. after access lost) and same visibility.
        t.observe_shape(a.clone());
        t.observe_visibility(true);
        assert!(t.take_changes().is_empty());

        // Latest observation wins; A -> B -> A between polls is no change.
        t.observe_shape(b.clone());
        t.observe_shape(a.clone());
        t.observe_visibility(false);
        t.observe_visibility(true);
        assert!(t.take_changes().is_empty());

        t.observe_shape(b.clone());
        t.observe_visibility(false);
        assert_eq!(
            t.take_changes(),
            PointerChanges {
                shape: Some(b),
                visible: Some(false)
            }
        );
    }
}
