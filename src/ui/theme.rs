//! The design language, shared with Sentinelle: one dark surface family,
//! text tokens, and one hue per *measure*. Here blue is size (Sentinelle's
//! load colour, which its storage bars also use). Status colours are
//! reserved for state and always come with a label.

use eframe::egui::{
    self, text::LayoutJob, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    TextFormat, Visuals,
};
use std::sync::Arc;

pub const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

// Surfaces
pub const BG: Color32 = hex(0x121211);
pub const SURFACE: Color32 = hex(0x1a1a19);
pub const SURFACE_HOVER: Color32 = hex(0x222220);
pub const RAISED: Color32 = hex(0x262624);
pub const BORDER: Color32 = hex(0x2a2a28);
pub const GRID: Color32 = hex(0x2c2c2a);
pub const TRACK: Color32 = hex(0x252523);

// Text tokens
pub const TEXT: Color32 = hex(0xf0efec);
pub const TEXT_2: Color32 = hex(0xc3c2b7);
pub const MUTED: Color32 = hex(0x8b8a83);
pub const FAINT: Color32 = hex(0x5e5d58);

// Measures (dark-mode steps of the validated categorical palette)
pub const LOAD: Color32 = hex(0x3987e5);
pub const HEAT: Color32 = hex(0xd95926);
pub const MEMORY: Color32 = hex(0x199e70);
pub const DOWN: Color32 = hex(0x9085e9);
pub const UP: Color32 = hex(0xd55181);
/// Neutral series (latency): identity comes from its label, not a hue.
pub const NEUTRAL: Color32 = hex(0xc3c2b7);

/// Size bars.
pub const SIZE: Color32 = LOAD;

/// Storage segments: one blue ramp, largest first and brightest.
pub const RAMP: [Color32; 8] = [
    hex(0x86b6ef),
    hex(0x6da7ec),
    hex(0x5598e7),
    hex(0x3987e5),
    hex(0x2a78d6),
    hex(0x256abf),
    hex(0x1c5cab),
    hex(0x184f95),
];
pub const OTHER: Color32 = hex(0x4a4a46);

// Status (reserved; always paired with a label)
pub const GOOD: Color32 = hex(0x0ca30c);
pub const WARNING: Color32 = hex(0xfab219);
pub const SERIOUS: Color32 = hex(0xec835a);
pub const CRITICAL: Color32 = hex(0xd03b3b);

pub const RADIUS: u8 = 10;
pub const GAP: f32 = 12.0;

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("strong".into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx, true);
    install_style(ctx);
}

/// The theme with egui's bundled fonts only, so tests and snapshots don't
/// depend on what's installed.
pub fn install_for_tests(ctx: &egui::Context) {
    install_fonts(ctx, false);
    install_style(ctx);
}

fn install_style(ctx: &egui::Context) {

    let mut v = Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = RAISED;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(8);
    v.extreme_bg_color = hex(0x141413);
    v.faint_bg_color = SURFACE_HOVER;
    v.override_text_color = Some(TEXT_2);
    v.hyperlink_color = LOAD;
    v.selection.bg_fill = LOAD.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, LOAD);
    v.popup_shadow = egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(120) };
    v.window_shadow = v.popup_shadow;
    for (w, fill) in [
        (&mut v.widgets.noninteractive, SURFACE),
        (&mut v.widgets.inactive, RAISED),
        (&mut v.widgets.hovered, hex(0x30302d)),
        (&mut v.widgets.active, hex(0x383835)),
        (&mut v.widgets.open, hex(0x30302d)),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = CornerRadius::same(6);
        w.bg_stroke = Stroke::new(1.0, BORDER);
        w.fg_stroke.color = TEXT_2;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.hovered.fg_stroke.color = TEXT;
    v.widgets.active.fg_stroke.color = TEXT;
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        use egui::TextStyle::*;
        s.text_styles = [
            (Small, font(11.0)),
            (Body, font(13.0)),
            (Button, font(13.0)),
            (Heading, strong(16.0)),
            (Monospace, mono(12.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.interact_size.y = 24.0;
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        s.interaction.tooltip_delay = 0.25;
    });
}

/// Use Inter (or another good UI sans) when installed; egui's built-in
/// fonts stay as fallbacks for symbols.
fn install_fonts(ctx: &egui::Context, system: bool) {
    let mut defs = FontDefinitions::default();
    let regular = system.then(|| find_font("Inter:style=Regular", "Inter")).flatten()
        .or_else(|| find_font("Noto Sans:style=Regular", "Noto Sans"))
        .or_else(|| find_font("Cantarell:style=Regular", "Cantarell"));
    let semibold = system.then(|| find_font("Inter:style=SemiBold", "Inter")).flatten()
        .or_else(|| find_font("Inter:style=Medium", "Inter"))
        .or_else(|| find_font("Noto Sans:style=SemiBold", "Noto Sans"));

    if let Some(data) = regular {
        defs.font_data.insert("ui".into(), Arc::new(FontData::from_owned(data)));
        defs.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "ui".into());
    }
    let mut strong_family = defs.families[&FontFamily::Proportional].clone();
    if let Some(data) = semibold {
        defs.font_data.insert("ui-strong".into(), Arc::new(FontData::from_owned(data)));
        strong_family.insert(0, "ui-strong".into());
    }
    defs.families.insert(FontFamily::Name("strong".into()), strong_family);
    ctx.set_fonts(defs);
}

fn find_font(pattern: &str, family: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{family}\n%{style}\n%{file}", pattern])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let fam = lines.next()?;
    let style = lines.next().unwrap_or("");
    let file = lines.next()?;
    if !fam.split(',').any(|f| f.trim() == family) {
        return None;
    }
    // Variable fonts only expose their default instance; a variable file
    // matched for a bold style would just be regular.
    let want = pattern.split("style=").nth(1).unwrap_or("Regular");
    if !style.split(',').any(|s| s.trim().eq_ignore_ascii_case(want)) && want != "Regular" {
        return None;
    }
    std::fs::read(file).ok()
}

/// Small uppercase label with tracking, used for section and card titles.
pub fn eyebrow(text: &str, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        &text.to_uppercase(),
        0.0,
        TextFormat { font_id: strong(10.5), color, extra_letter_spacing: 1.2, ..Default::default() },
    );
    job
}

// ------------------------------------------------------------- formatting

pub fn bytes(b: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 || v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

pub fn duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}
