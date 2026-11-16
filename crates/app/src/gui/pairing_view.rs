//! The parts of the window about remembered PCs: the authorized devices of
//! the sharing card and the form of a host that remembers this PC.

use eframe::egui::{self, Align, Layout, RichText, Ui};

use crate::text;
use crate::trust::{AuthorizedDevice, Timestamp};

use super::controller::Intent;
use super::elapsed;
use super::host_row::HostRow;
use super::view::{DETAIL_SIZE, error_color};

/// "Dispositivi autorizzati": who connects without a PIN, with a way to
/// remove each one. Nothing while the list is empty.
pub(super) fn authorized_devices(
    ui: &mut Ui,
    devices: &[AuthorizedDevice],
    now: Timestamp,
    intents: &mut Vec<Intent>,
) {
    if devices.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.separator();
    ui.label(RichText::new(text::AUTHORIZED_DEVICES).strong());
    ui.label(
        RichText::new(text::AUTHORIZED_HINT)
            .size(DETAIL_SIZE)
            .weak(),
    );
    for device in devices {
        ui.push_id(device.fingerprint.0, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(&device.name);
                    let last = elapsed::ago(device.last_access, now);
                    ui.label(
                        RichText::new(text::last_access(&last))
                            .size(DETAIL_SIZE)
                            .weak(),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(text::REMOVE).clicked() {
                        intents.push(Intent::RemoveDevice(device.fingerprint));
                    }
                });
            });
        });
    }
}

/// What the user asked for on a remembered host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RememberedAction {
    Connect,
    Forget,
}

/// `Accesso memorizzato [Connetti] [Rimuovi]`: no PIN field.
/// Under the PIN of a host that does not remember this PC: connects without
/// one so the person at the host can accept it. True when clicked.
pub(super) fn ask_access(ui: &mut Ui) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        clicked = ui.small_button(text::ASK_ACCESS).clicked();
    });
    ui.add_space(4.0);
    clicked
}

pub(super) fn remembered_form(ui: &mut Ui, error: Option<&str>) -> Option<RememberedAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.weak(text::HOST_REMEMBERED);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .button(text::REMOVE)
                .on_hover_text(text::FORGET_HINT)
                .clicked()
            {
                action = Some(RememberedAction::Forget);
            }
            if ui.add(egui::Button::new(text::CONNECT)).clicked() {
                action = Some(RememberedAction::Connect);
            }
        });
    });
    if let Some(error) = error {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new(error)
                    .size(DETAIL_SIZE)
                    .color(error_color(ui)),
            );
        });
    }
    ui.add_space(4.0);
    action
}

/// Under a computer this PC knows: `[Preferito] [Riattiva]` and, once
/// Wake-on-LAN went out, what happened. `Riattiva` is for a computer that
/// is not online and needs its MAC; without one it says why it is off.
pub(super) fn known_actions(ui: &mut Ui, row: &HostRow, notice: Option<&str>) -> Option<Intent> {
    let mut intent = None;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let favorite = ui
            .selectable_label(row.favorite, text::FAVORITE)
            .on_hover_text(text::FAVORITE_HINT);
        if favorite.clicked() {
            intent = Some(Intent::SetFavorite {
                id: row.id.clone(),
                favorite: !row.favorite,
            });
        }
        if row.wakeable() {
            let wake = ui
                .add_enabled(row.mac.is_some(), egui::Button::new(text::WAKE))
                .on_hover_text(text::WAKE_HINT)
                .on_disabled_hover_text(text::WAKE_NO_MAC);
            if wake.clicked() {
                intent = Some(Intent::WakeHost { id: row.id.clone() });
            }
        }
    });
    if let Some(notice) = notice {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new(notice).size(DETAIL_SIZE).weak());
        });
    }
    ui.add_space(4.0);
    intent
}
