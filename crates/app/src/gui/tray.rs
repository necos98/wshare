//! The notification-area icon: shows the status and keeps the app
//! reachable while its window is hidden.
//!
//! Its events are delivered by `tray-icon` on the UI thread's message loop
//! and forwarded to the [`Inbox`], whose waker asks eframe for a frame:
//! eframe then runs `App::logic` even while the window is hidden.

use anyhow::{Context, Result};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::text;

use super::model::GuiState;
use super::ports::{AppEvent, Inbox, TrayAction};

/// Resource id of the application icon (see build.rs / winresource).
const ICON_RESOURCE: u16 = 1;

pub struct Tray {
    icon: TrayIcon,
    sharing_item: CheckMenuItem,
    tooltip: String,
}

impl Tray {
    /// Creates the icon; must run on the UI thread (it owns the event loop).
    pub fn new(inbox: &Inbox) -> Result<Self> {
        let open = MenuItem::new(text::TRAY_OPEN, true, None);
        let sharing_item = CheckMenuItem::new(text::TRAY_SHARING, true, false, None);
        let quit = MenuItem::new(text::TRAY_QUIT, true, None);
        let menu = Menu::with_items(&[
            &open,
            &sharing_item,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let actions: Vec<(MenuId, TrayAction)> = vec![
            (open.id().clone(), TrayAction::Open),
            (sharing_item.id().clone(), TrayAction::ToggleSharing),
            (quit.id().clone(), TrayAction::Quit),
        ];
        let menu_inbox = inbox.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, action)) = actions.iter().find(|(id, _)| *id == event.id) {
                menu_inbox.send(AppEvent::Tray(*action));
            }
        }));
        let click_inbox = inbox.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if opens_window(&event) {
                click_inbox.send(AppEvent::Tray(TrayAction::Open));
            }
        }));

        let tooltip = GuiState::default().tray_tooltip();
        let icon = TrayIconBuilder::new()
            .with_icon(Icon::from_resource(ICON_RESOURCE, None).context("loading tray icon")?)
            .with_tooltip(&tooltip)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .context("creating tray icon")?;
        Ok(Self {
            icon,
            sharing_item,
            tooltip,
        })
    }

    /// Mirrors the state on the icon (only touching what changed).
    pub fn sync(&mut self, state: &GuiState) {
        let tooltip = state.tray_tooltip();
        if tooltip != self.tooltip {
            let _ = self.icon.set_tooltip(Some(&tooltip));
            self.tooltip = tooltip;
        }
        // Clicking a check item toggles it by itself: always re-assert.
        self.sharing_item.set_checked(state.sharing.enabled);
        self.sharing_item.set_enabled(!state.sharing.stopping);
    }
}

/// A left click (on release) or a double click opens the window.
fn opens_window(event: &TrayIconEvent) -> bool {
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state,
            ..
        } => *button_state == MouseButtonState::Up,
        TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } => true,
        _ => false,
    }
}
