//! The "Impostazioni" page. Every edit is sent at once as
//! [`Intent::ChangeSettings`]: there is no Save button.

use eframe::egui::{self, Align, ComboBox, Layout, RichText, TextEdit, Ui};

use crate::access_mode::AccessMode;
use crate::text;

use super::controller::Intent;
use super::model::GuiState;
use super::settings::{CodecChoice, MAX_FPS, MAX_MBPS, MIN_FPS, MIN_MBPS, MIN_PORT, Settings};
use super::view::{DETAIL_SIZE, card, error_color};

const GAP: f32 = 10.0;
const FPS_PRESETS: [u32; 8] = [15, 24, 30, 60, 90, 120, 144, 240];
const MBPS_PRESETS: [u32; 8] = [2, 5, 10, 20, 30, 50, 100, 200];
const PORT_FIELD_WIDTH: f32 = 64.0;
const PORT_DIGITS: usize = 5;

pub fn show(ui: &mut Ui, state: &GuiState, intents: &mut Vec<Intent>) {
    ui.horizontal(|ui| {
        if ui.button(format!("← {}", text::SETTINGS_BACK)).clicked() {
            intents.push(Intent::CloseSettings);
        }
        ui.label(RichText::new(text::SETTINGS_TITLE).size(16.0).strong());
    });
    ui.add_space(GAP);
    if let Some(notice) = &state.settings_notice {
        ui.colored_label(error_color(ui), notice);
        ui.add_space(GAP / 2.0);
    }
    restart_banner(ui, state, intents);

    let mut edited = state.settings.clone();
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            sharing_group(ui, state, &mut edited);
            files_group(ui, &mut edited, intents);
            viewing_group(ui, &mut edited);
            startup_group(ui, &mut edited);
            feedback_group(ui, &mut edited);
        });
    if edited != state.settings {
        intents.push(Intent::ChangeSettings(edited));
    }
}

fn restart_banner(ui: &mut Ui, state: &GuiState, intents: &mut Vec<Intent>) {
    if state.restarting {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(text::SETTINGS_RESTARTING);
        });
        ui.add_space(GAP / 2.0);
    } else if state.restart_needed() {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(text::SETTINGS_RESTART_NEEDED);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(text::SETTINGS_RESTART).clicked() {
                        intents.push(Intent::RestartSharing);
                    }
                });
            });
        });
        ui.add_space(GAP / 2.0);
    }
}

fn group(ui: &mut Ui, title: &str, hint: Option<&str>, add_rows: impl FnOnce(&mut Ui)) {
    ui.label(RichText::new(title).strong());
    if let Some(hint) = hint {
        ui.label(RichText::new(hint).size(DETAIL_SIZE).weak());
    }
    ui.add_space(4.0);
    add_rows(ui);
    ui.add_space(GAP);
}

fn sharing_group(ui: &mut Ui, state: &GuiState, edited: &mut Settings) {
    group(ui, text::SETTINGS_SHARING, None, |ui| {
        monitor_row(ui, state, edited);
        number_row(
            ui,
            text::SETTINGS_FPS,
            "settings_fps",
            &mut edited.fps,
            &FPS_PRESETS,
            (MIN_FPS, MAX_FPS),
        );
        number_row(
            ui,
            text::SETTINGS_MAX_MBPS,
            "settings_mbps",
            &mut edited.max_mbps,
            &MBPS_PRESETS,
            (MIN_MBPS, MAX_MBPS),
        );
        ui.label(
            RichText::new(text::SETTINGS_MAX_MBPS_HINT)
                .size(DETAIL_SIZE)
                .weak(),
        );
        codec_row(ui, edited);
        port_row(ui, edited);
        ui.checkbox(&mut edited.allow_input, text::SETTINGS_ALLOW_INPUT);
        ui.checkbox(&mut edited.share_clipboard, text::SETTINGS_SHARE_CLIPBOARD);
        ui.checkbox(&mut edited.share_audio, text::SETTING_SHARE_AUDIO)
            .on_hover_text(text::SETTING_SHARE_AUDIO_HINT);
        access_row(ui, edited);
    });
}

/// Files received from the other PC, as host and as viewer.
fn files_group(ui: &mut Ui, edited: &mut Settings, intents: &mut Vec<Intent>) {
    group(ui, text::SETTINGS_FILES, None, |ui| {
        ui.checkbox(&mut edited.receive_files, text::SETTING_RECEIVE_FILES);
        ui.label(
            RichText::new(text::SETTING_RECEIVE_FILES_HINT)
                .size(DETAIL_SIZE)
                .weak(),
        );
        ui.horizontal(|ui| {
            ui.label(text::SETTING_DOWNLOADS);
            if ui.button(text::SETTING_DOWNLOADS_PICK).clicked() {
                intents.push(Intent::ChooseDownloads);
            }
        });
        downloads_field(ui, edited);
        ui.label(
            RichText::new(text::SETTING_DOWNLOADS_HINT)
                .size(DETAIL_SIZE)
                .weak(),
        );
    });
}

/// The folder as typed; taken when the field loses the focus.
fn downloads_field(ui: &mut Ui, edited: &mut Settings) {
    let id = ui.make_persistent_id("settings_downloads_text");
    let mut buffer: String = ui
        .data_mut(|d| d.get_temp(id))
        .unwrap_or_else(|| edited.downloads.clone());
    let field = ui.add(TextEdit::singleline(&mut buffer).desired_width(f32::INFINITY));
    if field.lost_focus() {
        edited.downloads = buffer.trim().to_string();
    }
    if field.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, buffer));
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
    }
}

