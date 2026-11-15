//! What the renderer is asked to draw for the control bar. CONTRACT.

/// The bar's visual state, decided by the app. The renderer draws it on
/// every present until changed (like the statistics overlay), using
/// [`crate::toolbar_geometry`] with the window's current size and DPI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolbarView {
    /// Short labels, left to right (e.g. "Finestra", "Statistiche",
    /// "Disconnetti"). Drawn centred in each button rect.
    pub buttons: Vec<String>,
    /// Hover text of each button, same order as `buttons` (an empty text or
    /// a missing one: none). Drawn under the hovered button.
    pub tooltips: Vec<String>,
    pub expanded: bool,
    /// Button under the pointer, drawn highlighted.
    pub hovered: Option<usize>,
}
