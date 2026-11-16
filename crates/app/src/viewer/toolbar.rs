//! The viewer's control bar: pure state machine (no winit, no GPU).
//!
//! A handle sits at the top centre of the window, in full screen or not;
//! reaching it (or the top edge next to it) expands a bar with the
//! [`buttons`]. Two of them open a drop-down list (see `bar_menu`): the
//! host's monitors and the video quality; choosing a row is a
//! [`BarAction`] like a button click. Events over the bar or an open list
//! are consumed here and not forwarded to the host; a click elsewhere while
//! a list is open only closes it. Hit-testing comes from an injected
//! geometry function (production: `render::toolbar_geometry`), the same one
//! the renderer draws with, and from `render::menu_geometry` for the list.

use std::time::{Duration, Instant};

use protocol::session::{MonitorEntry, Quality};
use render::{MenuGeometry, MenuHit, ToolbarGeometry, ToolbarHit, ToolbarView, menu_geometry};

use super::bar_menu::{MenuKind, OpenMenu, SessionChoices};
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
    /// Watch the host's monitor at this position of its list.
    SelectMonitor(u32),
    SetQuality(Quality),
}

/// What a button does when clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonRole {
    Act(BarAction),
    /// Opens (or closes) this menu under the button.
    Open(MenuKind),
}

/// One button of the bar: what it says, what it does and its hover text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarButton {
    pub label: &'static str,
    pub tooltip: &'static str,
    pub role: ButtonRole,
}

/// Number of buttons, whatever the mode.
const BUTTON_COUNT: usize = 5;

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
            role: ButtonRole::Act(BarAction::ToggleFullscreen),
        },
        BarButton {
            label: text::BAR_SCREEN,
            tooltip: text::BAR_TIP_SCREEN,
            role: ButtonRole::Open(MenuKind::Screen),
        },
        BarButton {
            label: text::BAR_QUALITY,
            tooltip: text::BAR_TIP_QUALITY,
            role: ButtonRole::Open(MenuKind::Quality),
        },
        BarButton {
            label: text::BAR_STATS,
            tooltip: text::BAR_TIP_STATS,
            role: ButtonRole::Act(BarAction::ToggleStats),
        },
        BarButton {
            label: text::BAR_DISCONNECT,
            tooltip: text::BAR_TIP_DISCONNECT,
            role: ButtonRole::Act(BarAction::Disconnect),
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
    choices: SessionChoices,
    menu: Option<OpenMenu>,
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
            choices: SessionChoices::default(),
            menu: None,
            sent: None,
        }
    }

    pub fn set_window(&mut self, size: (u32, u32), scale: f32, now: Instant) {
        self.window = size;
        self.scale = scale;
        // The bar moved under a stationary pointer.
        self.refresh(now);
    }

    /// Entering or leaving full screen. Resets the bar (collapsed, no menu
    /// open) and relabels its first button.
    pub fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
        self.expanded = false;
        self.hovered = None;
        self.pressed = None;
        self.deadline = None;
        self.presses.clear();
        self.pointer = None;
        self.menu = None;
    }

    /// The host's monitors and the one it shares, for the "Screen" menu.
    pub fn set_monitors(&mut self, monitors: Vec<MonitorEntry>, current: u32, now: Instant) {
        self.choices.set_monitors(monitors, current);
        if self
            .menu
            .is_some_and(|menu| self.choices.len(menu.kind) == 0)
        {
            self.menu = None;
        }
        self.refresh(now);
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
            self.press(id)
        } else if let Some(i) = self.presses.iter().position(|p| p.id == id) {
            let press = self.presses.remove(i);
            let action = (press.on_bar && id == LEFT).then(|| self.click()).flatten();
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
            self.menu = None;
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
            menu: self.menu.map(|menu| menu.view(&self.choices)),
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

    /// The pointer is on the bar or an open list (not mid-drag from the
    /// video): show the normal arrow instead of the host's cursor.
    pub fn wants_arrow(&self) -> bool {
        self.owns_pointer()
    }

    /// A button went down: on the bar or a list it is ours. With a list
    /// open, a press anywhere off the list closes it and is ours too (it
    /// only dismisses), except on another bar button, which it presses.
    fn press(&mut self, id: u8) -> Outcome {
        let hit_button = self.current_hit_button();
        let mut on_bar = !self.video_drag() && self.over_bar();
        if let Some(menu) = self.menu {
            match self.menu_hit() {
                MenuHit::Item(row) => {
                    self.press_row((id == LEFT).then_some(row));
                    on_bar = true;
                }
                MenuHit::Panel => {
                    self.press_row(None);
                    on_bar = true;
                }
                MenuHit::None => {
                    self.menu = None;
                    if hit_button == Some(menu.button) || !on_bar {
                        // Closing it was the whole click.
                        self.presses.push(Press { id, on_bar: true });
                        return Outcome {
                            consumed: true,
                            action: None,
                        };
                    }
                }
            }
        }
        if on_bar && id == LEFT && self.menu.is_none() {
            self.pressed = hit_button;
        }
        self.presses.push(Press { id, on_bar });
        Outcome {
            consumed: on_bar,
            action: None,
        }
    }

    /// The primary button came up after going down on the bar or a list:
    /// the action of the row or button it went down on, if still under the
    /// pointer. Choosing a row closes its list; a menu button opens one.
    fn click(&mut self) -> Option<BarAction> {
        if let Some(menu) = self.menu {
            let chosen = menu
                .pressed
                .filter(|row| self.menu_hit() == MenuHit::Item(*row))?;
            self.menu = None;
            let action = self.choices.action(menu.kind, chosen)?;
            if let BarAction::SetQuality(quality) = action {
                self.choices.set_quality(quality);
            }
            return Some(action);
        }
        let pressed = self.pressed.take()?;
        if self.current_hit_button() != Some(pressed) {
            return None;
        }
        match buttons(self.fullscreen).get(pressed)?.role {
            ButtonRole::Act(action) => Some(action),
            ButtonRole::Open(kind) => {
                if self.choices.len(kind) > 0 {
                    self.menu = Some(OpenMenu::new(kind, pressed));
                }
                None
            }
        }
    }

    /// Records the row of the open list the primary button went down on.
    fn press_row(&mut self, row: Option<usize>) {
        if let Some(menu) = &mut self.menu {
            menu.pressed = row;
        }
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

    /// Where the open list lies, if one is open.
    fn menu_geo(&self) -> Option<MenuGeometry> {
        let menu = self.menu?;
        menu_geometry(
            &self.geo(),
            menu.button,
            self.choices.len(menu.kind),
            self.window,
            self.scale,
        )
    }

    fn menu_hit(&self) -> MenuHit {
        match (self.pointer, self.menu_geo()) {
            (Some((x, y)), Some(geometry)) => geometry.hit(x, y),
            _ => MenuHit::None,
        }
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

    fn over_menu(&self) -> bool {
        self.menu_hit() != MenuHit::None
    }

    fn owns_pointer(&self) -> bool {
        self.bar_pressed() || (!self.video_drag() && (self.over_bar() || self.over_menu()))
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

    /// Recomputes hover and the collapse timer from the current state. An
    /// open list keeps the bar open.
    fn refresh(&mut self, now: Instant) {
        self.hovered = if self.expanded {
            self.current_hit_button()
        } else {
            None
        };
        let row = match self.menu_hit() {
            MenuHit::Item(row) => Some(row),
            _ => None,
        };
        if let Some(menu) = &mut self.menu {
            menu.hovered = row;
        }
        let keep_open = self.menu.is_some()
            || self.bar_pressed()
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
mod tests;
