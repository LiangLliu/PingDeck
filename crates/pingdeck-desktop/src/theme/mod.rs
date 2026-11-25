//! UI-framework-agnostic OKLCH design token compiler reading `tokens/tokens.json`.
//!
//! Resolves all 8 eye-care palettes (`slate` + 7 named palettes) in light/dark and optional
//! high-contrast modes into concrete [`RgbaColor`] / [`HslaColor`] values with Björn Ottosson's
//! OKLCH→sRGB gamut mapping (exact parity with `xtask check-tokens`).

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const TOKENS_JSON: &str = include_str!("../../../../tokens/tokens.json");

const GAMUT_STEPS: usize = 60;
const GAMUT_SHRINK: f64 = 0.95;
const GAMUT_EPSILON: f64 = 1e-4;

/// The 8 PingDeck eye-care colour palettes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaletteId {
    #[default]
    Slate,
    Ocean,
    Nord,
    Sage,
    Emerald,
    Amber,
    Violet,
    Graphite,
}

impl PaletteId {
    pub const ALL: [Self; 8] = [
        Self::Slate,
        Self::Ocean,
        Self::Nord,
        Self::Sage,
        Self::Emerald,
        Self::Amber,
        Self::Violet,
        Self::Graphite,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Slate => "slate",
            Self::Ocean => "ocean",
            Self::Nord => "nord",
            Self::Sage => "sage",
            Self::Emerald => "emerald",
            Self::Amber => "amber",
            Self::Violet => "violet",
            Self::Graphite => "graphite",
        }
    }

    #[must_use]
    pub const fn label(self, lang: crate::core::model::settings::Language) -> &'static str {
        let s = crate::i18n::tr(lang).settings();
        match self {
            Self::Slate => s.palette_slate_name(),
            Self::Ocean => s.palette_ocean_name(),
            Self::Nord => s.palette_nord_name(),
            Self::Sage => s.palette_sage_name(),
            Self::Emerald => s.palette_emerald_name(),
            Self::Amber => s.palette_amber_name(),
            Self::Violet => s.palette_violet_name(),
            Self::Graphite => s.palette_graphite_name(),
        }
    }
}

/// Concrete sRGB + alpha colour (independent of any rendering toolkit).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgbaColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f32,
}

