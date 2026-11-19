//! Design token verification for `tokens/tokens.json` (UI-framework-agnostic OKLCH palettes).
//!
//! Validates OKLCH gamut mapping, alpha references, palette completeness (slate base + 7 named
//! palettes in light/dark + contrast overrides), and WCAG 2.x contrast floors in pure Rust.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use serde_json::Value;

use crate::{err_line, out_line};

const GAMUT_STEPS: usize = 60;
const GAMUT_SHRINK: f64 = 0.95;
const GAMUT_EPSILON: f64 = 1e-4;

const REQUIRED_PALETTES: &[&str] = &[
    "ocean", "nord", "sage", "emerald", "amber", "violet", "graphite",
];

const SURFACES: &[&str] = &[
    "canvas",
    "sunken",
    "pane",
    "surface",
    "surface-2",
    "overlay",
];
const TEXT_TOKENS: &[&str] = &["ink", "ink-2", "muted", "accent-ink"];

#[derive(Debug, Clone)]
struct ResolvedToken {
    rgb: Option<[u8; 3]>,
    base: Option<[u8; 3]>,
    alpha: Option<f64>,
}

fn to_lin(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
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

fn to_u8_rgb(lin: [f64; 3]) -> [u8; 3] {
    let conv = |v: f64| -> u8 {
        let clamped = to_gam(v).clamp(0.0, 1.0);
        (clamped * 255.0).round() as u8
    };
    [conv(lin[0]), conv(lin[1]), conv(lin[2])]
}

fn in_gamut(lin: [f64; 3]) -> bool {
    lin.iter()
        .all(|&v| (-GAMUT_EPSILON..=1.0 + GAMUT_EPSILON).contains(&v))
}

fn oklch_to_rgb(l: f64, c: f64, h: f64) -> [u8; 3] {
    let mut cur_c = c;
    for _ in 0..GAMUT_STEPS {
        let lin = oklch_to_linear(l, cur_c, h);
        if in_gamut(lin) {
            return to_u8_rgb(lin);
        }
        cur_c *= GAMUT_SHRINK;
    }
    to_u8_rgb(oklch_to_linear(l, 0.0, h))
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r, g, b])
}

fn parse_solid(v: &Value) -> Option<[u8; 3]> {
    if let Some(arr) = v.as_array()
        && arr.len() == 3
    {
        let l = arr[0].as_f64()?;
        let c = arr[1].as_f64()?;
        let h = arr[2].as_f64()?;
        return Some(oklch_to_rgb(l, c, h));
    }
    v.as_str().and_then(parse_hex)
}

fn resolve_map(
    obj: &serde_json::Map<String, Value>,
    scope: Option<&BTreeMap<String, ResolvedToken>>,
) -> Result<BTreeMap<String, ResolvedToken>, String> {
    let mut out = BTreeMap::new();
    for (k, v) in obj {
        if let Some(rgb) = parse_solid(v) {
            out.insert(
                k.clone(),
                ResolvedToken {
                    rgb: Some(rgb),
                    base: None,
                    alpha: None,
                },
            );
        } else if v.is_string() {
            out.insert(
                k.clone(),
                ResolvedToken {
                    rgb: None,
                    base: None,
                    alpha: None,
                },
            );
        }
    }
    for (k, v) in obj {
        if out.contains_key(k) {
            continue;
        }
        let Some(map) = v.as_object() else {
            return Err(format!("token --{k}: unsupported value"));
        };
        let Some(alpha) = map.get("a").and_then(Value::as_f64) else {
            return Err(format!("token --{k}: missing numeric alpha `a`"));
        };
        let Some(of) = map.get("of") else {
            return Err(format!("token --{k}: missing `of` base"));
        };
        let base = parse_solid(of)
            .or_else(|| {
                of.as_str()
                    .and_then(|name| out.get(name).and_then(|t| t.rgb))
            })
            .or_else(|| {
                of.as_str()
                    .and_then(|name| scope.and_then(|s| s.get(name)).and_then(|t| t.rgb))
            });
        let Some(base) = base else {
            return Err(format!("token --{k}: unknown base {of}"));
        };
        out.insert(
            k.clone(),
            ResolvedToken {
                rgb: None,
                base: Some(base),
                alpha: Some(alpha),
            },
        );
    }
    Ok(out)
}

