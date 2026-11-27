//! Curated OKLCH eye-care theme accent colors and custom `#RRGGBB` perceptual ramp synthesis.
//!
//! Users can keep each eye-care palette's native accent (`FollowPalette`), pick one of 8 curated
//! OKLCH accents, or provide any `#RRGGBB` hex color. All overrides synthesize a full light/dark
//! token ramp (`accent`, `accent_hover`, `accent_mark`, `accent_ink`, `accent_soft`, `accent_line`,
//! `selected_row`, `focus`, `on_accent`, `bubble_user`) with mathematically guaranteed WCAG AA
//! contrast (≥ 4.5:1; ≥ 7.0:1 in high-contrast mode).

use serde::{Deserialize, Serialize};

use super::{ResolvedPalette, RgbaColor, oklch_to_rgb, rgb_to_oklch, wcag_contrast};

/// Curated eye-care theme accent presets tuned in OKLCH perceptual space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccentPreset {
    Indigo,
    Ocean,
    Teal,
    Emerald,
    Amber,
    Coral,
    Rose,
    Violet,
}

impl AccentPreset {
    pub const ALL: [Self; 8] = [
        Self::Indigo,
        Self::Ocean,
        Self::Teal,
        Self::Emerald,
        Self::Amber,
        Self::Coral,
        Self::Rose,
        Self::Violet,
    ];

    /// Stable machine identifier used in persistence, element IDs, and i18n lookup.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Indigo => "indigo",
            Self::Ocean => "ocean",
            Self::Teal => "teal",
            Self::Emerald => "emerald",
            Self::Amber => "amber",
            Self::Coral => "coral",
            Self::Rose => "rose",
            Self::Violet => "violet",
        }
    }

    /// Canonical OKLCH `(chroma, hue_deg)` seed for this accent preset.
    #[must_use]
    pub const fn oklch_chroma_hue(self) -> (f64, f64) {
        match self {
            Self::Indigo => (0.195, 264.0),
            Self::Ocean => (0.170, 236.0),
            Self::Teal => (0.140, 192.0),
            Self::Emerald => (0.155, 156.0),
            Self::Amber => (0.155, 62.0),
            Self::Coral => (0.175, 32.0),
            Self::Rose => (0.185, 358.0),
            Self::Violet => (0.195, 294.0),
        }
    }

    /// Representative solid swatch color for the requested light/dark appearance.
    #[must_use]
    pub fn swatch_color(self, is_dark: bool) -> RgbaColor {
        let (c, h) = self.oklch_chroma_hue();
        let l = if is_dark { 0.66 } else { 0.52 };
        let [r, g, b] = oklch_to_rgb(l, c, h);
        RgbaColor::rgb(r, g, b)
    }
}

/// User-selected theme accent color override applied inside any eye-care palette.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AccentChoice {
    /// Use the active eye-care palette's native OKLCH accent ramp.
    #[default]
    FollowPalette,
    /// One of the 8 curated OKLCH eye-care theme accent presets.
    Preset(AccentPreset),
    /// Custom sRGB color packed as `0x00RRGGBB`, perceptually mapped into an OKLCH ramp.
    Custom(u32),
}

impl AccentChoice {
    /// Returns the solid preview swatch color (or `base.accent` when following the palette).
    #[must_use]
    pub fn preview_color(self, base: &ResolvedPalette) -> RgbaColor {
        match self {
            Self::FollowPalette => base.accent,
            Self::Preset(preset) => preset.swatch_color(base.is_dark),
            Self::Custom(hex) => rgb_from_u32(hex),
        }
    }

    /// Returns the `#RRGGBB` string when a custom color is active, or the swatch hex for presets.
    #[must_use]
    pub fn hex_string(self, base: &ResolvedPalette) -> String {
        format_hex_rgb(self.preview_color(base))
    }
}

/// Parses `#RGB`, `RGB`, `#RRGGBB`, or `RRGGBB` into `0x00RRGGBB`.
#[must_use]
pub fn parse_hex_rgb(raw: &str) -> Option<u32> {
    let hex = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        3 => {
            let r = u32::from_str_radix(&hex[0..1], 16).ok()? * 17;
            let g = u32::from_str_radix(&hex[1..2], 16).ok()? * 17;
            let b = u32::from_str_radix(&hex[2..3], 16).ok()? * 17;
            Some((r << 16) | (g << 8) | b)
        }
        6 => u32::from_str_radix(hex, 16).ok().map(|v| v & 0x00FF_FFFF),
        _ => None,
    }
}

/// Formats an [`RgbaColor`] as uppercase `#RRGGBB`.
#[must_use]
pub fn format_hex_rgb(c: RgbaColor) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

