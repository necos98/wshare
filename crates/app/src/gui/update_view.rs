//! The «Aggiornamenti» section of the settings page (version, check,
//! «Aggiorna e riavvia») and the line on the main page that says a new
//! version is out.

use eframe::egui::{RichText, Ui};

use crate::text;
use crate::update;

use super::controller::Intent;
use super::settings::Settings;
use super::settings_view::group;
use super::update_state::{UpdatePhase, UpdateState};
use super::view::{DETAIL_SIZE, error_color};

/// `edited`: the settings as edited on the page (the start-up check).
pub(super) fn section(
    ui: &mut Ui,
    state: &UpdateState,
    edited: &mut Settings,
    intents: &mut Vec<Intent>,
) {
    group(ui, text::UPDATE_TITLE, None, |ui| {
        ui.label(text::update_current(update::current_version()));
        ui.checkbox(&mut edited.check_updates, text::UPDATE_CHECK_ON_START);
        match &state.phase {
            UpdatePhase::Unknown => {}
            UpdatePhase::Checking => busy(ui, text::UPDATE_CHECKING),
            UpdatePhase::UpToDate => {
                ui.label(text::UPDATE_UP_TO_DATE);
            }
            UpdatePhase::Available(available) => {
                ui.label(RichText::new(text::update_available(available.version())).strong());
                notes(ui, available.notes());
                if ui
                    .button(text::UPDATE_INSTALL)
                    .on_hover_text(text::UPDATE_INSTALL_HINT)
                    .clicked()
                {
                    intents.push(Intent::InstallUpdate);
                }
            }
            UpdatePhase::Installing(_) => busy(ui, text::UPDATE_INSTALLING),
            UpdatePhase::RestartByHand(_) => {}
        }
        if !state.busy() && state.available().is_none() && ui.button(text::UPDATE_CHECK).clicked() {
            intents.push(Intent::CheckUpdates);
        }
        if let Some(notice) = &state.notice {
            ui.colored_label(error_color(ui), notice);
        }
        if state.service_outdated {
            ui.colored_label(error_color(ui), text::UPDATE_SERVICE_OUTDATED);
        }
    });
}

fn busy(ui: &mut Ui, label: &str) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(label);
    });
}

fn notes(ui: &mut Ui, notes: &str) {
    let notes = notes.trim();
    if !notes.is_empty() {
        ui.label(RichText::new(notes).size(DETAIL_SIZE).weak());
    }
}

/// On the main page: a new version is out; the click opens the settings.
pub(super) fn badge(ui: &mut Ui, state: &UpdateState, intents: &mut Vec<Intent>) {
    if let Some(available) = state.available() {
        ui.add_space(4.0);
        if ui
            .link(text::update_badge(available.version()))
            .on_hover_text(text::UPDATE_BADGE_TIP)
            .clicked()
        {
            intents.push(Intent::OpenSettings);
        }
    }
}
