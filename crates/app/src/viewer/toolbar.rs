//! The viewer's control bar: pure state machine (no winit, no GPU).
//!
//! A handle sits at the top centre of the window, in full screen or not;
//! reaching it (or the top edge next to it) expands a bar with the
//! [`buttons`]. Events over the bar are consumed here and not forwarded to
//! the host. Hit-testing comes from an injected geometry function
//! (production: `render::toolbar_geometry`), the same one the renderer
//! draws with.

use std::time::{Duration, Instant};

use render::{ToolbarGeometry, ToolbarHit, ToolbarView};

use crate::text;

/// The bar collapses this long after the pointer left it.
pub const COLLAPSE_DELAY: Duration = Duration::from_millis(700);
/// Height (device-independent px) of the top-edge reveal strip.
const EDGE: f32 = 2.0;
/// Horizontal slack (device-independent px) around the handle for the strip.
const EDGE_MARGIN: f32 = 24.0;

/// Mouse button id for the primary button (the only one that clicks).
pub const LEFT: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarAction {
    /// Full screen on from a window, off from full screen.
    ToggleFullscreen,
    ToggleStats,
    Disconnect,
}

/// One button of the bar: what it says, what it does and its hover text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarButton {
    pub label: &'static str,
    pub tooltip: &'static str,
    pub action: BarAction,
}

/// Number of buttons, whatever the mode.
const BUTTON_COUNT: usize = 3;

/// Buttons, left to right. The first one switches to the other mode, so it
/// names that.
pub const fn buttons(fullscreen: bool) -> [BarButton; BUTTON_COUNT] {
    let (label, tooltip) = if fullscreen {
        (text::BAR_WINDOWED, text::BAR_TIP_WINDOWED)
    } else {
        (text::BAR_FULLSCREEN, text::BAR_TIP_FULLSCREEN)
    };
    [
        BarButton {
            label,
            tooltip,
            action: BarAction::ToggleFullscreen,
        },
        BarButton {
            label: text::BAR_STATS,
            tooltip: text::BAR_TIP_STATS,
            action: BarAction::ToggleStats,
        },
        BarButton {
            label: text::BAR_DISCONNECT,
            tooltip: text::BAR_TIP_DISCONNECT,
            action: BarAction::Disconnect,
        },
    ]
}

/// Result of feeding one input event to the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Outcome {
    /// Handled locally: do not forward to the host.
    pub consumed: bool,
    pub action: Option<BarAction>,
}

struct Press {
    id: u8,
    on_bar: bool,
}

pub struct ControlBar<G> {
    geometry: G,
    window: (u32, u32),
    scale: f32,
    fullscreen: bool,
    expanded: bool,
    pointer: Option<(i32, i32)>,
    hovered: Option<usize>,
    pressed: Option<usize>,
    presses: Vec<Press>,
    deadline: Option<Instant>,
    sent: Option<ToolbarView>,
}

impl<G: Fn((u32, u32), f32, usize, bool) -> ToolbarGeometry> ControlBar<G> {
    pub fn new(geometry: G) -> Self {
        Self {
            geometry,
            window: (0, 0),
            scale: 1.0,
            fullscreen: false,
            expanded: false,
            pointer: None,
            hovered: None,
            pressed: None,
            presses: Vec::new(),
            deadline: None,
            sent: None,
        }
    }

    pub fn set_window(&mut self, size: (u32, u32), scale: f32, now: Instant) {
        self.window = size;
        self.scale = scale;
        // The bar moved under a stationary pointer.
        self.refresh(now);
    }