impl RgbaColor {
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 1.0 }
    }

    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Packs RGB channels as `0x00RRGGBB`.
    #[must_use]
    pub const fn to_hex_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// Converts to normalized `(h, s, l, a)` where each channel is in `0.0..=1.0`
    /// (matches GPUI `hsla(h, s, l, a)` directly).
    #[must_use]
    pub fn to_hsla(self) -> (f32, f32, f32, f32) {
        let r = f32::from(self.r) / 255.0;
        let g = f32::from(self.g) / 255.0;
        let b = f32::from(self.b) / 255.0;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) * 0.5;
        let d = max - min;
        if d <= 1e-6 {
            return (0.0, 0.0, l, self.a);
        }
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let mut h = if (max - r).abs() <= 1e-6 {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if (max - g).abs() <= 1e-6 {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        h /= 6.0;
        (
            h.clamp(0.0, 1.0),
            s.clamp(0.0, 1.0),
            l.clamp(0.0, 1.0),
            self.a.clamp(0.0, 1.0),
        )
    }

    /// Composites translucent `self` over opaque `bg`.
    #[must_use]
    pub fn composite_over(self, bg: Self) -> Self {
        let a = self.a.clamp(0.0, 1.0);
        let mix = |f: u8, b: u8| -> u8 {
            (f32::from(f) * a + f32::from(b) * (1.0 - a))
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Self::rgb(mix(self.r, bg.r), mix(self.g, bg.g), mix(self.b, bg.b))
    }
}

/// All resolved semantic tokens for one `(PaletteId, dark, high_contrast)` configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPalette {
    pub palette: PaletteId,
    pub is_dark: bool,
    pub high_contrast: bool,

    // Surfaces
    pub canvas: RgbaColor,
    pub pane: RgbaColor,
    pub surface: RgbaColor,
    pub surface_2: RgbaColor,
    pub sunken: RgbaColor,
    pub overlay: RgbaColor,

    // Typography
    pub ink: RgbaColor,
    pub ink_2: RgbaColor,
    pub muted: RgbaColor,
    pub subtle: RgbaColor,
    pub faint: RgbaColor,
    pub placeholder: RgbaColor,

    // Borders & interaction states
    pub line: RgbaColor,
    pub line_strong: RgbaColor,
    pub line_control: RgbaColor,
    pub hover: RgbaColor,
    pub press: RgbaColor,
    pub selected: RgbaColor,
    pub selected_line: RgbaColor,
    pub selected_row: RgbaColor,
    pub page_edge: RgbaColor,
    pub scrim: RgbaColor,

    // Brand accent & focus
    pub accent: RgbaColor,
    pub accent_hover: RgbaColor,
    pub accent_mark: RgbaColor,
    pub accent_ink: RgbaColor,
    pub accent_soft: RgbaColor,
    pub accent_line: RgbaColor,
    pub on_accent: RgbaColor,
    pub on_accent_line: RgbaColor,
    pub focus: RgbaColor,

    // Status semantics
    pub ok: RgbaColor,
    pub ok_soft: RgbaColor,
    pub ok_line: RgbaColor,
    pub warn: RgbaColor,
    pub warn_soft: RgbaColor,
    pub warn_line: RgbaColor,
    pub bad: RgbaColor,
    pub bad_fill: RgbaColor,
    pub bad_soft: RgbaColor,
    pub bad_line: RgbaColor,
    pub live: RgbaColor,

    // HTTP methods
    pub m_get: RgbaColor,
    pub m_post: RgbaColor,
    pub m_put: RgbaColor,
    pub m_patch: RgbaColor,
    pub m_delete: RgbaColor,

    // Timing phases
    pub ph_dns: RgbaColor,
    pub ph_connect: RgbaColor,
    pub ph_tls: RgbaColor,
    pub ph_wait: RgbaColor,
    pub ph_recv: RgbaColor,

    // Diff & syntax highlighting
    pub diff_add: RgbaColor,
    pub diff_add_bg: RgbaColor,
    pub diff_del: RgbaColor,
    pub diff_del_bg: RgbaColor,
    pub tk_key: RgbaColor,
    pub tk_str: RgbaColor,
    pub tk_num: RgbaColor,
    pub tk_kw: RgbaColor,
    pub tk_com: RgbaColor,
    pub tk_fn: RgbaColor,
    pub bubble_user: RgbaColor,
}

type ColorMap = HashMap<String, RgbaColor>;

struct ParsedTokens {
    light: ColorMap,
    dark: ColorMap,
    contrast_light: ColorMap,
    contrast_dark: ColorMap,
    palettes: HashMap<PaletteId, (ColorMap, ColorMap)>,
}

static PARSED: OnceLock<ParsedTokens> = OnceLock::new();

fn parsed() -> &'static ParsedTokens {
    PARSED.get_or_init(parse_embedded_tokens)
}

