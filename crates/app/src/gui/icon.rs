//! The window icon, taken from the same `.ico` embedded in the executable.

use eframe::egui::IconData;

const ICO: &[u8] = include_bytes!("../../assets/wshare.ico");
/// Size picked for the window (Windows scales it for title bar and taskbar).
const WINDOW_ICON_SIZE: u32 = 64;

pub fn window_icon() -> Option<IconData> {
    let png = png_entry(ICO, WINDOW_ICON_SIZE)?;
    eframe::icon_data::from_png_bytes(png).ok()
}

/// The PNG-encoded image of an `.ico` file closest in size to `wanted`
/// (BMP-encoded entries are skipped). Pure.
pub fn png_entry(ico: &[u8], wanted: u32) -> Option<&[u8]> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(ico.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(ico.get(at..at + 4)?.try_into().ok()?));
    if u16_at(0)? != 0 || u16_at(2)? != 1 {
        return None;
    }
    let count = usize::from(u16_at(4)?);
    (0..count)
        .filter_map(|i| {
            let entry = 6 + 16 * i;
            // A stored size of 0 means 256.
            let size = match *ico.get(entry)? {
                0 => 256,
                n => u32::from(n),
            };
            let len = usize::try_from(u32_at(entry + 8)?).ok()?;
            let offset = usize::try_from(u32_at(entry + 12)?).ok()?;
            let image = ico.get(offset..offset.checked_add(len)?)?;
            image
                .starts_with(b"\x89PNG")
                .then_some((size.abs_diff(wanted), image))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, image)| image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_has_a_png_close_to_the_wanted_size() {
        let png = png_entry(ICO, 64).expect("png entry");
        let icon = eframe::icon_data::from_png_bytes(png).unwrap();
        assert_eq!((icon.width, icon.height), (64, 64));
        let small = eframe::icon_data::from_png_bytes(png_entry(ICO, 16).unwrap()).unwrap();
        assert_eq!(small.width, 16);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(png_entry(b"", 32), None);
        assert_eq!(png_entry(b"not an icon at all", 32), None);
        // Valid header, entry pointing outside the file.
        let mut ico = vec![0, 0, 1, 0, 1, 0];
        ico.extend([32, 32, 0, 0, 1, 0, 32, 0, 100, 0, 0, 0, 22, 0, 0, 0]);
        assert_eq!(png_entry(&ico, 32), None);
    }
}
