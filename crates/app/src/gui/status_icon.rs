//! The tray icon in its two variants: the plain one, and the same with a
//! green dot while somebody is connected.

use anyhow::{Context, Result};
use tray_icon::Icon;

use super::icon;

const BYTES_PER_PIXEL: usize = 4;
/// The dot's radius, as a fraction of the icon's width.
const DOT_RADIUS: f32 = 0.24;
/// Where the dot's centre sits, from the top-left, as a fraction of the size.
const DOT_CENTRE: f32 = 0.72;
/// The white ring around the dot, in pixels, so it shows on any taskbar.
const RING_WIDTH: f32 = 1.5;
const DOT_COLOUR: [u8; 4] = [46, 204, 113, 255];
const RING_COLOUR: [u8; 4] = [255, 255, 255, 255];

pub struct StatusIcons {
    pub idle: Icon,
    pub connected: Icon,
}

impl StatusIcons {
    pub fn load() -> Result<Self> {
        let base = icon::tray_icon().context("decoding the tray icon")?;
        let connected = with_status_dot(&base.rgba, base.width, base.height);
        Ok(Self {
            idle: Icon::from_rgba(base.rgba, base.width, base.height)
                .context("building the tray icon")?,
            connected: Icon::from_rgba(connected, base.width, base.height)
                .context("building the connected tray icon")?,
        })
    }
}

/// `rgba` (a `width`×`height` image) with a green dot with a white ring in
/// its bottom-right corner.
pub fn with_status_dot(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = rgba.to_vec();
    let (w, h) = (width as f32, height as f32);
    let radius = DOT_RADIUS * w;
    let (cx, cy) = (DOT_CENTRE * w, DOT_CENTRE * h);
    for y in 0..height {
        for x in 0..width {
            let distance = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let colour = if distance <= radius {
                DOT_COLOUR
            } else if distance <= radius + RING_WIDTH {
                RING_COLOUR
            } else {
                continue;
            };
            let at = (y * width + x) as usize * BYTES_PER_PIXEL;
            if let Some(pixel) = out.get_mut(at..at + BYTES_PER_PIXEL) {
                pixel.copy_from_slice(&colour);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(image: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = (y * width + x) as usize * BYTES_PER_PIXEL;
        image[at..at + BYTES_PER_PIXEL].try_into().unwrap()
    }

    #[test]
    fn the_dot_is_drawn_in_the_corner_and_the_rest_is_untouched() {
        let plain = vec![10u8; 32 * 32 * BYTES_PER_PIXEL];
        let dotted = with_status_dot(&plain, 32, 32);
        assert_eq!(dotted.len(), plain.len());
        assert_eq!(pixel(&dotted, 32, 23, 23), DOT_COLOUR);
        assert_eq!(pixel(&dotted, 32, 0, 0), [10; 4]);
        assert_eq!(pixel(&dotted, 32, 31, 0), [10; 4]);
        assert_eq!(pixel(&dotted, 32, 0, 31), [10; 4]);
    }

    #[test]
    fn the_dot_has_a_ring_around_it() {
        let dotted = with_status_dot(&vec![0u8; 32 * 32 * BYTES_PER_PIXEL], 32, 32);
        let radius = (DOT_RADIUS * 32.0) as u32;
        assert_eq!(pixel(&dotted, 32, 23 + radius + 1, 23), RING_COLOUR);
    }

    #[test]
    fn a_tiny_image_does_not_panic() {
        assert_eq!(with_status_dot(&[], 0, 0), Vec::<u8>::new());
        let one = with_status_dot(&[0, 0, 0, 0], 1, 1);
        assert_eq!(one.len(), 4);
    }

    #[test]
    fn both_variants_load_from_the_embedded_icon() {
        assert!(StatusIcons::load().is_ok());
    }
}