/// Resolves a concrete semantic colour palette from embedded `tokens/tokens.json`.
#[must_use]
pub fn resolve_palette(palette: PaletteId, is_dark: bool, high_contrast: bool) -> ResolvedPalette {
    let db = parsed();
    let mut map = if is_dark {
        db.dark.clone()
    } else {
        db.light.clone()
    };
    if palette != PaletteId::Slate
        && let Some((pl, pd)) = db.palettes.get(&palette)
    {
        let overrides = if is_dark { pd } else { pl };
        for (k, &v) in overrides {
            map.insert(k.clone(), v);
        }
    }
    if high_contrast {
        let contrast = if is_dark {
            &db.contrast_dark
        } else {
            &db.contrast_light
        };
        for (k, &v) in contrast {
            map.insert(k.clone(), v);
        }
    }

    let fallback = if is_dark {
        RgbaColor::rgb(28, 32, 38)
    } else {
        RgbaColor::rgb(245, 247, 250)
    };
    let get = |key: &str| -> RgbaColor { map.get(key).copied().unwrap_or(fallback) };

    ResolvedPalette {
        palette,
        is_dark,
        high_contrast,
        canvas: get("canvas"),
        pane: get("pane"),
        surface: get("surface"),
        surface_2: get("surface-2"),
        sunken: get("sunken"),
        overlay: get("overlay"),
        ink: get("ink"),
        ink_2: get("ink-2"),
        muted: get("muted"),
        subtle: get("subtle"),
        faint: get("faint"),
        placeholder: get("placeholder"),
        line: get("line"),
        line_strong: get("line-strong"),
        line_control: get("line-control"),
        hover: get("hover"),
        press: get("press"),
        selected: get("selected"),
        selected_line: get("selected-line"),
        selected_row: get("selected-row"),
        page_edge: get("page-edge"),
        scrim: get("scrim"),
        accent: get("accent"),
        accent_hover: get("accent-hover"),
        accent_mark: get("accent-mark"),
        accent_ink: get("accent-ink"),
        accent_soft: get("accent-soft"),
        accent_line: get("accent-line"),
        on_accent: get("on-accent"),
        on_accent_line: map
            .get("on-accent-line")
            .copied()
            .unwrap_or(RgbaColor::rgba(255, 255, 255, 0.4)),
        focus: get("focus"),
        ok: get("ok"),
        ok_soft: get("ok-soft"),
        ok_line: get("ok-line"),
        warn: get("warn"),
        warn_soft: get("warn-soft"),
        warn_line: get("warn-line"),
        bad: get("bad"),
        bad_fill: get("bad-fill"),
        bad_soft: get("bad-soft"),
        bad_line: get("bad-line"),
        live: get("live"),
        m_get: get("m-get"),
        m_post: get("m-post"),
        m_put: get("m-put"),
        m_patch: get("m-patch"),
        m_delete: get("m-delete"),
        ph_dns: get("ph-dns"),
        ph_connect: get("ph-connect"),
        ph_tls: get("ph-tls"),
        ph_wait: get("ph-wait"),
        ph_recv: get("ph-recv"),
        diff_add: get("diff-add"),
        diff_add_bg: get("diff-add-bg"),
        diff_del: get("diff-del"),
        diff_del_bg: get("diff-del-bg"),
        tk_key: get("tk-key"),
        tk_str: get("tk-str"),
        tk_num: get("tk-num"),
        tk_kw: get("tk-kw"),
        tk_com: get("tk-com"),
        tk_fn: get("tk-fn"),
        bubble_user: get("bubble-user"),
    }
}

fn parse_embedded_tokens() -> ParsedTokens {
    let root: Value = serde_json::from_str(TOKENS_JSON).unwrap_or(Value::Null);
    let light = root
        .get("light")
        .and_then(Value::as_object)
        .map(|o| resolve_object(o, None))
        .unwrap_or_default();
    let dark = root
        .get("dark")
        .and_then(Value::as_object)
        .map(|o| resolve_object(o, None))
        .unwrap_or_default();
    let contrast_light = root
        .pointer("/contrast/light")
        .and_then(Value::as_object)
        .map(|o| resolve_object(o, Some(&light)))
        .unwrap_or_default();
    let contrast_dark = root
        .pointer("/contrast/dark")
        .and_then(Value::as_object)
        .map(|o| resolve_object(o, Some(&dark)))
        .unwrap_or_default();

    let mut palettes = HashMap::new();
    if let Some(p_obj) = root.get("palettes").and_then(Value::as_object) {
        for pid in PaletteId::ALL {
            if pid == PaletteId::Slate {
                continue;
            }
            if let Some(entry) = p_obj.get(pid.id()).and_then(Value::as_object) {
                let pl = entry
                    .get("light")
                    .and_then(Value::as_object)
                    .map(|o| resolve_object(o, Some(&light)))
                    .unwrap_or_default();
                let pd = entry
                    .get("dark")
                    .and_then(Value::as_object)
                    .map(|o| resolve_object(o, Some(&dark)))
                    .unwrap_or_default();
                palettes.insert(pid, (pl, pd));
            }
        }
    }

    ParsedTokens {
        light,
        dark,
        contrast_light,
        contrast_dark,
        palettes,
    }
}