fn viewing_group(ui: &mut Ui, edited: &mut Settings) {
    group(
        ui,
        text::SETTINGS_VIEWING,
        Some(text::SETTINGS_VIEWING_HINT),
        |ui| {
            ui.checkbox(&mut edited.viewer_fullscreen, text::SETTINGS_FULLSCREEN);
            ui.checkbox(&mut edited.viewer_stats, text::SETTINGS_STATS);
            ui.checkbox(&mut edited.viewer_view_only, text::SETTINGS_VIEW_ONLY);
        },
    );
}

fn startup_group(ui: &mut Ui, edited: &mut Settings) {
    group(ui, text::SETTINGS_STARTUP, None, |ui| {
        ui.checkbox(
            &mut edited.start_with_windows,
            text::SETTINGS_START_WITH_WINDOWS,
        );
        ui.checkbox(&mut edited.start_minimized, text::SETTINGS_START_MINIMIZED);
        ui.checkbox(&mut edited.share_on_start, text::SETTINGS_SHARE_ON_START);
    });
}

fn feedback_group(ui: &mut Ui, edited: &mut Settings) {
    group(ui, text::SETTINGS_FEEDBACK, None, |ui| {
        ui.checkbox(&mut edited.notifications, text::SETTINGS_NOTIFICATIONS);
        ui.checkbox(&mut edited.connection_indicator, text::SETTINGS_INDICATOR);
    });
}

fn monitor_row(ui: &mut Ui, state: &GuiState, edited: &mut Settings) {
    ui.horizontal(|ui| {
        ui.label(text::SETTINGS_MONITOR);
        let current = state
            .monitors
            .iter()
            .find(|m| m.index == edited.monitor)
            .map_or_else(
                || text::monitor_missing(edited.monitor),
                |m| m.label.clone(),
            );
        ComboBox::from_id_salt("settings_monitor")
            .selected_text(current)
            .show_ui(ui, |ui| {
                for monitor in &state.monitors {
                    ui.selectable_value(&mut edited.monitor, monitor.index, &monitor.label);
                }
            });
    });
}

/// A pick from `presets`, plus the current value when it is not one of
/// them (a hand-edited file), within `range`.
fn number_row(
    ui: &mut Ui,
    label: &str,
    id: &str,
    value: &mut u32,
    presets: &[u32],
    range: (u32, u32),
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ComboBox::from_id_salt(id)
            .selected_text(value.to_string())
            .show_ui(ui, |ui| {
                for choice in choices(presets, *value, range) {
                    ui.selectable_value(value, choice, choice.to_string());
                }
            });
    });
}

/// `presets` within `range`, with `current` among them, ascending.
fn choices(presets: &[u32], current: u32, range: (u32, u32)) -> Vec<u32> {
    let mut choices: Vec<u32> = presets
        .iter()
        .copied()
        .chain(std::iter::once(current))
        .filter(|v| (range.0..=range.1).contains(v))
        .collect();
    choices.sort_unstable();
    choices.dedup();
    choices
}

fn access_row(ui: &mut Ui, edited: &mut Settings) {
    ui.horizontal(|ui| {
        ui.label(text::SETTINGS_ACCESS);
        ComboBox::from_id_salt("settings_access")
            .selected_text(text::access_mode(edited.access))
            .show_ui(ui, |ui| {
                for mode in AccessMode::ALL {
                    ui.selectable_value(&mut edited.access, mode, text::access_mode(mode));
                }
            });
    });
    ui.label(
        RichText::new(text::SETTINGS_ACCESS_HINT)
            .size(DETAIL_SIZE)
            .weak(),
    );
}

fn codec_row(ui: &mut Ui, edited: &mut Settings) {
    ui.horizontal(|ui| {
        ui.label(text::SETTINGS_CODEC);
        ComboBox::from_id_salt("settings_codec")
            .selected_text(edited.codec.label())
            .show_ui(ui, |ui| {
                for codec in CodecChoice::ALL {
                    ui.selectable_value(&mut edited.codec, codec, codec.label());
                }
            });
    });
}

/// The port is typed; it is applied when the field loses the keyboard,
/// and only if it is a valid port (otherwise the old one comes back).
fn port_row(ui: &mut Ui, edited: &mut Settings) {
    ui.horizontal(|ui| {
        ui.label(text::SETTINGS_PORT);
        let id = ui.make_persistent_id("settings_port_text");
        let mut buffer: String = ui
            .data_mut(|d| d.get_temp(id))
            .unwrap_or_else(|| edited.port.to_string());
        let field = ui.add(
            TextEdit::singleline(&mut buffer)
                .desired_width(PORT_FIELD_WIDTH)
                .char_limit(PORT_DIGITS),
        );
        if field.lost_focus() {
            match parse_port(&buffer) {
                Some(port) => edited.port = port,
                None => buffer = edited.port.to_string(),
            }
        }
        if field.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, buffer));
        } else {
            ui.data_mut(|d| d.remove::<String>(id));
        }
    });
}

fn parse_port(typed: &str) -> Option<u16> {
    typed.trim().parse().ok().filter(|port| *port >= MIN_PORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_include_the_current_value_once_in_order() {
        assert_eq!(choices(&[60, 30], 45, (1, 240)), [30, 45, 60]);
        assert_eq!(choices(&[60, 30], 60, (1, 240)), [30, 60]);
    }

    #[test]
    fn choices_stay_in_range() {
        assert_eq!(choices(&[2, 5, 500], 1, (2, 100)), [2, 5]);
    }

    #[test]
    fn ports_must_be_valid_numbers() {
        assert_eq!(parse_port("7420"), Some(7420));
        assert_eq!(parse_port(" 80 "), Some(80));
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("65536"), None);
        assert_eq!(parse_port("-1"), None);
        assert_eq!(parse_port("sette"), None);
        assert_eq!(parse_port(""), None);
    }
}