    /// Entering or leaving full screen. Resets the bar (collapsed) and
    /// relabels its first button.
    pub fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
        self.expanded = false;
        self.hovered = None;
        self.pressed = None;
        self.deadline = None;
        self.presses.clear();
        self.pointer = None;
    }

    pub fn pointer_moved(&mut self, x: i32, y: i32, now: Instant) -> Outcome {
        self.pointer = Some((x, y));
        // A drag that started on the video is never interrupted.
        if !self.video_drag() && !self.expanded && self.reveal_zone(x, y) {
            self.expanded = true;
        }
        self.refresh(now);
        Outcome {
            consumed: self.owns_pointer(),
            action: None,
        }
    }

    pub fn pointer_left(&mut self, now: Instant) {
        self.pointer = None;
        self.refresh(now);
    }

    pub fn button(&mut self, id: u8, pressed: bool, now: Instant) -> Outcome {
        let outcome = if pressed {
            let on_bar = !self.video_drag() && self.over_bar();
            if on_bar && id == LEFT {
                self.pressed = self.current_hit_button();
            }
            self.presses.push(Press { id, on_bar });
            Outcome {
                consumed: on_bar,
                action: None,
            }
        } else if let Some(i) = self.presses.iter().position(|p| p.id == id) {
            let press = self.presses.remove(i);
            let mut action = None;
            if press.on_bar && id == LEFT {
                if self.pressed.is_some() && self.pressed == self.current_hit_button() {
                    action = self
                        .pressed
                        .and_then(|b| buttons(self.fullscreen).get(b).map(|b| b.action));
                }
                self.pressed = None;
            }
            Outcome {
                consumed: press.on_bar,
                action,
            }
        } else {
            Outcome::default()
        };
        self.refresh(now);
        outcome
    }

    pub fn tick(&mut self, now: Instant) {
        if self.deadline.is_some_and(|d| now >= d) && !self.bar_pressed() {
            self.expanded = false;
            self.deadline = None;
            self.hovered = None;
        }
    }

    /// When [`Self::tick`] next needs to run.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// What to draw.
    pub fn view(&self) -> ToolbarView {
        let buttons = buttons(self.fullscreen);
        ToolbarView {
            buttons: buttons.iter().map(|b| b.label.to_string()).collect(),
            tooltips: buttons.iter().map(|b| b.tooltip.to_string()).collect(),
            expanded: self.expanded,
            hovered: self.hovered,
        }
    }

    /// The view if it differs from the last one returned here.
    pub fn take_changed_view(&mut self) -> Option<ToolbarView> {
        let view = self.view();
        (self.sent.as_ref() != Some(&view)).then(|| {
            self.sent = Some(view.clone());
            view
        })
    }

    /// The pointer is on the bar (not mid-drag from the video): show the
    /// normal arrow instead of the host's cursor.
    pub fn wants_arrow(&self) -> bool {
        self.owns_pointer()
    }

    fn video_drag(&self) -> bool {
        self.presses.iter().any(|p| !p.on_bar)
    }

    fn bar_pressed(&self) -> bool {
        self.presses.iter().any(|p| p.on_bar)
    }

    fn geo(&self) -> ToolbarGeometry {
        (self.geometry)(self.window, self.scale, BUTTON_COUNT, self.expanded)
    }

    fn hit(&self) -> ToolbarHit {
        self.pointer
            .map_or(ToolbarHit::None, |(x, y)| self.geo().hit(x, y))
    }

    fn current_hit_button(&self) -> Option<usize> {
        match self.hit() {
            ToolbarHit::Button(i) => Some(i),
            _ => None,
        }
    }

    fn over_bar(&self) -> bool {
        self.expanded && matches!(self.hit(), ToolbarHit::Bar | ToolbarHit::Button(_))
    }

    fn owns_pointer(&self) -> bool {
        self.bar_pressed() || (!self.video_drag() && self.over_bar())
    }

    /// The handle, or the top-edge strip next to it.
    fn reveal_zone(&self, x: i32, y: i32) -> bool {
        let geo = self.geo();
        if geo.hit(x, y) == ToolbarHit::Handle {
            return true;
        }
        let margin = (EDGE_MARGIN * self.scale) as i32;
        let edge = (EDGE * self.scale).ceil() as i32;
        y >= 0 && y < edge.max(1) && x >= geo.handle.left - margin && x < geo.handle.right + margin
    }

    /// Recomputes hover and the collapse timer from the current state.
    fn refresh(&mut self, now: Instant) {
        self.hovered = if self.expanded {
            self.current_hit_button()
        } else {
            None
        };
        let keep_open = self.bar_pressed()
            || self.over_bar()
            || self
                .pointer
                .is_some_and(|(x, y)| !self.video_drag() && self.reveal_zone(x, y));
        if !self.expanded || keep_open {
            self.deadline = None;
        } else if self.deadline.is_none() {
            self.deadline = Some(now + COLLAPSE_DELAY);
        }
    }
}

#[cfg(test)]
mod tests {
    use render::Rect;

    use super::*;

    type Geo = fn((u32, u32), f32, usize, bool) -> ToolbarGeometry;

