//! Text that may not fit (SPEC.md §10 *Truncated text*): one line, cut with
//! an ellipsis, with the full text in a tooltip only when it was cut.

use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, FontId, Painter, Response, Sense, Ui};

use crate::widgets::popup;

/// `text` cut with an ellipsis to fit `width` in `font`, and whether it was
/// cut. Paths lose their middle, so the file name stays; anything else its
/// end.
pub fn elide(painter: &Painter, text: &str, font: &FontId, width: f32) -> (String, bool) {
    let fits = |s: &str| {
        painter
            .layout_no_wrap(s.to_string(), font.clone(), Color32::PLACEHOLDER)
            .size()
            .x
            <= width
    };
    if fits(text) {
        return (text.to_string(), false);
    }
    let chars: Vec<char> = text.chars().collect();
    let path = text.contains(['/', '\\']);
    let candidate = |keep: usize| -> String {
        if path {
            let tail = keep / 2 + keep % 2;
            let head = keep - tail;
            let mut s: String = chars[..head].iter().collect();
            s.push('…');
            s.extend(&chars[chars.len() - tail..]);
            s
        } else {
            let mut s: String = chars[..keep].iter().collect();
            s.push('…');
            s
        }
    };
    // The most characters that still fit.
    let (mut low, mut high) = (0, chars.len() - 1);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if fits(&candidate(mid)) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    (candidate(low), true)
}

/// `job` on one line in the width left in `ui`; `full` is the tooltip text
/// when it doesn't fit.
pub fn truncated(ui: &mut Ui, mut job: LayoutJob, full: &str) -> Response {
    job.wrap = TextWrapping::truncate_at_width(ui.available_width().max(0.0));
    let galley = ui.painter().layout_job(job);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    let elided = galley.elided;
    ui.painter().galley(rect.min, galley, Color32::PLACEHOLDER);
    if elided {
        popup::tip_below(response, |ui| {
            ui.label(full);
        })
    } else {
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util;

    #[test]
    fn text_loses_its_end_and_paths_their_middle() {
        let mut cut = Vec::new();
        let mut harness = test_util::harness().build_ui(|ui| {
            let font = FontId::monospace(13.0);
            let painter = ui.painter();
            let width = painter
                .layout_no_wrap("x".repeat(20), font.clone(), Color32::WHITE)
                .size()
                .x;
            cut = vec![
                elide(painter, "nohostauthenticationforlocalhost", &font, width),
                elide(painter, r"C:\users\me\.ssh\id_ed25519_dev", &font, width),
                elide(painter, "short", &font, width),
            ];
        });
        harness.run();
        drop(harness);
        assert_eq!(cut[0], ("nohostauthenticatio…".to_string(), true));
        assert_eq!(cut[1], (r"C:\users\…d25519_dev".to_string(), true));
        assert_eq!(cut[2], ("short".to_string(), false));
    }
}