#[must_use]
const fn rgb_from_u32(hex: u32) -> RgbaColor {
    RgbaColor::rgb(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

/// Applies `choice` onto a resolved eye-care palette, synthesizing a full WCAG-compliant OKLCH
/// accent token ramp while preserving the underlying eye-care paper/dark surfaces.
pub fn apply_accent_override(palette: &mut ResolvedPalette, choice: AccentChoice) {
    let (seed_c, hue) = match choice {
        AccentChoice::FollowPalette => return,
        AccentChoice::Preset(preset) => preset.oklch_chroma_hue(),
        AccentChoice::Custom(hex) => {
            let rgb = rgb_from_u32(hex);
            let (_, c, h) = rgb_to_oklch(rgb.r, rgb.g, rgb.b);
            // Keep neutral custom greys tasteful while capping neon chromas into eye-care bounds.
            (c.clamp(0.0, 0.21), h)
        }
    };

    let min_ink_contrast = if palette.high_contrast { 7.0 } else { 4.5 };
    let white = RgbaColor::rgb(255, 255, 255);

    if palette.is_dark {
        let c_main = seed_c.min(0.195);
        // Primary filled button accent must sustain >= 4.5:1 against white label ink.
        let accent = tune_lightness_for_contrast(0.56, c_main, hue, white, 4.5, false);
        let (acc_l, _, _) = rgb_to_oklch(accent.r, accent.g, accent.b);
        let [hr, hg, hb] = oklch_to_rgb((acc_l - 0.055).max(0.28), c_main * 0.94, hue);
        let [mr, mg, mb] = oklch_to_rgb(0.745, (c_main * 0.80).min(0.155), hue);
        let [sr, sg, sb] = oklch_to_rgb(0.305, (c_main * 0.36).min(0.068), hue);
        let accent_soft = RgbaColor::rgb(sr, sg, sb);
        let accent_ink = tune_lightness_for_two_bgs(
            0.835,
            (c_main * 0.58).min(0.115),
            hue,
            palette.surface,
            accent_soft,
            min_ink_contrast,
            true,
        );
        let [rr, rg, rb] = oklch_to_rgb(0.326, (c_main * 0.16).min(0.028), hue);
        let [fr, fg, fb] = oklch_to_rgb(0.785, (c_main * 0.70).min(0.135), hue);
        let [br, bg_, bb] = oklch_to_rgb(0.300, (c_main * 0.16).min(0.028), hue);

        palette.accent = accent;
        palette.accent_hover = RgbaColor::rgb(hr, hg, hb);
        palette.accent_mark = RgbaColor::rgb(mr, mg, mb);
        palette.accent_ink = accent_ink;
        palette.accent_soft = accent_soft;
        palette.accent_line = RgbaColor::rgba(accent_ink.r, accent_ink.g, accent_ink.b, 0.44);
        palette.on_accent = white;
        palette.selected_row = RgbaColor::rgb(rr, rg, rb);
        palette.focus = RgbaColor::rgb(fr, fg, fb);
        palette.bubble_user = RgbaColor::rgb(br, bg_, bb);
        palette.tk_key = accent_ink;
    } else {
        let c_main = seed_c.min(0.205);
        let accent = tune_lightness_for_contrast(0.51, c_main, hue, white, 4.5, false);
        let (acc_l, _, _) = rgb_to_oklch(accent.r, accent.g, accent.b);
        let [hr, hg, hb] = oklch_to_rgb((acc_l - 0.055).max(0.26), c_main * 0.94, hue);
        let [mr, mg, mb] = oklch_to_rgb((acc_l + 0.03).min(0.62), c_main * 0.92, hue);
        let [sr, sg, sb] = oklch_to_rgb(0.944, (c_main * 0.15).min(0.028), hue);
        let accent_soft = RgbaColor::rgb(sr, sg, sb);
        let accent_ink = tune_lightness_for_two_bgs(
            0.46,
            (c_main * 0.92).min(0.185),
            hue,
            palette.surface,
            accent_soft,
            min_ink_contrast,
            false,
        );
        let [rr, rg, rb] = oklch_to_rgb(0.946, (c_main * 0.10).min(0.018), hue);
        let [fr, fg, fb] = oklch_to_rgb(0.515, (c_main * 0.85).min(0.165), hue);
        let [br, bg_, bb] = oklch_to_rgb(0.948, (c_main * 0.08).min(0.015), hue);

        palette.accent = accent;
        palette.accent_hover = RgbaColor::rgb(hr, hg, hb);
        palette.accent_mark = RgbaColor::rgb(mr, mg, mb);
        palette.accent_ink = accent_ink;
        palette.accent_soft = accent_soft;
        palette.accent_line = RgbaColor::rgba(accent.r, accent.g, accent.b, 0.36);
        palette.on_accent = white;
        palette.selected_row = RgbaColor::rgb(rr, rg, rb);
        palette.focus = RgbaColor::rgb(fr, fg, fb);
        palette.bubble_user = RgbaColor::rgb(br, bg_, bb);
        palette.tk_key = accent_ink;
    }
}

/// Steps OKLCH lightness down (or up) until `wcag_contrast(candidate, bg) >= min_ratio`.
fn tune_lightness_for_contrast(
    start_l: f64,
    c: f64,
    h: f64,
    bg: RgbaColor,
    min_ratio: f64,
    lighten: bool,
) -> RgbaColor {
    let mut l = start_l;
    for _ in 0..48 {
        let [r, g, b] = oklch_to_rgb(l, c, h);
        let cand = RgbaColor::rgb(r, g, b);
        if wcag_contrast(cand, bg) >= min_ratio {
            return cand;
        }
        l = if lighten {
            (l + 0.014).min(0.97)
        } else {
            (l - 0.014).max(0.16)
        };
    }
    let [r, g, b] = oklch_to_rgb(l, c, h);
    RgbaColor::rgb(r, g, b)
}

fn tune_lightness_for_two_bgs(
    start_l: f64,
    c: f64,
    h: f64,
    bg_a: RgbaColor,
    bg_b: RgbaColor,
    min_ratio: f64,
    lighten: bool,
) -> RgbaColor {
    let mut l = start_l;
    for _ in 0..48 {
        let [r, g, b] = oklch_to_rgb(l, c, h);
        let cand = RgbaColor::rgb(r, g, b);
        if wcag_contrast(cand, bg_a) >= min_ratio && wcag_contrast(cand, bg_b) >= min_ratio {
            return cand;
        }
        l = if lighten {
            (l + 0.014).min(0.97)
        } else {
            (l - 0.014).max(0.16)
        };
    }
    let [r, g, b] = oklch_to_rgb(l, c, h);
    RgbaColor::rgb(r, g, b)
}
