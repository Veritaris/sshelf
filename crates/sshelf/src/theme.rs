//! One Dark theme: the palette and its mapping onto egui's [`egui::Visuals`].
//!
//! Colors follow Atom's One Dark (https://github.com/atom/one-dark-syntax)
//! with the values of SPEC.md §10 *Visual design*.

use egui::{Color32, Stroke};

/// One Dark, as SPEC.md §10 *Visual design* lists it.
pub struct Palette {
    /// Background of the window and panels.
    pub bg: Color32,
    /// Rail, inputs, popups, code.
    pub bg_dark: Color32,
    /// Raised: inactive widget fill, cards, stripes.
    pub surface: Color32,
    /// Selected; also hovered widget fill and separators.
    pub selection: Color32,
    /// Outline; also pressed widget fill.
    pub border: Color32,
    /// Magenta: selection, primary actions, links.
    pub accent: Color32,
    /// Primary buttons, hovered and pressed.
    pub accent_hover: Color32,
    pub accent_pressed: Color32,
    pub text: Color32,
    /// Secondary text: other aliases, file names, hints.
    pub dim: Color32,
    /// Text on hovered and selected widgets.
    pub text_strong: Color32,
    pub green: Color32,
    pub yellow: Color32,
    pub red: Color32,
    /// Comment Grey: marks only (a stopped tunnel's dot), never body text.
    pub comment: Color32,
    /// Tooltip background.
    pub tooltip: Color32,
    /// Light Yellow mixed into the background: warning banners, chips, hovers.
    pub yellow_tint: Color32,
    /// Light Red mixed into the background: error banners, the error status bar.
    pub red_tint: Color32,
    /// Green mixed into the background: success banners, "running" chips.
    pub green_tint: Color32,
    /// Disabled menu items.
    pub disabled_text: Color32,
    /// Info, links, keyboard focus.
    pub blue: Color32,
    /// A hovered link.
    pub link_hover: Color32,
    /// Every other row of a table.
    pub zebra: Color32,
    /// Separators inside cards; a hovered copyable row.
    pub separator: Color32,
    /// Dark Red: destructive buttons (white text), and hovered.
    pub danger: Color32,
    pub danger_hover: Color32,
    /// Modal overlay.
    pub overlay: Color32,
}

/// Host colours (*Notes & tags*), named for their swatches' tooltips; none
/// of them is a status colour (SPEC.md §10 *Visual design*).
pub const SWATCHES: [(&str, Color32); 5] = [
    ("Blue", Color32::from_rgb(97, 175, 239)),     // #61afef
    ("Cyan", Color32::from_rgb(86, 182, 194)),     // #56b6c2
    ("Magenta", Color32::from_rgb(198, 120, 221)), // #c678dd
    ("Dark Yellow", Color32::from_rgb(209, 154, 102)), // #d19a66
    ("Comment Grey", Color32::from_rgb(92, 99, 112)), // #5c6370
];

