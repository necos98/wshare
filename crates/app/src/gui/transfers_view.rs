//! The files part of the main page: sending files to the viewer connected
//! here or from a viewer window to its host, the transfers in progress
//! with their "Annulla", and how the last one ended with "Apri cartella".

use eframe::egui::{Align, Layout, RichText, Ui};
use protocol::transfer::TransferId;

use crate::text;

use super::controller::Intent;
use super::model::{Sharing, ViewerSession};
use super::transfers::FileTransfers;
use super::view::DETAIL_SIZE;

/// For the viewer connected to this PC's sharing.
pub fn host_files(ui: &mut Ui, sharing: &Sharing, intents: &mut Vec<Intent>) {
    if sharing.viewer.is_some() && ui.button(text::SEND_TO_VIEWER).clicked() {
        intents.push(Intent::SendToViewer);
    }
    transfers(ui, &sharing.files, intents, Intent::CancelHostTransfer);
}

/// For a viewer window launched from here, once connected.
pub fn viewer_files(ui: &mut Ui, viewer: &ViewerSession, intents: &mut Vec<Intent>) {
    let id = viewer.id;
    ui.horizontal(|ui| {
        ui.add_space(INDENT);
        if ui.small_button(text::SEND_FROM_VIEWER).clicked() {
            intents.push(Intent::SendFromViewer(id));
        }
    });
    transfers(ui, &viewer.files, intents, |transfer| {
        Intent::CancelViewerTransfer {
            viewer: id,
            transfer,
        }
    });
}

/// Room left of the rows of a viewer, under its status dot.
const INDENT: f32 = 18.0;

/// The transfers in progress, each with its "Annulla", then the end of
/// the last one.
fn transfers(
    ui: &mut Ui,
    files: &FileTransfers,
    intents: &mut Vec<Intent>,
    cancel: impl Fn(Option<TransferId>) -> Intent,
) {
    for transfer in files.active() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(text::transfer_row(transfer)).size(DETAIL_SIZE));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button(text::CANCEL_TRANSFER).clicked() {
                    intents.push(cancel(Some(transfer.info.id)));
                }
            });
        });
    }
    let Some(last) = &files.last else {
        return;
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(last).size(DETAIL_SIZE).weak());
        if let Some(folder) = &files.received {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button(text::OPEN_FOLDER).clicked() {
                    intents.push(Intent::OpenFolder(folder.clone()));
                }
            });
        }
    });
}
