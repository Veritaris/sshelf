//! Small display helpers shared by the views.

use std::time::{Duration, SystemTime};

use sshconf::{ConfigSet, LineRef};
use sshelf_core::health::{Health, HealthSnapshot};

use crate::theme::Palette;
use crate::widgets::{marks, popup};

/// `"3 h ago"`-style age of `time`.
pub fn ago(time: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(time)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    match secs {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

/// `file:line` with only the file name, and the full `path:line`.
pub fn location(set: &ConfigSet, at: LineRef) -> (String, String) {
    let file = &set.files[at.file];
    let name = file
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.path.display().to_string());
    let line = set.line(at).number;
    (format!("{name}:{line}"), set.location(at))
}

/// A painted health mark of `diameter` (SPEC.md §10, M7/M8 design): a
/// pulsing yellow ring while checking, then green (login OK), yellow
/// (reachable, but key or host key trouble) or red (unreachable); Comment
/// Grey before the first check. Hover tells more.
pub fn health_mark(
    ui: &mut egui::Ui,
    health: Option<&HealthSnapshot>,
    alias: &str,
    diameter: f32,
) -> egui::Response {
    let p = &Palette::ONE_DARK;
    let checking = health.is_some_and(|h| h.checking.contains(alias));
    let report = health.and_then(|h| h.reports.get(alias));
    if checking {
        let response = marks::pulsing_ring(ui, diameter, p.yellow);
        return popup::tip_below(response, |ui| {
            ui.label("Checking…");
        });
    }
    let (color, text, detail) = match report {
        Some(report) => (
            match report.health {
                Health::Ok { .. } => p.green,
                Health::AuthFailed | Health::HostKeyUnknown | Health::HostKeyChanged => p.yellow,
                Health::Unreachable(_) => p.red,
            },
            report.health.describe(),
            format!("checked {}", ago(report.checked_at)),
        ),
        None => (
            p.comment,
            "Not checked yet".to_string(),
            "Check logs in without a password".to_string(),
        ),
    };
    let (rect, response) =
        ui.allocate_exact_size(egui::Vec2::splat(diameter), egui::Sense::hover());
    marks::paint_dot(ui.painter(), rect.center(), diameter, color);
    popup::tip_below(response, |ui| {
        ui.label(text);
        ui.label(egui::RichText::new(detail).color(p.dim));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        let now = SystemTime::now();
        assert_eq!(ago(now), "just now");
        assert_eq!(ago(now - Duration::from_secs(5 * 60)), "5 min ago");
        assert_eq!(ago(now - Duration::from_secs(3 * 3600 + 5)), "3 h ago");
        assert_eq!(ago(now - Duration::from_secs(2 * 86400)), "2 days ago");
        assert_eq!(ago(now + Duration::from_secs(60)), "just now");
    }
}
