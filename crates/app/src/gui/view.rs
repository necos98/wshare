//! Draws the window from the [`GuiState`] and turns clicks into
//! [`Intent`]s. Holds only the text being typed ([`Drafts`]).

use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, Response, RichText, Role, Sense,
    StrokeKind, TextEdit, Ui, WidgetInfo, pos2, vec2,
};

use super::controller::Intent;
use super::model::{GuiState, HostRow, Sharing, ViewerSession, ViewerStatus, format_pin};

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
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Form {
    #[default]
    Host,
    Address,
}

const GAP: f32 = 10.0;
/// Font size of secondary lines (details, warnings).
const DETAIL_SIZE: f32 = 12.0;

pub fn show(ui: &mut Ui, state: &GuiState, drafts: &mut Drafts) -> Vec<Intent> {
    let mut intents = Vec::new();
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        sharing_section(ui, &state.sharing, &mut intents);
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
        ui.label(RichText::new("Condividi questo schermo").size(16.0).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut on = sharing.enabled;
            if toggle(ui, &mut on, "Condividi questo schermo").changed() {
                intents.push(Intent::SetSharing(on));
            }
        });
    });
    if let Some(error) = &sharing.error {
        ui.add_space(4.0);
        ui.colored_label(error_color(ui), error);
    }
    if !sharing.enabled {
        ui.add_space(4.0);
        ui.weak("Attivalo per far vedere e controllare questo PC da un altro computer in rete.");
        return;
    }
    ui.add_space(GAP / 2.0);
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.weak("PIN");
                let pin = sharing.pin.as_deref().map_or_else(|| "– – –".to_string(), format_pin);
                ui.label(RichText::new(pin).monospace().size(34.0).strong());
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(sharing.listening, egui::Button::new("Nuovo PIN")).clicked() {
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
                    if connected { ok_color(ui) } else { ui.visuals().weak_text_color() },
                );
            } else {
                ui.spinner();
            }
            ui.label(sharing.status_line());
            if connected {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Disconnetti").clicked() {
                        intents.push(Intent::DisconnectViewer);
                    }
                });
            }
        });
        ui.add_space(4.0);
        let mut allowed = sharing.input_allowed;
        if ui.checkbox(&mut allowed, "Consenti controllo remoto (mouse e tastiera)").changed() {
            intents.push(Intent::SetInputAllowed(allowed));
        }
        if sharing.input_blocked {
            ui.label(
                RichText::new(
                    "Input bloccato da Windows (finestra amministratore o schermata protetta).",
                )
                .size(DETAIL_SIZE)
                .color(ui.visuals().warn_fg_color),
            );
        }
        if let Some(notice) = &sharing.notice {
            ui.label(RichText::new(notice).size(DETAIL_SIZE).weak());
        }
    });
}

fn network_section(ui: &mut Ui, state: &GuiState, drafts: &mut Drafts, intents: &mut Vec<Intent>) {
    let rows = state.host_rows();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Computer in rete").size(16.0).strong());
        if rows.is_empty() && state.discovery_error.is_none() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.spinner());
        }
    });
    ui.add_space(4.0);
    if let Some(error) = &state.discovery_error {
        ui.colored_label(error_color(ui), error);
    } else if rows.is_empty() {
        ui.weak("Ricerca in corso… Sull'altro computer apri wshare e attiva la condivisione.");
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
            let error = state.connect_error.as_deref().filter(|_| drafts.last_form == Form::Host);
            let focus = std::mem::take(&mut drafts.focus_host_pin);
            let connect = pin_form(ui, &mut drafts.host_pin, None, error, focus);
            if connect {
                drafts.last_form = Form::Host;
                let pin = std::mem::take(&mut drafts.host_pin);
                intents.push(Intent::ConnectHost { id: row.id.clone(), pin });
            }
        }
    }
    ui.add_space(GAP / 2.0);
    egui::CollapsingHeader::new("Connetti a un indirizzo IP").show_unindented(ui, |ui| {
        let error = state.connect_error.as_deref().filter(|_| drafts.last_form == Form::Address);
        if pin_form(ui, &mut drafts.address_pin, Some(&mut drafts.address), error, false) {
            drafts.last_form = Form::Address;
            let pin = std::mem::take(&mut drafts.address_pin);
            intents.push(Intent::ConnectAddress { address: drafts.address.clone(), pin });
        }
    });
}

/// One computer of the list; clickable when it can be connected to.
fn host_row(ui: &mut Ui, row: &HostRow, selected: bool) -> Response {
    let enabled = row.connectable();
    // Reserve the background so it is painted below the text.
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = Frame::NONE.inner_margin(Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let name = RichText::new(&row.name).strong();
        ui.label(if enabled { name } else { name.weak() });
        ui.label(RichText::new(row.details()).size(DETAIL_SIZE).weak());
    });
    let rect = inner.response.rect;
    let sense = if enabled { Sense::click() } else { Sense::hover() };
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
        if let Some(address) = address {
            let field = ui
                .add(TextEdit::singleline(address).hint_text("192.168.1.20").desired_width(130.0));
            submit |= enter_pressed(ui, &field);
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
        let ready = !pin.trim().is_empty();
        submit |= ui.add_enabled(ready, egui::Button::new("Connetti")).clicked();
        submit &= ready;
    });
    if let Some(error) = error {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new(error).size(DETAIL_SIZE).color(error_color(ui)));
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
            ViewerStatus::Connecting | ViewerStatus::Connected => {
                ui.horizontal(|ui| {
                    if viewer.status == ViewerStatus::Connecting {
                        ui.spinner();
                    } else {
                        status_dot(ui, ok_color(ui));
                    }
                    ui.label(viewer.status_line());
                });
            }
            ViewerStatus::Failed(_) => {
                ui.horizontal_top(|ui| {
                    let text_width = ui.available_width() - 28.0;
                    ui.allocate_ui(vec2(text_width, 0.0), |ui| {
                        let text = RichText::new(viewer.status_line()).color(error_color(ui));
                        ui.add(egui::Label::new(text).wrap());
                    });
                    if ui.small_button("×").on_hover_text("Chiudi").clicked() {
                        intents.push(Intent::DismissViewer(viewer.id));
                    }
                });
            }
        }
    }
}

fn error_color(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(255, 120, 110)
    } else {
        Color32::from_rgb(190, 40, 30)
    }
}

fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
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
    ui.painter().circle_filled(rect.center(), size * 0.22, color);
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
        let fill = if *on { ui.visuals().selection.bg_fill } else { visuals.bg_fill };
        ui.painter().rect(rect, radius, fill, visuals.bg_stroke, StrokeKind::Inside);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let knob = if *on { Color32::WHITE } else { visuals.fg_stroke.color };
        ui.painter().circle_filled(pos2(x, rect.center().y), 0.72 * radius, knob);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
