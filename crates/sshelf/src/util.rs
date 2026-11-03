//! Small display helpers shared by the views.

use std::time::{Duration, SystemTime};

use sshconf::{ConfigSet, LineRef};
use sshelf_core::health::{Health, HealthSnapshot};

use crate::theme::Palette;

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

/// A painted health mark (SPEC.md §12, M8): a yellow ring while checking,
/// then green (auth OK), yellow (reachable, but key or host key trouble) or
/// red (unreachable). Nothing before the first check; hover tells more.
pub fn health_mark(ui: &mut egui::Ui, health: Option<&HealthSnapshot>, alias: &str) {
    let p = &Palette::ONE_DARK;
    let checking = health.is_some_and(|h| h.checking.contains(alias));
    let report = health.and_then(|h| h.reports.get(alias));
    let (rect, response) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    let center = rect.center();
    let text = if checking {
        ui.painter()
            .circle_stroke(center, 4.0, egui::Stroke::new(1.5, p.yellow));
        "Checking…".to_string()
    } else if let Some(report) = report {
        let color = match report.health {
            Health::Ok { .. } => p.green,
            Health::AuthFailed | Health::HostKeyUnknown | Health::HostKeyChanged => p.yellow,
            Health::Unreachable(_) => p.red,
        };
        ui.painter().circle_filled(center, 4.0, color);
        format!("{} ({})", report.health.describe(), ago(report.checked_at))
    } else {
        return;
    };
    response.on_hover_text(text);
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