    const WIN: (u32, u32) = (1000, 600);

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Hand-built layout, independent of the renderer's numbers. For a
    /// 1000 px window: handle 440..560 x 0..12; bar 300..700 x 0..40; three
    /// buttons of 100 px at x = 350, 450, 550 (y 5..35).
    fn fake(window: (u32, u32), _scale: f32, count: usize, expanded: bool) -> ToolbarGeometry {
        let cx = window.0 as i32 / 2;
        let buttons = (0..count as i32)
            .map(|i| rect(cx - 150 + i * 100, 5, cx - 50 + i * 100, 35))
            .collect();
        ToolbarGeometry {
            handle: rect(cx - 60, 0, cx + 60, 12),
            bar: rect(cx - 200, 0, cx + 200, 40),
            buttons: if expanded { buttons } else { Vec::new() },
            expanded,
        }
    }

    fn bar() -> ControlBar<Geo> {
        let mut b = ControlBar::new(fake as Geo);
        b.set_window(WIN, 1.0, Instant::now());
        b.set_fullscreen(true);
        b
    }

    fn windowed_bar() -> ControlBar<Geo> {
        let mut b = ControlBar::new(fake as Geo);
        b.set_window(WIN, 1.0, Instant::now());
        b
    }

    fn t(ms: u64, base: Instant) -> Instant {
        base + Duration::from_millis(ms)
    }

    const ON_HANDLE: (i32, i32) = (500, 5);
    const ON_BTN0: (i32, i32) = (400, 20);
    const ON_BTN1: (i32, i32) = (500, 20);
    const ON_BTN2: (i32, i32) = (600, 20);
    const ON_BAR_GAP: (i32, i32) = (500, 38);
    const VIDEO: (i32, i32) = (500, 300);

    fn mv(b: &mut ControlBar<Geo>, p: (i32, i32), now: Instant) -> Outcome {
        b.pointer_moved(p.0, p.1, now)
    }

    #[test]
    fn a_window_has_the_bar_too() {
        let now = Instant::now();
        let mut b = windowed_bar();
        assert_eq!(mv(&mut b, VIDEO, now), Outcome::default());
        assert!(!b.view().expanded);
        mv(&mut b, ON_HANDLE, now);
        assert!(b.view().expanded, "the handle reveals the bar");
        assert!(mv(&mut b, ON_BTN1, now).consumed);
        assert!(b.wants_arrow());
        assert_eq!(b.view().hovered, Some(1));
        mv(&mut b, VIDEO, now);
        assert!(b.next_deadline().is_some(), "and it folds away again");
    }

    #[test]
    fn a_window_click_on_the_first_button_asks_for_full_screen() {
        let now = Instant::now();
        let mut b = windowed_bar();
        mv(&mut b, ON_HANDLE, now);
        mv(&mut b, ON_BTN0, now);
        assert!(b.button(LEFT, true, now).consumed);
        let up = b.button(LEFT, false, now);
        assert_eq!(up.action, Some(BarAction::ToggleFullscreen));
    }

    #[test]
    fn a_window_click_on_the_video_still_reaches_the_host() {
        let now = Instant::now();
        let mut b = windowed_bar();
        mv(&mut b, VIDEO, now);
        assert_eq!(b.button(LEFT, true, now), Outcome::default());
        assert_eq!(b.button(LEFT, false, now), Outcome::default());
    }

    #[test]
    fn the_first_button_names_the_other_mode_and_hints_the_shortcut() {
        let windowed = windowed_bar().view();
        assert_eq!(
            windowed.buttons,
            ["Schermo intero", "Statistiche", "Disconnetti"]
        );
        assert_eq!(
            windowed.tooltips,
            [
                "Schermo intero (Ctrl+Alt+Invio)",
                "Mostra o nascondi le statistiche (Ctrl+Alt+I)",
                "Chiudi la connessione"
            ]
        );
        let fullscreen = bar().view();
        assert_eq!(fullscreen.buttons[0], "Finestra");
        assert_eq!(
            fullscreen.tooltips[0],
            "Torna alla finestra (Ctrl+Alt+Invio)"
        );
    }

