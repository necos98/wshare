//! Draws the window from the [`GuiState`] and turns clicks into
//! [`Intent`]s. Holds only the text being typed ([`Drafts`]).

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, Response, RichText, Role, Sense,
    StrokeKind, TextEdit, Ui, WidgetInfo, pos2, vec2,
};

use crate::text;
use crate::trust::Timestamp;

use super::approval_view;
use super::controller::Intent;
use super::model::{GuiState, HostRow, Sharing, ViewerSession, ViewerStatus, format_pin};
use super::pairing_view::{self, RememberedAction};
use super::settings_state::Page;
use super::settings_view;

/// Text fields being edited; not part of the application state.
#[derive(Debug, Default)]
pub struct Drafts {
    host_pin: String,
    address: String,
    address_pin: String,
    /// Move the keyboard to the PIN field of the host just selected.
    focus_host_pin: bool,
    /// Which form the last connection request came from, to show its error there.
    last_form: Form,
    /// The `GuiState::pin_prompts` already acted on.
    seen_pin_prompts: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Form {
    #[default]
    Host,
    Address,
}

pub(super) const GAP: f32 = 10.0;
/// Font size of secondary lines (details, warnings).
pub(super) const DETAIL_SIZE: f32 = 12.0;

pub fn show(ui: &mut Ui, state: &GuiState, drafts: &mut Drafts) -> Vec<Intent> {
    let mut intents = Vec::new();
    if state.page == Page::Settings {
        settings_view::show(ui, state, &mut intents);
        return intents;
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            if let Some(prompt) = &state.sharing.approval {
                approval_view::show(ui, prompt, &mut intents);
            }
            sharing_section(ui, &state.sharing, &mut intents);
            if let Some(notice) = &state.settings_notice {
                ui.add_space(4.0);
                ui.colored_label(error_color(ui), notice);
            }
            ui.add_space(GAP);
            ui.separator();
            ui.add_space(GAP / 2.0);
            network_section(ui, state, drafts, &mut intents);
            viewers_section(ui, &state.viewers, &mut intents);
        });
    intents
}

fn sharing_section(ui: &mut Ui, sharing: &Sharing, intents: &mut Vec<Intent>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(text::SHARE_THIS_SCREEN).size(16.0).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .button("⚙")
                .on_hover_text(text::SETTINGS_OPEN_TIP)
                .clicked()
            {
                intents.push(Intent::OpenSettings);
            }
            ui.add_space(4.0);
            let mut on = sharing.enabled;
            // Off but still winding down: cannot be switched on yet.
            ui.add_enabled_ui(!sharing.stopping, |ui| {
                if toggle(ui, &mut on, text::SHARE_THIS_SCREEN).changed() {
                    intents.push(Intent::SetSharing(on));
                }
            });
        });
    });
    if let Some(error) = &sharing.error {
        ui.add_space(4.0);
        ui.colored_label(error_color(ui), error);
    }
    if !sharing.enabled {
        ui.add_space(4.0);
        if sharing.stopping {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(sharing.off_line());
            });
        } else {
            ui.weak(sharing.off_line());
        }
        return;
    }
    ui.add_space(GAP / 2.0);
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.weak("PIN");
                let pin = sharing
                    .pin
                    .as_deref()
                    .map_or_else(|| "– – –".to_string(), format_pin);
                ui.label(RichText::new(pin).monospace().size(34.0).strong());
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(sharing.listening, egui::Button::new(text::NEW_PIN))
                    .clicked()
                {
                    intents.push(Intent::NewPin);
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let connected = sharing.viewer.is_some();
            if sharing.listening {
                status_dot(
                    ui,
                    if connected {
                        ok_color(ui)
                    } else {
                        ui.visuals().weak_text_color()
                    },
                );
            } else {
                ui.spinner();
            }
            ui.label(sharing.status_line());
            if connected {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(text::DISCONNECT).clicked() {
                        intents.push(Intent::DisconnectViewer);
                    }
                });
            }
        });
        if let Some(screen) = &sharing.screen {
            ui.label(RichText::new(screen).size(DETAIL_SIZE).weak());
        }
        ui.add_space(4.0);
        let mut allowed = sharing.input_allowed;
        if ui
            .checkbox(&mut allowed, text::ALLOW_REMOTE_CONTROL)
            .changed()
        {
            intents.push(Intent::SetInputAllowed(allowed));
        }
        if sharing.input_blocked {
            ui.label(
                RichText::new(text::INPUT_BLOCKED)
                    .size(DETAIL_SIZE)
                    .color(ui.visuals().warn_fg_color),
            );
        }
        if let Some(notice) = &sharing.notice {
            ui.label(RichText::new(notice).size(DETAIL_SIZE).weak());
        }
        pairing_view::authorized_devices(ui, &sharing.authorized, Timestamp::now(), intents);
    });
}

