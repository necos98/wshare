//! Tests of [`super::ControlBar`] w        .map(|i| rect(start + i * 100, 5, start + (i + 1) * 100, 35))th a hand-built layout.

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
/// 1000 px window: handle 440..560 x 0..12; five buttons of 100 px from
/// x = 250 to 750 (y 5..35) in a bar 200..800 x 0..40.
fn fake(window: (u32, u32), _scale: f32, count: usize, expanded: bool) -> ToolbarGeometry {
    let cx = window.0 as i32 / 2;
    let start = cx - count as i32 * 50;
    let buttons = (0..count as i32)
        .map(|i| rect(start + i * 100, 5, start + (i + 1) * 100, 35))
        .collect();
    ToolbarGeometry {
        handle: rect(cx - 60, 0, cx + 60, 12),
        bar: rect(start - 50, 0, cx + (cx - start) + 50, 40),
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
const ON_FULLSCREEN: (i32, i32) = (300, 20);
const ON_SCREEN: (i32, i32) = (400, 20);
const ON_QUALITY: (i32, i32) = (500, 20);
const ON_STATS: (i32, i32) = (600, 20);
const ON_DISCONNECT: (i32, i32) = (700, 20);
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
    assert!(mv(&mut b, ON_STATS, now).consumed);
    assert!(b.wants_arrow());
    assert_eq!(b.view().hovered, Some(3));
    mv(&mut b, VIDEO, now);
    assert!(b.next_deadline().is_some(), "and it folds away again");
}

#[test]
fn a_window_click_on_the_first_button_asks_for_full_screen() {
    let now = Instant::now();
    let mut b = windowed_bar();
    mv(&mut b, ON_HANDLE, now);
    mv(&mut b, ON_FULLSCREEN, now);
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
        [
            "Schermo intero",
            "Schermo",
            "Qualità",
            "Statistiche",
            "Disconnetti"
        ]
    );
    assert_eq!(
        windowed.tooltips,
        [
            "Schermo intero (Ctrl+Alt+Invio)",
            "Scegli quale monitor dell'altro computer guardare",
            "Scegli la qualità del video",
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
    assert_eq!(
        v.buttons,
        [
            "Finestra",
            "Schermo",
            "Qualità",
            "Statistiche",
            "Disconnetti"
        ]
    );
    assert_eq!(v.menu, None);
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
    let out = mv(&mut b, ON_STATS, now);
    assert!(out.consumed);
    assert_eq!(b.view().hovered, Some(3));
    assert_eq!(b.next_deadline(), None);
    mv(&mut b, ON_BAR_GAP, now);
    assert_eq!(b.view().hovered, None);
    assert!(b.wants_arrow());
    mv(&mut b, ON_DISCONNECT, now);
    assert_eq!(b.view().hovered, Some(4));
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
    mv(&mut b, ON_FULLSCREEN, t(300, base));
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
    mv(&mut b, ON_STATS, base);
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
        (ON_FULLSCREEN, BarAction::ToggleFullscreen),
        (ON_STATS, BarAction::ToggleStats),
        (ON_DISCONNECT, BarAction::Disconnect),
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
    mv(&mut b, ON_FULLSCREEN, now);
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
    mv(&mut b, ON_FULLSCREEN, now);
    b.button(LEFT, true, now);
    mv(&mut b, ON_STATS, now);
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
    mv(&mut b, ON_DISCONNECT, now);
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
    let out = mv(&mut b, ON_STATS, now);
    assert!(!out.consumed);
    assert!(!b.wants_arrow());
    assert_eq!(b.button(LEFT, false, now), Outcome::default());
}

#[test]
fn press_on_bar_does_not_collapse_while_held() {
    let base = Instant::now();
    let mut b = bar();
    mv(&mut b, ON_HANDLE, base);
    mv(&mut b, ON_FULLSCREEN, base);
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
    mv(&mut b, ON_DISCONNECT, now);
    assert!(b.take_changed_view().is_some());
    mv(&mut b, (701, 20), now); // same button: no change
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
    mv(&mut b, ON_STATS, now);
    assert_eq!(b.view().hovered, Some(3));
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

// ---- drop-down menus ----
//
// With the fake layout and `render::menu_geometry` at scale 1, a list
// hangs from its button's left edge at y = 44 (bar bottom + 4), with rows
// of 28 px from y = 48.

fn two_monitors() -> Vec<MonitorEntry> {
    vec![
        MonitorEntry::new("DISPLAY1", 1920, 1080, true),
        MonitorEntry::new("DISPLAY2", 2560, 1440, false),
    ]
}

/// A point on row `index` of the list under the button starting at `left`.
fn row(left: i32, index: i32) -> (i32, i32) {
    (left + 50, 48 + 28 * index + 14)
}

/// Left edges of the "Screen" and "Quality" buttons.
const SCREEN_LEFT: i32 = 350;
const QUALITY_LEFT: i32 = 450;

fn click(b: &mut ControlBar<Geo>, p: (i32, i32), now: Instant) -> Outcome {
    mv(b, p, now);
    let down = b.button(LEFT, true, now);
    assert!(down.consumed, "a press on the bar or a list is ours");
    b.button(LEFT, false, now)
}

/// A full-screen bar, expanded, that knows the host's two monitors.
fn bar_with_monitors(now: Instant) -> ControlBar<Geo> {
    let mut b = bar();
    b.set_monitors(two_monitors(), 0, now);
    mv(&mut b, ON_HANDLE, now);
    b
}

#[test]
fn the_screen_menu_lists_the_monitors_and_picks_one() {
    let now = Instant::now();
    let mut b = bar_with_monitors(now);
    assert_eq!(click(&mut b, ON_SCREEN, now).action, None, "it opens");
    let menu = b.view().menu.expect("menu open");
    assert_eq!(menu.button, 1);
    assert_eq!(
        menu.items,
        [
            "1. DISPLAY1 (1920×1080, principale)",
            "2. DISPLAY2 (2560×1440)"
        ]
    );
    assert_eq!(menu.checked, Some(0));
    let over = mv(&mut b, row(SCREEN_LEFT, 1), now);
    assert!(over.consumed && b.wants_arrow());
    assert_eq!(b.view().menu.unwrap().hovered, Some(1));
    let chosen = click(&mut b, row(SCREEN_LEFT, 1), now);
    assert_eq!(
        chosen,
        Outcome {
            consumed: true,
            action: Some(BarAction::SelectMonitor(1))
        }
    );
    assert_eq!(b.view().menu, None, "choosing closes it");
}

#[test]
fn the_screen_menu_waits_for_the_host_list() {
    let now = Instant::now();
    let mut b = bar();
    mv(&mut b, ON_HANDLE, now);
    click(&mut b, ON_SCREEN, now);
    assert_eq!(b.view().menu, None);
}

#[test]
fn the_quality_menu_picks_a_level_and_checks_it_from_then_on() {
    let now = Instant::now();
    let mut b = bar_with_monitors(now);
    click(&mut b, ON_QUALITY, now);
    let menu = b.view().menu.unwrap();
    assert_eq!(menu.items, ["Automatica", "Alta", "Media", "Bassa"]);
    assert_eq!(menu.checked, Some(0));
    let chosen = click(&mut b, row(QUALITY_LEFT, 2), now);
    assert_eq!(chosen.action, Some(BarAction::SetQuality(Quality::Medium)));
    click(&mut b, ON_QUALITY, now);
    assert_eq!(b.view().menu.unwrap().checked, Some(2));
}

#[test]
fn a_click_on_the_video_only_closes_an_open_menu() {
    let now = Instant::now();
    let mut b = bar_with_monitors(now);
    click(&mut b, ON_SCREEN, now);
    assert!(
        !mv(&mut b, VIDEO, now).consumed,
        "moves still reach the host"
    );
    let dismissed = click(&mut b, VIDEO, now);
    assert_eq!(
        dismissed,
        Outcome {
            consumed: true,
            action: None
        }
    );
    assert_eq!(b.view().menu, None);
    assert_eq!(
        b.button(LEFT, true, now),
        Outcome::default(),
        "then clicks pass"
    );
}

#[test]
fn the_menu_button_closes_its_own_menu() {
    let now = Instant::now();
    let mut b = bar_with_monitors(now);
    click(&mut b, ON_SCREEN, now);
    assert_eq!(click(&mut b, ON_SCREEN, now).action, None);
    assert_eq!(b.view().menu, None, "not reopened by the release");
}

#[test]
fn another_button_closes_the_menu_and_does_its_job() {
    let now = Instant::now();
    let mut b = bar_with_monitors(now);
    click(&mut b, ON_SCREEN, now);
    assert_eq!(
        click(&mut b, ON_STATS, now).action,
        Some(BarAction::ToggleStats)
    );
    assert_eq!(b.view().menu, None);
    click(&mut b, ON_SCREEN, now);
    click(&mut b, ON_QUALITY, now);
    assert_eq!(
        b.view().menu.unwrap().button,
        2,
        "one menu replaces the other"
    );
}

#[test]
fn an_open_menu_keeps_the_bar_open() {
    let base = Instant::now();
    let mut b = bar_with_monitors(base);
    click(&mut b, ON_SCREEN, base);
    mv(&mut b, VIDEO, base);
    assert_eq!(b.next_deadline(), None);
    b.tick(t(5000, base));
    assert!(b.view().expanded && b.view().menu.is_some());
}

#[test]
fn releasing_on_another_row_chooses_nothing() {
    let now = Instant::now();
    let mut b = bar_with_monitors(now);
    click(&mut b, ON_SCREEN, now);
    mv(&mut b, row(SCREEN_LEFT, 0), now);
    b.button(LEFT, true, now);
    mv(&mut b, row(SCREEN_LEFT, 1), now);
    assert_eq!(b.button(LEFT, false, now).action, None);
    assert!(b.view().menu.is_some(), "still open");
    // The list's padding is ours but chooses nothing.
    let padding = (SCREEN_LEFT + 50, 45);
    assert_eq!(
        click(&mut b, padding, now),
        Outcome {
            consumed: true,
            action: None
        }
    );
}

#[test]
fn a_right_click_on_a_row_is_ours_but_chooses_nothing() {
    let now = Instant::now();
    let mut b = bar_with_monitors(now);
    click(&mut b, ON_SCREEN, now);
    mv(&mut b, row(SCREEN_LEFT, 1), now);
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
fn a_new_monitor_list_updates_an_open_menu_and_an_empty_one_closes_it() {
    let now = Instant::now();
    let mut b = bar_with_monitors(now);
    click(&mut b, ON_SCREEN, now);
    b.set_monitors(two_monitors(), 1, now);
    assert_eq!(b.view().menu.unwrap().checked, Some(1));
    b.set_monitors(Vec::new(), 0, now);
    assert_eq!(b.view().menu, None);
}

#[test]
fn leaving_full_screen_closes_the_menu() {
    let now = Instant::now();
    let mut b = bar_with_monitors(now);
    click(&mut b, ON_QUALITY, now);
    b.set_fullscreen(false);
    assert_eq!(b.view().menu, None);
}