impl Palette {
    pub const ONE_DARK: Self = Self {
        bg: Color32::from_rgb(40, 44, 52),               // #282c34
        bg_dark: Color32::from_rgb(33, 37, 43),          // #21252b
        surface: Color32::from_rgb(44, 49, 58),          // #2c313a
        selection: Color32::from_rgb(62, 68, 81),        // #3e4451
        border: Color32::from_rgb(75, 82, 99),           // #4b5263
        accent: Color32::from_rgb(198, 120, 221),        // #c678dd
        accent_hover: Color32::from_rgb(208, 142, 228),  // #d08ee4
        accent_pressed: Color32::from_rgb(177, 95, 200), // #b15fc8
        text: Color32::from_rgb(171, 178, 191),          // #abb2bf
        dim: Color32::from_rgb(154, 161, 173),           // #9aa1ad
        text_strong: Color32::from_rgb(215, 218, 224),   // #d7dae0
        green: Color32::from_rgb(152, 195, 121),         // #98c379
        yellow: Color32::from_rgb(229, 192, 123),        // #e5c07b
        red: Color32::from_rgb(224, 108, 117),           // #e06c75
        comment: Color32::from_rgb(92, 99, 112),         // #5c6370
        tooltip: Color32::from_rgb(24, 26, 31),          // #181a1f
        yellow_tint: Color32::from_rgb(59, 53, 39),      // #3b3527
        red_tint: Color32::from_rgb(58, 42, 46),         // #3a2a2e
        green_tint: Color32::from_rgb(43, 53, 40),       // #2b3528
        disabled_text: Color32::from_rgb(127, 132, 142), // #7f848e
        blue: Color32::from_rgb(97, 175, 239),           // #61afef
        link_hover: Color32::from_rgb(140, 196, 245),    // #8cc4f5
        zebra: Color32::from_rgb(42, 46, 54),            // #2a2e36
        separator: Color32::from_rgb(53, 59, 69),        // #353b45
        danger: Color32::from_rgb(190, 80, 70),          // #be5046
        danger_hover: Color32::from_rgb(204, 96, 86),    // #cc6056
        overlay: Color32::from_rgba_unmultiplied_const(24, 26, 31, 184), // rgba(24,26,31,.72)
    };
}

/// Corner radii (SPEC.md §10: 1, 4, 8 and 16 px).
pub mod radius {
    /// Table rows; the inner side of joined pieces.
    pub const ROW: u8 = 1;
    /// Controls, cards, menus, chips.
    pub const CONTROL: u8 = 8;
    /// The window, dialogs, the tray flyout, large panels.
    pub const LARGE: u8 = 16;
}

/// Motion timings in seconds (SPEC.md §10 *Interaction and motion*).
pub mod timing {
    /// Hover background fade of buttons, rows, cards and the like.
    pub const HOVER: f32 = 0.080;
    /// Sliding selection highlight and its text colour cross-fade.
    pub const SLIDE: f32 = 0.180;
    /// Hover time before a tooltip appears.
    pub const TOOLTIP_DELAY: f32 = 0.400;
    /// Tooltip fade (with a 3 px slide), in and out.
    pub const TOOLTIP_FADE: f32 = 0.120;
    /// The next tooltip within this long after one closes shows at once.
    pub const TOOLTIP_GRACE: f32 = 0.300;
    /// Toast fade (with a 6 px rise) in, and fade out.
    pub const TOAST_FADE: f32 = 0.120;
    /// How long a toast stays before it fades out (paused while hovered).
    pub const TOAST_HOLD: f32 = 3.0;
    /// Context menu fade (and 98 → 100 % scale) in and out; also the fade of
    /// a menu's hover highlight when the pointer leaves it.
    pub const MENU: f32 = 0.120;
    /// The hover highlight sliding between menu rows.
    pub const MENU_SLIDE: f32 = 0.050;
    /// One full pulse (100 → 40 → 100 %) of a starting or waiting ring.
    pub const PULSE: f32 = 1.0;
    /// The favourite star's colour fade.
    pub const STAR: f32 = 0.120;
}

/// Forces the dark theme and installs the One Dark visuals on `ctx`.
pub fn apply(ctx: &egui::Context) {
    let p = &Palette::ONE_DARK;
    let mut vis = egui::Visuals::dark();

    // The window paints its own (rounded) background; panels sit on it.
    vis.panel_fill = Color32::TRANSPARENT;
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
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.selection); // separators
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);

    w.inactive.bg_fill = p.surface;
    w.inactive.weak_bg_fill = p.surface;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);

    for state in [&mut w.hovered, &mut w.open] {
        state.bg_fill = p.selection;
        state.weak_bg_fill = p.selection;
        state.bg_stroke = Stroke::new(1.0, p.border);
        state.fg_stroke = Stroke::new(1.5, p.text_strong);
    }

    w.active.bg_fill = p.border;
    w.active.weak_bg_fill = p.border;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(2.0, p.accent);

    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, vis);
}