    #[test]
    fn switching_mode_resets_the_bar_and_relabels_it() {
        let now = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, now);
        assert!(b.view().expanded);
        b.set_fullscreen(false);
        let view = b.view();
        assert!(!view.expanded);
        assert_eq!(view.buttons[0], "Schermo intero");
        assert_eq!(b.next_deadline(), None);
        assert!(!b.wants_arrow());
    }

    #[test]
    fn view_starts_collapsed_in_fullscreen_with_labels() {
        let b = bar();
        let v = b.view();
        assert!(!v.expanded);
        assert_eq!(v.buttons, ["Finestra", "Statistiche", "Disconnetti"]);
        assert_eq!(v.hovered, None);
    }

    #[test]
    fn pointer_on_video_does_not_reveal() {
        let mut b = bar();
        let out = mv(&mut b, VIDEO, Instant::now());
        assert_eq!(out, Outcome::default());
        assert!(!b.view().expanded);
    }

    #[test]
    fn handle_hover_reveals() {
        let mut b = bar();
        mv(&mut b, ON_HANDLE, Instant::now());
        assert!(b.view().expanded);
    }

    #[test]
    fn top_edge_near_handle_reveals() {
        let mut b = bar();
        mv(&mut b, (400, 0), Instant::now()); // beyond the 24 px margin
        assert!(!b.view().expanded);
        mv(&mut b, (430, 1), Instant::now());
        assert!(b.view().expanded);
    }

    #[test]
    fn top_edge_far_from_handle_does_not_reveal() {
        let mut b = bar();
        mv(&mut b, (50, 0), Instant::now());
        assert!(!b.view().expanded);
    }

    #[test]
    fn stays_open_over_bar_and_tracks_hover() {
        let now = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, now);
        let out = mv(&mut b, ON_BTN1, now);
        assert!(out.consumed);
        assert_eq!(b.view().hovered, Some(1));
        assert_eq!(b.next_deadline(), None);
        mv(&mut b, ON_BAR_GAP, now);
        assert_eq!(b.view().hovered, None);
        assert!(b.wants_arrow());
        mv(&mut b, ON_BTN2, now);
        assert_eq!(b.view().hovered, Some(2));
    }

    #[test]
    fn collapses_after_delay_once_pointer_leaves() {
        let base = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, base);
        let out = mv(&mut b, VIDEO, base);
        assert!(!out.consumed);
        assert_eq!(b.next_deadline(), Some(t(700, base)));
        b.tick(t(699, base));
        assert!(b.view().expanded);
        b.tick(t(700, base));
        assert!(!b.view().expanded);
        assert_eq!(b.next_deadline(), None);
    }

    #[test]
    fn returning_to_bar_cancels_collapse() {
        let base = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, base);
        mv(&mut b, VIDEO, base);
        mv(&mut b, ON_BTN0, t(300, base));
        assert_eq!(b.next_deadline(), None);
        b.tick(t(5000, base));
        assert!(b.view().expanded);
    }

    #[test]
    fn moving_on_video_does_not_postpone_collapse() {
        let base = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, base);
        mv(&mut b, VIDEO, base);
        mv(&mut b, (10, 300), t(500, base));
        assert_eq!(b.next_deadline(), Some(t(700, base)));
    }

    #[test]
    fn pointer_leaving_window_starts_collapse() {
        let base = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_BTN1, base);
        mv(&mut b, ON_HANDLE, base);
        b.pointer_left(t(10, base));
        assert_eq!(b.next_deadline(), Some(t(710, base)));
        b.tick(t(710, base));
        assert!(!b.view().expanded);
    }

    #[test]
    fn click_fires_action_on_release_over_same_button() {
        let now = Instant::now();
        for (pos, action) in [
            (ON_BTN0, BarAction::ToggleFullscreen),
            (ON_BTN1, BarAction::ToggleStats),
            (ON_BTN2, BarAction::Disconnect),
        ] {
            let mut b = bar();
            mv(&mut b, ON_HANDLE, now);
            mv(&mut b, pos, now);
            let down = b.button(LEFT, true, now);
            assert_eq!(
                down,
                Outcome {
                    consumed: true,
                    action: None
                }
            );
            let up = b.button(LEFT, false, now);
            assert_eq!(
                up,
                Outcome {
                    consumed: true,
                    action: Some(action)
                }
            );
        }
    }

    #[test]
    fn press_on_button_release_elsewhere_is_no_action_but_consumed() {
        let now = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, now);
        mv(&mut b, ON_BTN0, now);
        b.button(LEFT, true, now);
        // Dragging out of the bar: still ours while pressed.
        assert!(mv(&mut b, VIDEO, now).consumed);
        assert_eq!(b.next_deadline(), None);
        assert_eq!(
            b.button(LEFT, false, now),
            Outcome {
                consumed: true,
                action: None
            }
        );
        assert!(!mv(&mut b, VIDEO, now).consumed);
        assert!(b.next_deadline().is_some());
    }

    #[test]
    fn press_on_one_button_release_on_another_is_no_action() {
        let now = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, now);
        mv(&mut b, ON_BTN0, now);
        b.button(LEFT, true, now);
        mv(&mut b, ON_BTN1, now);
        assert_eq!(b.button(LEFT, false, now).action, None);
    }

    #[test]
    fn press_on_bar_gap_has_no_action_but_is_consumed() {
        let now = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, now);
        mv(&mut b, ON_BAR_GAP, now);
        assert_eq!(
            b.button(LEFT, true, now),
            Outcome {
                consumed: true,
                action: None
            }
        );
        assert_eq!(
            b.button(LEFT, false, now),
            Outcome {
                consumed: true,
                action: None
            }
        );
    }

    #[test]
    fn right_click_on_button_is_consumed_without_action() {
        let now = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, now);
        mv(&mut b, ON_BTN2, now);
        assert!(b.button(1, true, now).consumed);
        assert_eq!(
            b.button(1, false, now),
            Outcome {
                consumed: true,
                action: None
            }
        );
    }

    #[test]
    fn video_click_passes_through() {
        let now = Instant::now();
        let mut b = bar();
        mv(&mut b, VIDEO, now);
        assert_eq!(b.button(LEFT, true, now), Outcome::default());
        assert_eq!(b.button(LEFT, false, now), Outcome::default());
    }

    #[test]
    fn drag_from_video_over_bar_is_not_consumed_and_does_not_reveal() {
        let now = Instant::now();
        let mut b = bar();
        mv(&mut b, VIDEO, now);
        b.button(LEFT, true, now);
        let out = mv(&mut b, ON_HANDLE, now);
        assert!(!out.consumed);
        assert!(!b.view().expanded);
        assert!(!b.wants_arrow());
        assert_eq!(b.button(LEFT, false, now), Outcome::default());
    }

    #[test]
    fn drag_from_video_over_open_bar_is_not_consumed_nor_clickable() {
        let now = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, now);
        mv(&mut b, VIDEO, now);
        b.button(LEFT, true, now);
        let out = mv(&mut b, ON_BTN1, now);
        assert!(!out.consumed);
        assert!(!b.wants_arrow());
        assert_eq!(b.button(LEFT, false, now), Outcome::default());
    }

    #[test]
    fn press_on_bar_does_not_collapse_while_held() {
        let base = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, base);
        mv(&mut b, ON_BTN0, base);
        b.button(LEFT, true, base);
        mv(&mut b, VIDEO, base);
        b.tick(t(5000, base));
        assert!(b.view().expanded);
    }

    #[test]
    fn unknown_release_is_forwarded() {
        let mut b = bar();
        assert_eq!(b.button(LEFT, false, Instant::now()), Outcome::default());
    }

    #[test]
    fn changed_view_reported_once() {
        let now = Instant::now();
        let mut b = bar();
        assert!(b.take_changed_view().is_some());
        assert!(b.take_changed_view().is_none());
        mv(&mut b, ON_HANDLE, now);
        assert!(b.take_changed_view().unwrap().expanded);
        mv(&mut b, ON_BTN2, now);
        assert!(b.take_changed_view().is_some());
        mv(&mut b, (601, 20), now); // same button: no change
        assert!(b.take_changed_view().is_none());
        b.set_fullscreen(false);
        let relabelled = b.take_changed_view().unwrap();
        assert_eq!(relabelled.buttons[0], "Schermo intero");
        assert!(b.take_changed_view().is_none());
    }

    #[test]
    fn resize_moves_the_bar_under_a_still_pointer() {
        let now = Instant::now();
        let mut b = bar();
        mv(&mut b, ON_HANDLE, now);
        mv(&mut b, ON_BTN1, now);
        assert_eq!(b.view().hovered, Some(1));
        b.set_window((2000, 600), 1.0, now); // bar now centred at x=1000
        assert_eq!(b.view().hovered, None);
        assert!(b.next_deadline().is_some());
    }

    #[test]
    fn scale_scales_the_reveal_strip() {
        let now = Instant::now();
        let mut b = bar();
        b.set_window(WIN, 2.0, now);
        // Strip is 4 px tall at 2x and the margin 48 px.
        mv(&mut b, (400, 3), now);
        assert!(b.view().expanded);
    }
}
