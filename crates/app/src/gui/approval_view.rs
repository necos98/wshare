//! The request of a viewer to be accepted, at the top of the window:
//! who asks, "Ricorda questo dispositivo", and the two buttons.

use eframe::egui::{Align, Layout, RichText, Ui};

use crate::text;

use super::approval::ApprovalPrompt;
use super::controller::Intent;
use super::view::{DETAIL_SIZE, card};

const MESSAGE_SIZE: f32 = 16.0;

pub(super) fn show(ui: &mut Ui, prompt: &ApprovalPrompt, intents: &mut Vec<Intent>) {
    card(ui, |ui| {
        ui.label(RichText::new(text::APPROVAL_TITLE).size(DETAIL_SIZE).weak());
        ui.label(RichText::new(prompt.message()).size(MESSAGE_SIZE).strong());
        ui.add_space(4.0);
        let mut remember = prompt.remember;
        if ui
            .checkbox(&mut remember, text::APPROVAL_REMEMBER)
            .changed()
        {
            intents.push(Intent::SetApprovalRemember(remember));
        }
        ui.label(RichText::new(text::APPROVAL_HINT).size(DETAIL_SIZE).weak());
        ui.add_space(4.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button(text::APPROVAL_ACCEPT).clicked() {
                intents.push(Intent::AnswerApproval { accept: true });
            }
            if ui.button(text::APPROVAL_REFUSE).clicked() {
                intents.push(Intent::AnswerApproval { accept: false });
            }
        });
    });
    ui.add_space(super::view::GAP);
}
