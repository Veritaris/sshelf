//! One Dark theme: the palette and its mapping onto egui's [`egui::Visuals`].
//!
//! Colors follow Atom's One Dark (https://github.com/atom/one-dark-syntax),
//! matching the claude-overlay look.

use egui::{Color32, Stroke};

/// Toned-down, low-saturation One Dark colors.
pub struct Palette {
    /// Panel fill.
    pub bg: Color32,
    /// Windows, text fields, code.
    pub bg_dark: Color32,
    /// Inactive widget fill, stripes.
    pub surface: Color32,
    /// Hovered widget fill.
    pub surface_hover: Color32,
    /// Selected text / pressed widget fill.
    pub selection: Color32,
    pub border: Color32,
    /// Header / brand / links.
    pub accent: Color32,
    pub text: Color32,
    /// Secondary text: other aliases, file names, hints.
    pub dim: Color32,
    /// Text on hovered widgets.
    pub text_strong: Color32,
    pub green: Color32,
    pub yellow: Color32,
    pub red: Color32,
}

impl Palette {
    pub const ONE_DARK: Self = Self {
        bg: Color32::from_rgb(40, 44, 52),             // #282c34
        bg_dark: Color32::from_rgb(33, 37, 43),        // #21252b
        surface: Color32::from_rgb(44, 49, 58),        // #2c313a
        surface_hover: Color32::from_rgb(58, 64, 76),  // #3a404c
        selection: Color32::from_rgb(62, 68, 81),      // #3e4451
        border: Color32::from_rgb(86, 92, 108),        // #565c6c
        accent: Color32::from_rgb(198, 120, 221),      // #c678dd
        text: Color32::from_rgb(171, 178, 191),        // #abb2bf
        dim: Color32::from_rgb(130, 137, 151),         // softer gray
        text_strong: Color32::from_rgb(215, 218, 224), // #d7dae0
        green: Color32::from_rgb(152, 195, 121),       // #98c379
        yellow: Color32::from_rgb(229, 192, 123),      // #e5c07b
        red: Color32::from_rgb(224, 108, 117),         // #e06c75
    };
}

/// Forces the dark theme and installs the One Dark visuals on `ctx`.
pub fn apply(ctx: &egui::Context) {
    let p = &Palette::ONE_DARK;
    let mut vis = egui::Visuals::dark();

    vis.panel_fill = p.bg;
    vis.window_fill = p.bg_dark;
    vis.window_stroke = Stroke::new(1.0, p.border);
    vis.faint_bg_color = p.surface;
    vis.extreme_bg_color = p.bg_dark;
    vis.code_bg_color = p.bg_dark;
    vis.hyperlink_color = p.accent;
    vis.warn_fg_color = p.yellow;
    vis.error_fg_color = p.red;
    vis.selection.bg_fill = p.selection;
    vis.selection.stroke = Stroke::new(1.0, p.accent);

    let w = &mut vis.widgets;
    w.noninteractive.bg_fill = p.bg;
    w.noninteractive.weak_bg_fill = p.bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.surface_hover); // separators
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);

    w.inactive.bg_fill = p.surface;
    w.inactive.weak_bg_fill = p.surface;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);

    for state in [&mut w.hovered, &mut w.open] {
        state.bg_fill = p.surface_hover;
        state.weak_bg_fill = p.surface_hover;
        state.bg_stroke = Stroke::new(1.0, p.border);
        state.fg_stroke = Stroke::new(1.5, p.text_strong);
    }

    w.active.bg_fill = p.selection;
    w.active.weak_bg_fill = p.selection;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(2.0, p.accent);

    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, vis);
}
