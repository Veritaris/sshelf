//! Left sidebar: view selection and the orphan indicator (SPEC.md §6, §10).

use egui::RichText;
use sshelf_core::orphans::Orphan;

use crate::theme::Palette;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Hosts,
    Tunnels,
    Groups,
    Settings,
}

/// Returns `true` when the orphan indicator was clicked.
pub fn show(ui: &mut egui::Ui, view: &mut View, orphans: &[Orphan]) -> bool {
    let p = &Palette::ONE_DARK;
    ui.add_space(8.0);
    ui.label(RichText::new("sshelf").color(p.accent).strong().size(18.0));
    ui.add_space(12.0);
    ui.selectable_value(view, View::Hosts, "Hosts");
    ui.selectable_value(view, View::Tunnels, "Tunnels");
    ui.selectable_value(view, View::Groups, "Groups");
    ui.selectable_value(view, View::Settings, "Settings");

    let mut clicked = false;
    if !orphans.is_empty() {
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(8.0);
            let label = RichText::new(format!("⚠ {} orphaned", orphans.len())).color(p.yellow);
            clicked = ui
                .button(label)
                .on_hover_ui(|ui| {
                    ui.label("App data that no longer links to a host:");
                    for orphan in orphans {
                        ui.label(format!("• {}", orphan.describe()));
                    }
                    ui.weak("Click to relink or delete.");
                })
                .clicked();
        });
    }
    clicked
}
