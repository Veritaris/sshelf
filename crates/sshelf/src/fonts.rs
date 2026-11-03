//! The bundled fonts (SPEC.md §10 *Visual design*): Inter for UI text,
//! JetBrains Mono for values, paths and logs, and Lucide for icons in its own
//! family. egui's default fonts stay behind them as fallbacks. Versions,
//! sources and checksums are in `assets/fonts/README.md`.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

use crate::icons;

const INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
const JETBRAINS_MONO: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
const LUCIDE: &[u8] = include_bytes!("../assets/fonts/lucide.ttf");

/// The 600 weight of Inter (egui picks fonts by family, not by weight).
const SEMIBOLD_FAMILY: &str = "inter-semibold";

/// Installs the fonts on `ctx`; call once, before the first frame.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(definitions());
}

fn definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    for (name, data) in [
        ("Inter", INTER),
        ("Inter-SemiBold", INTER_SEMIBOLD),
        ("JetBrainsMono", JETBRAINS_MONO),
        ("lucide", LUCIDE),
    ] {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(data)));
    }
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "Inter".to_owned());
    let fallbacks = proportional.clone();
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "JetBrainsMono".to_owned());
    fonts.families.insert(
        FontFamily::Name(SEMIBOLD_FAMILY.into()),
        std::iter::once("Inter-SemiBold".to_owned())
            .chain(fallbacks)
            .collect(),
    );
    fonts.families.insert(
        FontFamily::Name(icons::FAMILY.into()),
        vec!["lucide".to_owned()],
    );
    fonts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_fonts_lead_their_families() {
        let fonts = definitions();
        let first = |family: FontFamily| fonts.families[&family][0].clone();
        assert_eq!(first(FontFamily::Proportional), "Inter");
        assert_eq!(first(FontFamily::Monospace), "JetBrainsMono");
        assert_eq!(
            first(FontFamily::Name(SEMIBOLD_FAMILY.into())),
            "Inter-SemiBold"
        );
        assert_eq!(
            fonts.families[&FontFamily::Name(icons::FAMILY.into())],
            ["lucide"]
        );
        // egui's own fonts are still there for anything Inter lacks.
        assert!(fonts.families[&FontFamily::Proportional].len() > 1);
    }

    /// Every icon sshelf names exists in the bundled Lucide font (the test
    /// harness panics on a missing glyph).
    #[test]
    fn every_icon_has_a_glyph() {
        let mut harness = crate::test_util::harness().build_ui(|ui| {
            for &icon in icons::ALL {
                ui.label(egui::RichText::new(icon.to_string()).font(icons::font(14.0)));
            }
        });
        harness.run();
    }
}
