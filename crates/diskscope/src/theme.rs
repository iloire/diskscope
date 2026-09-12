//! Visual identity.
//!
//! The design leans on what the algorithm already looks like. A cushion
//! treemap is a hillshade — the same lit-relief rendering a topographic map
//! uses for terrain — so the app is dressed as a survey instrument reading a
//! landscape rather than as a dashboard.
//!
//! * **Ground** is slate blue, not black. The chrome stays a distinctly cool,
//!   mid-dark blue so the saturated treemap is the only vivid thing on screen,
//!   and only the map's own well drops to the deepest value — the unprinted
//!   paper of a chart, showing through as the gutters between directories.
//! * **Accent** is chart magenta, the colour aeronautical charts reserve for
//!   controlled airspace. It is also the one hue the file-kind palette barely
//!   uses, so a magenta outline stays readable on top of any block.
//! * **Type** is three voices with three jobs: DIN Condensed (the face of
//!   gauges and technical drawings) for structure and readouts, a monospace
//!   for every number so columns align like an instrument panel, and a quiet
//!   humanist sans for file names, which should not compete.

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

pub mod color {
    use egui::Color32;

    /// Chrome ground.
    pub const INK: Color32 = Color32::from_rgb(0x1b, 0x25, 0x30);
    /// Raised surfaces: toolbar, sidebar rows, status bar.
    pub const PLATE: Color32 = Color32::from_rgb(0x22, 0x2e, 0x3b);
    /// The map's well. Deepest value, used nowhere else, so the treemap's
    /// padding gutters read as the chart paper behind it.
    pub const CHART: Color32 = Color32::from_rgb(0x0d, 0x13, 0x18);
    /// Hairlines and dividers.
    pub const RULE: Color32 = Color32::from_rgb(0x34, 0x43, 0x4f);
    /// Secondary text, units, inactive labels.
    pub const SOUNDING: Color32 = Color32::from_rgb(0x8c, 0xa3, 0xb5);
    /// Primary text.
    pub const PAPER: Color32 = Color32::from_rgb(0xdb, 0xe5, 0xec);
    /// The single accent: selection, active crumb, highlighted kind.
    pub const MAGENTA: Color32 = Color32::from_rgb(0xe0, 0x45, 0x9a);
    /// Reserved for one meaning only — "this could not be read".
    pub const AMBER: Color32 = Color32::from_rgb(0xe0, 0xa0, 0x3c);
}

/// Family name for the structural face, registered in [`install`].
const STRUCTURE: &str = "structure";
/// Family name for the tabular data face.
const DATA: &str = "data";

/// macOS ships DIN Condensed as a plain static TTF, which is the whole reason
/// it is usable here. If it is missing the app falls back to the bundled
/// proportional face and simply speaks with less of an accent.
const STRUCTURE_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Supplemental/DIN Condensed Bold.ttf",
    "/System/Library/Fonts/Supplemental/DIN Alternate Bold.ttf",
];

/// Structural face at `size`: panel headers, buttons, readouts.
pub fn structure(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(STRUCTURE.into()))
}

/// Tabular face at `size`. Every number in the app uses this so that columns
/// of sizes line up digit for digit.
pub fn data(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(DATA.into()))
}

/// Name face at `size`: file and folder names, paths.
pub fn name(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Letter-spaces a short label by inserting thin spaces.
///
/// egui has no tracking control, and DIN Condensed set as tight uppercase is
/// hard to read at label sizes. Only for headers of a few words — it would
/// mangle anything longer.
pub fn spaced(label: &str) -> String {
    let mut out = String::with_capacity(label.len() * 2);
    for (i, c) in label.chars().enumerate() {
        if i > 0 {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    out
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);

    // Forced dark: the map's own colours are the palette, and a light chrome
    // would fight them.
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(apply);
}

fn apply(style: &mut egui::Style) {
    style.text_styles = [
        (TextStyle::Heading, structure(15.0)),
        (TextStyle::Body, name(13.0)),
        (TextStyle::Button, structure(14.0)),
        (TextStyle::Monospace, data(12.0)),
        (TextStyle::Small, name(11.0)),
    ]
    .into();

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = color::INK;
    v.window_fill = color::PLATE;
    v.extreme_bg_color = color::CHART;
    v.faint_bg_color = color::PLATE;
    v.window_stroke = Stroke::new(1.0, color::RULE);
    v.override_text_color = Some(color::PAPER);
    v.hyperlink_color = color::MAGENTA;
    v.selection.bg_fill = color::MAGENTA.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, color::MAGENTA);

    // 2 px corners: crisp enough to read as machined, short of the zero-radius
    // broadsheet look and well short of anything pill-shaped.
    let radius = CornerRadius::same(2);
    for widget in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.corner_radius = radius;
    }
    v.widgets.noninteractive.bg_fill = color::PLATE;
    v.widgets.noninteractive.weak_bg_fill = color::PLATE;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, color::RULE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, color::SOUNDING);

    v.widgets.inactive.bg_fill = color::PLATE;
    v.widgets.inactive.weak_bg_fill = color::PLATE;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, color::RULE);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, color::PAPER);

    v.widgets.hovered.bg_fill = Color32::from_rgb(0x2b, 0x3a, 0x49);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2b, 0x3a, 0x49);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, color::SOUNDING);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, color::PAPER);

    v.widgets.active.bg_fill = color::MAGENTA.gamma_multiply(0.30);
    v.widgets.active.weak_bg_fill = color::MAGENTA.gamma_multiply(0.30);
    v.widgets.active.bg_stroke = Stroke::new(1.0, color::MAGENTA);
    v.widgets.active.fg_stroke = Stroke::new(1.0, color::PAPER);

    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(8.0, 5.0);
    s.button_padding = egui::vec2(9.0, 3.0);
    s.window_margin = egui::Margin::same(12);
    s.interact_size.y = 22.0;
    s.scroll.bar_width = 8.0;
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // The bundled proportional and monospace faces are the fallbacks for every
    // family, so a missing system font degrades to legible rather than blank.
    let fallback_proportional: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let monospace: Vec<String> = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    let mut structure_stack = Vec::new();
    for path in STRUCTURE_CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                STRUCTURE.to_owned(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            structure_stack.push(STRUCTURE.to_owned());
            break;
        }
    }
    structure_stack.extend(fallback_proportional);
    fonts
        .families
        .insert(FontFamily::Name(STRUCTURE.into()), structure_stack);
    fonts
        .families
        .insert(FontFamily::Name(DATA.into()), monospace);

    ctx.set_fonts(fonts);
}