fn network_section(ui: &mut Ui, state: &GuiState, drafts: &mut Drafts, intents: &mut Vec<Intent>) {
    let rows = state.host_rows();
    if drafts.seen_pin_prompts != state.pin_prompts {
        // The host asked for the PIN again: an empty field with the keyboard.
        drafts.seen_pin_prompts = state.pin_prompts;
        drafts.host_pin.clear();
        drafts.focus_host_pin = true;
    }
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(text::COMPUTERS_ON_NETWORK)
                .size(16.0)
                .strong(),
        );
        if rows.is_empty() && state.discovery_error.is_none() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.spinner());
        }
    });
    ui.add_space(4.0);
    if let Some(error) = &state.discovery_error {
        ui.colored_label(error_color(ui), error);
    } else if rows.is_empty() {
        ui.weak(text::SEARCHING);
    }
    for row in &rows {
        let selected = state.selected_host.as_deref() == Some(row.id.as_str());
        if host_row(ui, row, selected).clicked() {
            drafts.host_pin.clear();
            drafts.focus_host_pin = !selected;
            let id = (!selected).then(|| row.id.clone());
            intents.push(Intent::SelectHost(id));
        }
        if selected {
            selected_host_form(ui, row, state, drafts, intents);
        }
    }
    if let Some(warning) = state.remembered.warning() {
        ui.label(
            RichText::new(text::remembered_unavailable(warning))
                .size(DETAIL_SIZE)
                .weak(),
        );
    }
    ui.add_space(GAP / 2.0);
    egui::CollapsingHeader::new(text::CONNECT_TO_ADDRESS).show_unindented(ui, |ui| {
        let error = state
            .connect_error
            .as_deref()
            .filter(|_| drafts.last_form == Form::Address);
        if pin_form(
            ui,
            &mut drafts.address_pin,
            Some(&mut drafts.address),
            error,
            false,
        ) {
            drafts.last_form = Form::Address;
            let pin = std::mem::take(&mut drafts.address_pin);
            intents.push(Intent::ConnectAddress {
                address: drafts.address.clone(),
                pin,
            });
        }
    });
}

/// Under the selected computer: `Connetti` (and `Dimentica`) if it
/// remembers this PC, else the PIN form.
fn selected_host_form(
    ui: &mut Ui,
    row: &HostRow,
    state: &GuiState,
    drafts: &mut Drafts,
    intents: &mut Vec<Intent>,
) {
    let error = state
        .connect_error
        .as_deref()
        .filter(|_| drafts.last_form == Form::Host);
    let connect = |pin| Intent::ConnectHost {
        id: row.id.clone(),
        pin,
    };
    if row.remembered {
        match pairing_view::remembered_form(ui, error) {
            Some(RememberedAction::Connect) => {
                drafts.last_form = Form::Host;
                intents.push(connect(String::new()));
            }
            Some(RememberedAction::Forget) => {
                drafts.last_form = Form::Host;
                intents.push(Intent::ForgetHost { id: row.id.clone() });
            }
            None => {}
        }
        return;
    }
    let focus = std::mem::take(&mut drafts.focus_host_pin);
    if pin_form(ui, &mut drafts.host_pin, None, error, focus) {
        drafts.last_form = Form::Host;
        intents.push(connect(std::mem::take(&mut drafts.host_pin)));
    }
    if pairing_view::ask_access(ui) {
        drafts.last_form = Form::Host;
        intents.push(Intent::RequestAccess { id: row.id.clone() });
    }
}

/// One computer of the list; clickable when it can be connected to.
fn host_row(ui: &mut Ui, row: &HostRow, selected: bool) -> Response {
    let enabled = row.connectable();
    // Reserve the background so it is painted below the text.
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = Frame::NONE
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let name = RichText::new(&row.name).strong();
            ui.label(if enabled { name } else { name.weak() });
            ui.label(RichText::new(row.details()).size(DETAIL_SIZE).weak());
        });
    let rect = inner.response.rect;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, ui.make_persistent_id(&row.id), sense);
    let visuals = ui.visuals();
    let fill = if selected {
        visuals.selection.bg_fill.gamma_multiply(0.35)
    } else if enabled && response.hovered() {
        visuals.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    let shape = egui::epaint::RectShape::filled(rect, CornerRadius::same(6), fill);
    ui.painter().set(background, shape);
    response.on_hover_cursor(if enabled {
        egui::CursorIcon::PointingHand
    } else {
        egui::CursorIcon::NotAllowed
    })
}

