//! «Newspaper dark» look: warm near-black paper, off-white ink, hairline rules, one muted red accent.
//! Everything goes through egui's `Style` / `Visuals` / fonts; the helpers below only build `RichText`s and
//! draw rules (lines), never whole widgets.
//!
//! Fonts (all SIL OFL 1.1, latin subset from Fontsource 5.3.0, WOFF → TTF container change only; licences in
//! `assets/fonts/`): Playfair Display (masthead, headlines), Source Serif 4 (body), IBM Plex Sans Condensed
//! (labels, buttons), IBM Plex Mono (ids, versions, paths). egui's own fonts stay as fallbacks (symbols, other
//! scripts).

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText, Shadow, Stroke, TextStyle, Ui, Vec2};
use std::sync::Arc;

pub const PAPER: Color32 = Color32::from_rgb(0x12, 0x11, 0x0f);
pub const PAPER_2: Color32 = Color32::from_rgb(0x18, 0x16, 0x13);
pub const PAPER_3: Color32 = Color32::from_rgb(0x1f, 0x1c, 0x18);
pub const HOVER: Color32 = Color32::from_rgb(0x26, 0x22, 0x1c);
pub const PRESSED: Color32 = Color32::from_rgb(0x2e, 0x29, 0x20);
pub const INK: Color32 = Color32::from_rgb(0xe8, 0xe2, 0xd6);
pub const INK_2: Color32 = Color32::from_rgb(0xbd, 0xb4, 0xa0);
pub const INK_3: Color32 = Color32::from_rgb(0x8a, 0x81, 0x70);
pub const INK_4: Color32 = Color32::from_rgb(0x5d, 0x56, 0x4a);
pub const RULE: Color32 = Color32::from_rgb(0x33, 0x2e, 0x26);
pub const RULE_STRONG: Color32 = Color32::from_rgb(0x5b, 0x52, 0x44);
/// The one accent: masthead rule, the primary action, selection marks.
pub const RED: Color32 = Color32::from_rgb(0xc9, 0x46, 0x3b);
pub const GOLD: Color32 = Color32::from_rgb(0xd4, 0xa8, 0x55);
pub const GREEN: Color32 = Color32::from_rgb(0x8f, 0xb5, 0x7a);

const DISPLAY: &str = "display";
const HEADLINE: &str = "headline";
const SERIF_BOLD: &str = "serif-bold";
const SERIF_ITALIC: &str = "serif-italic";
const SANS: &str = "sans";
const SANS_BOLD: &str = "sans-bold";

pub fn display_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(DISPLAY.into()))
}
pub fn headline_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(HEADLINE.into()))
}
pub fn serif_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SERIF_BOLD.into()))
}
pub fn serif_italic(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SERIF_ITALIC.into()))
}
pub fn sans(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SANS.into()))
}
pub fn sans_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SANS_BOLD.into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

fn fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let fallback = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mono_fallback = defs.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let files: [(&str, &'static [u8]); 8] = [
        ("PlayfairDisplay-Black", include_bytes!("../assets/fonts/PlayfairDisplay-Black.ttf")),
        ("PlayfairDisplay-Bold", include_bytes!("../assets/fonts/PlayfairDisplay-Bold.ttf")),
        ("SourceSerif4-Regular", include_bytes!("../assets/fonts/SourceSerif4-Regular.ttf")),
        ("SourceSerif4-Italic", include_bytes!("../assets/fonts/SourceSerif4-Italic.ttf")),
        ("SourceSerif4-SemiBold", include_bytes!("../assets/fonts/SourceSerif4-SemiBold.ttf")),
        ("IBMPlexSansCondensed-Medium", include_bytes!("../assets/fonts/IBMPlexSansCondensed-Medium.ttf")),
        ("IBMPlexSansCondensed-SemiBold", include_bytes!("../assets/fonts/IBMPlexSansCondensed-SemiBold.ttf")),
        ("IBMPlexMono-Regular", include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")),
    ];
    for (name, bytes) in files {
        defs.font_data.insert(name.to_string(), Arc::new(FontData::from_static(bytes)));
    }
    let chain = |first: &str, rest: &[String]| -> Vec<String> { std::iter::once(first.to_string()).chain(rest.iter().cloned()).collect() };
    defs.families.insert(FontFamily::Proportional, chain("SourceSerif4-Regular", &fallback));
    defs.families.insert(FontFamily::Monospace, chain("IBMPlexMono-Regular", &mono_fallback));
    for (family, first) in [
        (DISPLAY, "PlayfairDisplay-Black"),
        (HEADLINE, "PlayfairDisplay-Bold"),
        (SERIF_BOLD, "SourceSerif4-SemiBold"),
        (SERIF_ITALIC, "SourceSerif4-Italic"),
        (SANS, "IBMPlexSansCondensed-Medium"),
        (SANS_BOLD, "IBMPlexSansCondensed-SemiBold"),
    ] {
        defs.families.insert(FontFamily::Name(family.into()), chain(first, &fallback));
    }
    defs
}

fn widget(fill: Color32, weak: Color32, stroke: Stroke, fg: Color32) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals { bg_fill: fill, weak_bg_fill: weak, bg_stroke: stroke, corner_radius: CornerRadius::ZERO, fg_stroke: Stroke::new(1.0, fg), expansion: 0.0 }
}

