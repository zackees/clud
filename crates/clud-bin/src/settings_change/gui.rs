//! Native approval: no stdin, environment, hook metadata, or reusable token.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use eframe::egui;

use super::{Failure, Preview, APPROVAL_LIFETIME};

pub(super) fn approve(preview: &Preview) -> Result<(), Failure> {
    if !crate::platform::secret_prompt::graphics_available() {
        return Err(Failure::Unavailable);
    }
    let outcome = Rc::new(RefCell::new(false));
    let returned = Rc::clone(&outcome);
    let preview = preview.clone();
    let mut first_frame = true;
    let mut native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([680.0, 520.0]),
        ..Default::default()
    };
    crate::platform::secret_prompt::configure(&mut native);
    eframe::run_ui_native("clud settings approval", native, move |context, _frame| {
        if preview.created.elapsed() > APPROVAL_LIFETIME {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        egui::CentralPanel::default().show(context, |ui| {
            if let Some(accepted) = draw(ui, &preview, &mut first_frame) {
                *returned.borrow_mut() = accepted;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        context.request_repaint_after(Duration::from_secs(1));
    })
    .map_err(|_| Failure::Unavailable)?;
    let accepted = *outcome.borrow();
    if accepted {
        Ok(())
    } else {
        Err(Failure::Cancelled)
    }
}

fn draw(ui: &mut egui::Ui, preview: &Preview, first_frame: &mut bool) -> Option<bool> {
    ui.heading(if preview.roots {
        "Approve deletion roots change"
    } else {
        "Approve settings change"
    });
    ui.label(format!("File: {}", preview.target.display()));
    ui.label(format!("Reason: {}", preview.reason));
    if preview.roots {
        ui.colored_label(
            egui::Color32::YELLOW,
            "This changes where safe-rm can delete. Review every path and reason.",
        );
    }
    let decision = ui
        .horizontal(|ui| {
            let cancel = ui.button("Cancel");
            if *first_frame {
                cancel.request_focus();
                *first_frame = false;
            }
            let accept = ui.button(if preview.roots {
                "Approve deletion roots"
            } else {
                "Approve change"
            });
            let cancelled =
                cancel.clicked() || ui.input(|input| input.key_pressed(egui::Key::Escape));
            if cancelled || accept.clicked() {
                Some(accept.clicked() && !cancelled)
            } else {
                None
            }
        })
        .inner;
    ui.separator();
    egui::ScrollArea::vertical().show(ui, |ui| draw_changes(ui, preview));
    decision
}

fn draw_changes(ui: &mut egui::Ui, preview: &Preview) {
    for change in &preview.changes {
        ui.strong(&change.pointer);
        ui.label("Before");
        ui.monospace(change.before.as_ref().map_or_else(
            || "<missing>".into(),
            |value| serde_json::to_string_pretty(value).unwrap_or_default(),
        ));
        ui.label("After");
        ui.monospace(change.after.as_ref().map_or_else(
            || "<missing>".into(),
            |value| serde_json::to_string_pretty(value).unwrap_or_default(),
        ));
        ui.separator();
    }
}