fn rel_lum(rgb: [u8; 3]) -> f64 {
    let r = to_lin(f64::from(rgb[0]) / 255.0);
    let g = to_lin(f64::from(rgb[1]) / 255.0);
    let b = to_lin(f64::from(rgb[2]) / 255.0);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn wcag(a: [u8; 3], b: [u8; 3]) -> f64 {
    let x = rel_lum(a);
    let y = rel_lum(b);
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn composite_over(fg: [u8; 3], alpha: f64, bg: [u8; 3]) -> [u8; 3] {
    let mix = |f: u8, b: u8| -> u8 {
        (f64::from(f) * alpha + f64::from(b) * (1.0 - alpha))
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [mix(fg[0], bg[0]), mix(fg[1], bg[1]), mix(fg[2], bg[2])]
}

fn rgb_on(map: &BTreeMap<String, ResolvedToken>, key: &str, bg: [u8; 3]) -> Option<[u8; 3]> {
    let tok = map.get(key)?;
    if let Some(rgb) = tok.rgb {
        Some(rgb)
    } else {
        Some(composite_over(tok.base?, tok.alpha?, bg))
    }
}

pub fn check_all(root: &Path) -> Result<usize, Vec<String>> {
    let path = root.join("tokens/tokens.json");
    let raw = fs::read_to_string(&path).map_err(|e| vec![format!("{}: {e}", path.display())])?;
    let json: Value =
        serde_json::from_str(&raw).map_err(|e| vec![format!("{}: {e}", path.display())])?;

    let mut problems = Vec::new();

    let Some(light_obj) = json.get("light").and_then(Value::as_object) else {
        return Err(vec!["tokens.json missing `light` object".into()]);
    };
    let Some(dark_obj) = json.get("dark").and_then(Value::as_object) else {
        return Err(vec!["tokens.json missing `dark` object".into()]);
    };
    let Some(palettes_obj) = json.get("palettes").and_then(Value::as_object) else {
        return Err(vec!["tokens.json missing `palettes` object".into()]);
    };
    let Some(contrast_obj) = json.get("contrast").and_then(Value::as_object) else {
        return Err(vec!["tokens.json missing `contrast` object".into()]);
    };

    let light = resolve_map(light_obj, None).map_err(|e| vec![format!("light: {e}")])?;
    let dark = resolve_map(dark_obj, None).map_err(|e| vec![format!("dark: {e}")])?;

    for (mode, base, c_key) in [("light", &light, "light"), ("dark", &dark, "dark")] {
        if let Some(c_obj) = contrast_obj.get(c_key).and_then(Value::as_object) {
            if let Err(e) = resolve_map(c_obj, Some(base)) {
                problems.push(format!("contrast.{mode}: {e}"));
            }
        } else {
            problems.push(format!("tokens.json missing contrast.{mode}"));
        }
    }

    let check_theme =
        |label: &str, theme: &BTreeMap<String, ResolvedToken>, probs: &mut Vec<String>| {
            for &surf in SURFACES {
                let Some(bg) = theme.get(surf).and_then(|t| t.rgb) else {
                    probs.push(format!("{label}: missing solid surface `{surf}`"));
                    continue;
                };
                for &fg_key in TEXT_TOKENS {
                    let Some(fg) = rgb_on(theme, fg_key, bg) else {
                        probs.push(format!("{label}: missing text token `{fg_key}`"));
                        continue;
                    };
                    let ratio = wcag(fg, bg);
                    if ratio < 4.5 {
                        probs.push(format!("{label}: {fg_key} on {surf} WCAG {ratio:.2} < 4.5"));
                    }
                }
            }
            if let (Some(on_acc), Some(acc)) = (
                theme.get("on-accent").and_then(|t| t.rgb),
                theme.get("accent").and_then(|t| t.rgb),
            ) {
                let ratio = wcag(on_acc, acc);
                if ratio < 4.5 {
                    probs.push(format!(
                        "{label}: on-accent on accent WCAG {ratio:.2} < 4.5"
                    ));
                }
            } else {
                probs.push(format!("{label}: missing on-accent or accent"));
            }
        };

    check_theme("slate/light", &light, &mut problems);
    check_theme("slate/dark", &dark, &mut problems);

    for &pal in REQUIRED_PALETTES {
        let Some(p_obj) = palettes_obj.get(pal).and_then(Value::as_object) else {
            problems.push(format!("tokens.json missing required palette `{pal}`"));
            continue;
        };
        for (mode, base) in [("light", &light), ("dark", &dark)] {
            let Some(m_obj) = p_obj.get(mode).and_then(Value::as_object) else {
                problems.push(format!("palette `{pal}` missing `{mode}`"));
                continue;
            };
            match resolve_map(m_obj, Some(base)) {
                Ok(overrides) => {
                    let mut merged = base.clone();
                    merged.extend(overrides);
                    check_theme(&format!("{pal}/{mode}"), &merged, &mut problems);
                }
                Err(e) => problems.push(format!("palette {pal}/{mode}: {e}")),
            }
        }
    }

    if problems.is_empty() {
        Ok((palettes_obj.len() + 1) * 2)
    } else {
        Err(problems)
    }
}

pub fn run(root: &Path, args: &[String]) -> ExitCode {
    if !args.is_empty() {
        err_line("usage: cargo xtask check-tokens");
        return ExitCode::from(2);
    }
    match check_all(root) {
        Ok(variants) => {
            out_line(&format!(
                "tokens ok: {variants} palette variants verified (OKLCH gamut + WCAG >= 4.5)"
            ));
            ExitCode::SUCCESS
        }
        Err(problems) => {
            for p in &problems {
                err_line(&format!("token error: {p}"));
            }
            err_line(&format!(
                "check-tokens failed with {} issue(s)",
                problems.len()
            ));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_root;

    #[test]
    fn tokens_json_passes_gamut_and_wcag_checks() {
        let variants = check_all(&repo_root()).expect("tokens.json must pass all checks");
        assert_eq!(variants, 16, "slate + 7 palettes in light and dark = 16");
    }
}
