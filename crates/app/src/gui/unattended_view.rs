//! The «Accesso non presidiato» section of the settings page: whether it is
//! on, the optional password and the buttons that turn it on, update it or
//! turn it off (each asks Windows for administrator rights).

use eframe::egui::{RichText, TextEdit, Ui};

use crate::text;
use crate::unattended::MAX_PASSWORD_CHARS;

use super::controller::Intent;
use super::settings_view::group;
use super::unattended_state::UnattendedState;
use super::view::{DETAIL_SIZE, error_color};

/// `update_needed`: the service has other host settings than the window's
/// (see `UnattendedState::needs_update`). `password`: the field as typed,
/// kept between frames.
pub(super) fn section(
    ui: &mut Ui,
    state: &UnattendedState,
    update_needed: bool,
    password: &mut String,
    intents: &mut Vec<Intent>,
) {
    group(
        ui,
        text::UNATTENDED_TITLE,
        Some(text::UNATTENDED_HINT),
        |ui| {
            let status = match state.installed {
                Some(true) => text::UNATTENDED_ON,
                Some(false) => text::UNATTENDED_OFF,
                None => "",
            };
            if !status.is_empty() {
                ui.label(status);
            }
            if update_needed {
                ui.colored_label(error_color(ui), text::UNATTENDED_UPDATE_NEEDED);
            }
            ui.label(text::UNATTENDED_PASSWORD_LABEL);
            ui.add_enabled(
                !state.busy(),
                TextEdit::singleline(password)
                    .password(true)
                    .char_limit(MAX_PASSWORD_CHARS),
            );
            ui.label(
                RichText::new(text::UNATTENDED_PASSWORD_HINT)
                    .size(DETAIL_SIZE)
                    .weak(),
            );
            buttons(ui, state, password, intents);
            if state.busy() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(text::UNATTENDED_WORKING);
                });
            }
            if let Some(notice) = &state.notice {
                ui.colored_label(error_color(ui), notice);
            }
        },
    );
}

fn buttons(ui: &mut Ui, state: &UnattendedState, password: &mut String, intents: &mut Vec<Intent>) {
    let installed = state.installed == Some(true);
    let enable = if installed {
        text::UNATTENDED_UPDATE
    } else {
        text::UNATTENDED_ENABLE
    };
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!state.busy(), |ui| {
            let clicked = ui.button(enable);
            let clicked = if installed {
                clicked.on_hover_text(text::UNATTENDED_UPDATE_HINT)
            } else {
                clicked
            };
            if clicked.clicked() {
                // The password goes to the controller once and is not kept
                // in the field.
                intents.push(Intent::EnableUnattended {
                    password: std::mem::take(password),
                });
            }
            if installed && ui.button(text::UNATTENDED_DISABLE).clicked() {
                intents.push(Intent::DisableUnattended);
            }
        });
    });
}