fn style(s: &mut egui::Style) {
    use egui::style::ScrollStyle;
    s.text_styles = [
        (TextStyle::Small, sans(11.5)),
        (TextStyle::Body, FontId::proportional(15.0)),
        (TextStyle::Button, sans_bold(13.0)),
        (TextStyle::Heading, headline_font(24.0)),
        (TextStyle::Monospace, mono(12.5)),
    ]
    .into();
    s.interaction.selectable_labels = false;
    let sp = &mut s.spacing;
    sp.item_spacing = Vec2::new(10.0, 7.0);
    sp.button_padding = Vec2::new(12.0, 5.0);
    sp.interact_size = Vec2::new(40.0, 26.0);
    sp.window_margin = Margin::same(22);
    sp.menu_margin = Margin::same(6);
    sp.icon_width = 15.0;
    sp.icon_width_inner = 9.0;
    sp.icon_spacing = 6.0;
    sp.combo_width = 180.0;
    sp.scroll = ScrollStyle { bar_width: 8.0, ..ScrollStyle::thin() };

    let v = &mut s.visuals;
    v.dark_mode = true;
    v.override_text_color = None;
    v.weak_text_color = Some(INK_3);
    v.hyperlink_color = GOLD;
    v.faint_bg_color = PAPER_2;
    v.extreme_bg_color = Color32::from_rgb(0x0c, 0x0b, 0x0a);
    v.text_edit_bg_color = Some(Color32::from_rgb(0x0e, 0x0d, 0x0b));
    v.code_bg_color = PAPER_3;
    v.warn_fg_color = GOLD;
    v.error_fg_color = RED;
    v.panel_fill = PAPER;
    v.window_fill = PAPER_2;
    v.window_stroke = Stroke::new(1.0, RULE_STRONG);
    v.window_corner_radius = CornerRadius::ZERO;
    v.menu_corner_radius = CornerRadius::ZERO;
    v.window_shadow = Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(170) };
    v.popup_shadow = Shadow { offset: [0, 6], blur: 14, spread: 0, color: Color32::from_black_alpha(140) };
    v.window_highlight_topmost = false;
    v.selection.bg_fill = Color32::from_rgb(0x5a, 0x24, 0x1f);
    v.selection.stroke = Stroke::new(1.0, INK);
    v.text_cursor.stroke = Stroke::new(1.5, INK);
    v.striped = false;
    v.button_frame = true;
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = true;
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.5 };
    let w = &mut v.widgets;
    // noninteractive: panels, separators (the hairline rules), labels
    w.noninteractive = widget(PAPER, PAPER, Stroke::new(1.0, RULE), INK_2);
    // buttons are outlined type: no fill until hovered
    w.inactive = widget(PAPER_3, PAPER, Stroke::new(1.0, RULE_STRONG), INK);
    w.hovered = widget(HOVER, HOVER, Stroke::new(1.0, INK_3), INK);
    w.active = widget(PRESSED, PRESSED, Stroke::new(1.0, INK), INK);
    w.open = widget(PAPER_3, PAPER_3, Stroke::new(1.0, INK_3), INK);
}

/// Fonts + dark newspaper style (for both egui themes, so a light Windows theme changes nothing).
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(style);
}

// ---------------------------------------------------------------- text helpers

/// Small uppercase, letter-spaced label (kickers, section labels, column heads).
pub fn kicker(text: &str, color: Color32) -> RichText {
    RichText::new(text.to_uppercase()).font(sans_bold(11.0)).extra_letter_spacing(1.3).color(color)
}

/// Button caption: uppercase condensed sans with a little letter spacing.
pub fn caps(text: &str) -> RichText {
    RichText::new(text.to_uppercase()).font(sans_bold(12.5)).extra_letter_spacing(0.9)
}

pub fn headline(text: &str, size: f32) -> RichText {
    RichText::new(text).font(headline_font(size)).color(INK)
}

pub fn italic(text: &str) -> RichText {
    RichText::new(text).font(serif_italic(14.5)).color(INK_3)
}

// ---------------------------------------------------------------- rules and buttons

/// A full-width horizontal rule at the current position.
pub fn rule(ui: &mut Ui, color: Color32, width: f32) {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, width.max(1.0)), egui::Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, Stroke::new(width, color));
}

/// The masthead double rule: a heavy line over a thin one.
pub fn double_rule(ui: &mut Ui, color: Color32) {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, 6.0), egui::Sense::hover());
    ui.painter().hline(r.x_range(), r.top() + 1.0, Stroke::new(2.0, color));
    ui.painter().hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, color));
}

/// The primary action (accent fill).
pub fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(caps(text).color(Color32::WHITE)).fill(RED).stroke(Stroke::new(1.0, RED)).min_size(Vec2::new(0.0, 32.0))
}

/// A normal outlined action.
pub fn button(text: &str) -> egui::Button<'static> {
    egui::Button::new(caps(text)).min_size(Vec2::new(0.0, 32.0))
}

/// A small outlined action (inside columns and dialogs).
pub fn small_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_uppercase()).font(sans_bold(11.5)).extra_letter_spacing(0.8))
}

/// A frameless text action (masthead navigation, links inside a column).
pub fn text_button(text: RichText) -> egui::Button<'static> {
    egui::Button::new(text).frame(false)
}