fn resolve_object(
    obj: &serde_json::Map<String, Value>,
    scope: Option<&HashMap<String, RgbaColor>>,
) -> HashMap<String, RgbaColor> {
    let mut out = HashMap::new();
    for (k, v) in obj {
        if let Some(rgb) = parse_solid(v) {
            out.insert(k.clone(), rgb);
        }
    }
    for (k, v) in obj {
        if out.contains_key(k) {
            continue;
        }
        let Some(map) = v.as_object() else {
            continue;
        };
        let Some(alpha) = map.get("a").and_then(Value::as_f64) else {
            continue;
        };
        let Some(of) = map.get("of") else {
            continue;
        };
        let base = parse_solid(of)
            .or_else(|| of.as_str().and_then(|name| out.get(name).copied()))
            .or_else(|| {
                of.as_str()
                    .and_then(|name| scope.and_then(|s| s.get(name).copied()))
            });
        if let Some(base) = base {
            out.insert(
                k.clone(),
                RgbaColor::rgba(base.r, base.g, base.b, alpha as f32),
            );
        }
    }
    out
}

fn parse_solid(v: &Value) -> Option<RgbaColor> {
    if let Some(arr) = v.as_array()
        && arr.len() == 3
    {
        let l = arr[0].as_f64()?;
        let c = arr[1].as_f64()?;
        let h = arr[2].as_f64()?;
        let [r, g, b] = oklch_to_rgb(l, c, h);
        return Some(RgbaColor::rgb(r, g, b));
    }
    let hex = v.as_str()?.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(RgbaColor::rgb(r, g, b))
}

fn to_gam(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn oklch_to_linear(l: f64, c: f64, h: f64) -> [f64; 3] {
    let rad = h.to_radians();
    let a = c * rad.cos();
    let b = c * rad.sin();
    let l_ = (l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m_ = (l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s_ = (l - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
    [
        4.076_741_662_1 * l_ - 3.307_711_591_3 * m_ + 0.230_969_929_2 * s_,
        -1.268_438_004_6 * l_ + 2.609_757_401_1 * m_ - 0.341_319_396_5 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_614_7 * m_ + 1.707_614_701 * s_,
    ]
}

fn oklch_to_rgb(l: f64, c: f64, h: f64) -> [u8; 3] {
    let mut cur_c = c;
    for _ in 0..GAMUT_STEPS {
        let lin = oklch_to_linear(l, cur_c, h);
        if lin
            .iter()
            .all(|&v| (-GAMUT_EPSILON..=1.0 + GAMUT_EPSILON).contains(&v))
        {
            return to_u8_rgb(lin);
        }
        cur_c *= GAMUT_SHRINK;
    }
    to_u8_rgb(oklch_to_linear(l, 0.0, h))
}

fn to_u8_rgb(lin: [f64; 3]) -> [u8; 3] {
    let conv = |v: f64| -> u8 { (to_gam(v).clamp(0.0, 1.0) * 255.0).round() as u8 };
    [conv(lin[0]), conv(lin[1]), conv(lin[2])]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_all_16_palettes_with_distinct_accents_and_valid_hsla() {
        let slate_l = resolve_palette(PaletteId::Slate, false, false);
        let slate_d = resolve_palette(PaletteId::Slate, true, false);
        assert!(slate_l.surface.r > 230 && slate_d.surface.r < 60);

        let ocean_l = resolve_palette(PaletteId::Ocean, false, false);
        assert_ne!(slate_l.accent, ocean_l.accent);

        for pid in PaletteId::ALL {
            for dark in [false, true] {
                for hc in [false, true] {
                    let p = resolve_palette(pid, dark, hc);
                    let (h, s, l, a) = p.accent.to_hsla();
                    assert!((0.0..=1.0).contains(&h));
                    assert!((0.0..=1.0).contains(&s));
                    assert!((0.0..=1.0).contains(&l));
                    assert!((a - 1.0).abs() < 1e-4);
                }
            }
        }
    }
}