/// `[address] [PIN] [Connetti]`; returns true when the user asked to connect.
/// With an address field the PIN is optional (the host may remember this
/// PC); without one, the PIN is what the form is for.
fn pin_form(
    ui: &mut Ui,
    pin: &mut String,
    address: Option<&mut String>,
    error: Option<&str>,
    focus_pin: bool,
) -> bool {
    let mut submit = false;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let has_address = address.is_some();
        let mut address_given = false;
        if let Some(address) = address {
            let field = ui.add(
                TextEdit::singleline(address)
                    .hint_text("192.168.1.20")
                    .desired_width(130.0),
            );
            submit |= enter_pressed(ui, &field);
            address_given = !address.trim().is_empty();
        }
        let field = ui.add(
            TextEdit::singleline(pin)
                .hint_text("PIN")
                .desired_width(if has_address { 70.0 } else { 110.0 })
                .char_limit(16),
        );
        if focus_pin {
            field.request_focus();
        }
        submit |= enter_pressed(ui, &field);
        let ready = if has_address {
            address_given
        } else {
            !pin.trim().is_empty()
        };
        submit |= ui
            .add_enabled(ready, egui::Button::new(text::CONNECT))
            .clicked();
        submit &= ready;
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
    submit
}

fn enter_pressed(ui: &Ui, field: &Response) -> bool {
    field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

fn viewers_section(ui: &mut Ui, viewers: &[ViewerSession], intents: &mut Vec<Intent>) {
    if viewers.is_empty() {
        return;
    }
    ui.add_space(GAP / 2.0);
    ui.separator();
    for viewer in viewers {
        match &viewer.status {
            ViewerStatus::Connecting | ViewerStatus::AwaitingApproval | ViewerStatus::Connected => {
                let connecting = viewer.is_connecting();
                ui.horizontal(|ui| {
                    if connecting {
                        ui.spinner();
                    } else {
                        status_dot(ui, ok_color(ui));
                    }
                    ui.label(viewer.status_line());
                    if connecting {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button(text::CANCEL).clicked() {
                                intents.push(Intent::CancelViewer(viewer.id));
                            }
                        });
                    }
                });
            }
            ViewerStatus::Failed(_) => {
                ui.horizontal_top(|ui| {
                    let text_width = ui.available_width() - 28.0;
                    ui.allocate_ui(vec2(text_width, 0.0), |ui| {
                        let text = RichText::new(viewer.status_line()).color(error_color(ui));
                        ui.add(egui::Label::new(text).wrap());
                    });
                    if ui.small_button("×").on_hover_text(text::CLOSE).clicked() {
                        intents.push(Intent::DismissViewer(viewer.id));
                    }
                });
            }
        }
    }
}

pub(super) fn error_color(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(255, 120, 110)
    } else {
        Color32::from_rgb(190, 40, 30)
    }
}

pub(super) fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::group(ui.style())
        .inner_margin(Margin::same(12))
        .corner_radius(CornerRadius::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

fn ok_color(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(80, 200, 120)
    } else {
        Color32::from_rgb(30, 150, 70)
    }
}

fn status_dot(ui: &mut Ui, color: Color32) {
    let size = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(vec2(size * 0.6, size), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), size * 0.22, color);
}

/// An on/off switch.
fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let height = ui.spacing().interact_size.y * 1.1;
    let (rect, mut response) = ui.allocate_exact_size(vec2(height * 1.9, height), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::selected(Role::Switch, ui.is_enabled(), *on, label));
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let radius = 0.5 * rect.height();
        let fill = if *on {
            ui.visuals().selection.bg_fill
        } else {
            visuals.bg_fill
        };
        ui.painter()
            .rect(rect, radius, fill, visuals.bg_stroke, StrokeKind::Inside);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let knob = if *on {
            Color32::WHITE
        } else {
            visuals.fg_stroke.color
        };
        ui.painter()
            .circle_filled(pos2(x, rect.center().y), 0.72 * radius, knob);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
