//! The notification-area icon: shows the status (its icon changes while
//! somebody is connected), offers the quick actions and the balloon
//! notifications, and keeps the app reachable while its window is hidden.
//!
//! Its events are delivered by `tray-icon` on the UI thread's message loop
//! and forwarded to the [`Inbox`], whose waker asks eframe for a frame:
//! eframe then runs `App::logic` even while the window is hidden.

use anyhow::{Context, Result};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::HWND;

use crate::text;

use super::balloon;
use super::model::GuiState;
use super::notifications::Notification;
use super::ports::{AppEvent, Inbox, TrayAction};
use super::status_icon::StatusIcons;

pub struct Tray {
    icon: TrayIcon,
    icons: StatusIcons,
    sharing_item: CheckMenuItem,
    disconnect_item: MenuItem,
    new_pin_item: MenuItem,
    input_item: CheckMenuItem,
    /// What the icon last showed, to touch it only when it changes.
    shown: Shown,
}

/// The parts of the state the icon mirrors.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    tooltip: String,
    /// The connected viewer's name.
    viewer: Option<String>,
}

impl Shown {
    fn of(state: &GuiState) -> Self {
        Self {
            tooltip: state.tray_tooltip(),
            viewer: state.sharing.viewer.as_ref().map(|v| v.name.clone()),
        }
    }
}

impl Tray {
    /// Creates the icon; must run on the UI thread (it owns the event loop).
    pub fn new(inbox: &Inbox) -> Result<Self> {
        let open = MenuItem::new(text::TRAY_OPEN, true, None);
        let sharing_item = CheckMenuItem::new(text::TRAY_SHARING, true, false, None);
        let disconnect_item = MenuItem::new(text::DISCONNECT, false, None);
        let new_pin_item = MenuItem::new(text::TRAY_NEW_PIN, false, None);
        let input_item = CheckMenuItem::new(text::TRAY_ALLOW_INPUT, true, false, None);
        let quit = MenuItem::new(text::TRAY_QUIT, true, None);
        let menu = Menu::with_items(&[
            &open,
            &sharing_item,
            &PredefinedMenuItem::separator(),
            &disconnect_item,
            &new_pin_item,
            &input_item,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let actions: Vec<(MenuId, TrayAction)> = vec![
            (open.id().clone(), TrayAction::Open),
            (sharing_item.id().clone(), TrayAction::ToggleSharing),
            (disconnect_item.id().clone(), TrayAction::DisconnectViewer),
            (new_pin_item.id().clone(), TrayAction::NewPin),
            (input_item.id().clone(), TrayAction::ToggleInputAllowed),
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

        let icons = StatusIcons::load()?;
        let initial = GuiState::default();
        let shown = Shown::of(&initial);
        let icon = TrayIconBuilder::new()
            .with_icon(icons.idle.clone())
            .with_tooltip(&shown.tooltip)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .context("creating tray icon")?;
        Ok(Self {
            icon,
            icons,
            sharing_item,
            disconnect_item,
            new_pin_item,
            input_item,
            shown,
        })
    }

    /// Mirrors the state on the icon and the menu (only touching what
    /// changed).
    pub fn sync(&mut self, state: &GuiState) {
        let now = Shown::of(state);
        if now != self.shown {
            self.show(&now);
            self.shown = now;
        }
        // Clicking a check item toggles it by itself: always re-assert.
        self.sharing_item.set_checked(state.sharing.enabled);
        self.sharing_item.set_enabled(!state.sharing.stopping);
        self.input_item.set_checked(state.settings.allow_input);
        self.new_pin_item.set_enabled(state.sharing.listening);
    }

    fn show(&self, now: &Shown) {
        if now.tooltip != self.shown.tooltip {
            let _ = self.icon.set_tooltip(Some(&now.tooltip));
        }
        if now.viewer != self.shown.viewer {
            let icon = match now.viewer {
                Some(_) => &self.icons.connected,
                None => &self.icons.idle,
            };
            let _ = self.icon.set_icon(Some(icon.clone()));
            self.disconnect_item.set_enabled(now.viewer.is_some());
            self.disconnect_item.set_text(match &now.viewer {
                Some(name) => text::tray_disconnect(name),
                None => text::DISCONNECT.to_string(),
            });
        }
    }

    /// Pops a balloon out of the icon.
    pub fn notify(&self, notification: &Notification) {
        let window = HWND(self.icon.window_handle());
        balloon::show(window, &notification.title, &notification.body);
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
