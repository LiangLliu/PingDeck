//! Pure formatting & localization helpers for the "Models & Pricing" settings page.

use pingdeck_desktop::core::app::EffectivePriceView;
use pingdeck_desktop::core::model::model_catalog::{
    CatalogSyncErrorKind, DefaultBasis, ModelCatalogRow, PriceBasis, PriceBasisKind, PricingMode,
};

use crate::ui::Tone;

/// Maps supported ISO 4217 currency codes to their display symbol.
#[must_use]
pub fn currency_symbol(code: &str) -> &'static str {
    match code.trim().to_ascii_uppercase().as_str() {
        "CNY" | "JPY" => "¥",
        "EUR" => "€",
        "GBP" => "£",
        "HKD" => "HK$",
        _ => "$",
    }
}

/// Formats a non-negative price with clean precision (2 decimals for normal prices, up to 4 for
/// tiny fractions such as cache-read tokens).
#[must_use]
pub fn format_money(amount: f64, currency: &str) -> String {
    let sym = currency_symbol(currency);
    if !amount.is_finite() {
        return "—".into();
    }
    if amount == 0.0 {
        return format!("{sym}0.00");
    }
    if amount >= 0.01 {
        let two = format!("{amount:.2}");
        if (amount - two.parse::<f64>().unwrap_or(amount)).abs() < 1e-6 {
            return format!("{sym}{two}");
        }
    }
    let s = format!("{amount:.4}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    format!("{sym}{trimmed}")
}

/// Formats optional token limits compactly (`128K`, `1M`, `16.4K`, `—`).
#[must_use]
pub fn format_tokens(tokens: Option<u64>) -> String {
    let Some(n) = tokens else {
        return "—".into();
    };
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        if (m - m.round()).abs() < 0.05 {
            format!("{}M", m.round() as u64)
        } else {
            format!("{m:.1}M")
        }
    } else if n >= 1_000 {
        let k = n as f64 / 1_000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{}K", k.round() as u64)
        } else {
            format!("{k:.1}K")
        }
    } else {
        n.to_string()
    }
}

/// Formats a Unix millisecond timestamp as `YYYY-MM-DD` (UTC) without external date crates.
#[must_use]
pub fn format_ymd(ts_ms: i64) -> String {
    let days = ts_ms.div_euclid(86_400_000);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}

/// Formats a relative duration string (`刚刚` / `3 分钟前` / `2 小时前` / `5 天前`).
#[must_use]
pub fn format_ago(ts_ms: i64, now_ms: i64, zh: bool) -> String {
    let diff_s = ((now_ms - ts_ms).max(0)) / 1000;
    if diff_s < 60 {
        if zh {
            "刚刚".into()
        } else {
            "just now".into()
        }
    } else if diff_s < 3600 {
        let m = diff_s / 60;
        if zh {
            format!("{m} 分钟前")
        } else {
            format!("{m}m ago")
        }
    } else if diff_s < 86_400 {
        let h = diff_s / 3600;
        if zh {
            format!("{h} 小时前")
        } else {
            format!("{h}h ago")
        }
    } else {
        let d = diff_s / 86_400;
        if zh {
            format!("{d} 天前")
        } else {
            format!("{d}d ago")
        }
    }
}

/// Formats a future due timestamp for the automatic sync schedule.
#[must_use]
pub fn format_next_due(next_due_ms: i64, now_ms: i64, zh: bool) -> String {
    let rem_s = (next_due_ms - now_ms) / 1000;
    if rem_s <= 60 {
        if zh {
            "下次检查：即将进行".into()
        } else {
            "Next check: due soon".into()
        }
    } else if rem_s < 3600 {
        let m = rem_s / 60;
        if zh {
            format!("下次检查：{m} 分钟后")
        } else {
            format!("Next check: in {m}m")
        }
    } else if rem_s < 86_400 {
        let h = rem_s / 3600;
        if zh {
            format!("下次检查：{h} 小时后")
        } else {
            format!("Next check: in {h}h")
        }
    } else {
        let d = format_ymd(next_due_ms);
        if zh {
            format!("下次检查：{d}")
        } else {
            format!("Next check: {d}")
        }
    }
}

/// Localizes a catalog sync failure using `CatalogSyncErrorKind` and HTTP status.
#[must_use]
pub fn format_sync_error(
    kind: Option<CatalogSyncErrorKind>,
    status: Option<u16>,
    raw: Option<&str>,
    zh: bool,
) -> String {
    match kind {
        Some(CatalogSyncErrorKind::Network) => if zh {
            "无法连接 models.dev，请检查网络"
        } else {
            "Could not reach models.dev — check your connection"
        }
        .into(),
        Some(CatalogSyncErrorKind::Timeout) => if zh {
            "models.dev 在 30 秒内没有响应"
        } else {
            "models.dev did not respond within 30s"
        }
        .into(),
        Some(CatalogSyncErrorKind::Http) => {
            let code = status.unwrap_or(0);
            if code >= 500 {
                if zh {
                    format!("models.dev 暂时不可用（HTTP {code}）")
                } else {
                    format!("models.dev temporarily unavailable (HTTP {code})")
                }
            } else if zh {
                format!("models.dev 拒绝了请求（HTTP {code}）")
            } else {
                format!("models.dev rejected the request (HTTP {code})")
            }
        }
        Some(CatalogSyncErrorKind::TooLarge) => if zh {
            "下载内容超过 16 MiB，已放弃"
        } else {
            "Catalog download exceeded 16 MiB limit"
        }
        .into(),
        Some(CatalogSyncErrorKind::Invalid) => if zh {
            "models.dev 返回的不是有效的模型目录"
        } else {
            "models.dev returned an invalid catalog payload"
        }
        .into(),
        Some(CatalogSyncErrorKind::TooFew) => if zh {
            "models.dev 返回的目录模型过少，可能不完整，已放弃"
        } else {
            "Catalog had too few models to trust; kept local cache"
        }
        .into(),
        _ => raw.unwrap_or("Sync failed").to_owned(),
    }
}

/// Formats an [`EffectivePriceView`] concisely for inline badges and provider tables.
#[must_use]
pub fn format_effective_price(view: &EffectivePriceView, zh: bool) -> String {
    let p = &view.price;
    if let Some(per_call) = p.per_call {
        let amt = format_money(per_call, &p.currency);
        return if zh {
            format!("{amt}/次")
        } else {
            format!("{amt}/call")
        };
    }
    let inp = format_money(p.input_per_mtok, &p.currency);
    let out = format_money(p.output_per_mtok, &p.currency);
    format!("{inp} / {out}")
}

/// Describes the rule that produced an effective price (`PriceBasis`).
#[must_use]
pub fn format_basis(basis: &PriceBasis, zh: bool) -> String {
    let fam = basis
        .family_name
        .as_deref()
        .or(basis.family.as_deref())
        .unwrap_or("");
    let base = match basis.kind {
        PriceBasisKind::ProviderModel => {
            if zh {
                "此服务商单独定价".into()
            } else {
                "Provider override".into()
            }
        }
        PriceBasisKind::Provider => {
            if zh {
                format!("服务商目录价 ({fam})")
            } else {
                format!("Provider catalog ({fam})")
            }
        }
        PriceBasisKind::Model => {
            if zh {
                "模型默认覆盖价".into()
            } else {
                "Model default override".into()
            }
        }
        PriceBasisKind::Official => {
            if fam.is_empty() {
                if zh {
                    "官方价".into()
                } else {
                    "Official".into()
                }
            } else if zh {
                format!("官方价 ({fam})")
            } else {
                format!("Official ({fam})")
            }
        }
        PriceBasisKind::Reference => {
            if fam.is_empty() {
                if zh {
                    "参考价".into()
                } else {
                    "Reference".into()
                }
            } else if zh {
                format!("参考价 ({fam})")
            } else {
                format!("Reference ({fam})")
            }
        }
    };
    if let Some(relay) = &basis.relay {
        format!("{base} ×{}", relay.multiplier)
    } else {
        base
    }
}

/// Display tuple for one catalog table row: `(input_str, output_str, cache_str, mode_label, mode_tone)`.
#[must_use]
pub fn format_row_prices(
    row: &ModelCatalogRow,
    zh: bool,
) -> (String, String, String, String, Tone) {
    match row.pricing.mode {
        PricingMode::Fixed => {
            let cur = &row.pricing.currency;
            let amt = row
                .pricing
                .per_call
                .map_or_else(|| "—".into(), |v| format_money(v, cur));
            let cell = if zh {
                format!("{amt}/次")
            } else {
                format!("{amt}/call")
            };
            let label = if zh { "按次" } else { "Per call" }.into();
            (cell, "—".into(), "—".into(), label, Tone::Accent)
        }
        PricingMode::Custom => {
            let cur = &row.pricing.currency;
            let inp = row
                .pricing
                .input_per_mtok
                .map_or_else(|| "—".into(), |v| format_money(v, cur));
            let out = row
                .pricing
                .output_per_mtok
                .map_or_else(|| "—".into(), |v| format_money(v, cur));
            let cache = format_cache_pair(
                row.pricing.cache_read_per_mtok,
                row.pricing.cache_write_per_mtok,
                cur,
            );
            let label = if zh { "自定义" } else { "Custom" }.into();
            (inp, out, cache, label, Tone::Accent)
        }
        PricingMode::Dynamic => {
            let cur = &row.currency;
            let has_price = row.input_per_mtok.is_some() && row.output_per_mtok.is_some();
            let inp = row
                .input_per_mtok
                .map_or_else(|| "—".into(), |v| format_money(v, cur));
            let out = row
                .output_per_mtok
                .map_or_else(|| "—".into(), |v| format_money(v, cur));
            let cache = format_cache_pair(row.cache_read_per_mtok, row.cache_write_per_mtok, cur);
            if let Some(fam) = &row.pricing.catalog_family {
                let label = if zh {
                    format!("{fam} 目录")
                } else {
                    format!("{fam} catalog")
                };
                (inp, out, cache, label, Tone::Accent)
            } else if !has_price {
                (
                    "—".into(),
                    "—".into(),
                    "—".into(),
                    if zh { "无价格" } else { "No price" }.into(),
                    Tone::Warning,
                )
            } else {
                let label = match row.price_basis {
                    Some(DefaultBasis::Reference) => {
                        if zh {
                            "参考价"
                        } else {
                            "Reference"
                        }
                    }
                    _ => {
                        if zh {
                            "跟随目录"
                        } else {
                            "Catalog"
                        }
                    }
                };
                (inp, out, cache, label.into(), Tone::Neutral)
            }
        }
    }
}

fn format_cache_pair(read: Option<f64>, write: Option<f64>, currency: &str) -> String {
    match (read, write) {
        (None, None) => "—".into(),
        (Some(r), None) => format_money(r, currency),
        (None, Some(w)) => format!("— / {}", format_money(w, currency)),
        (Some(r), Some(w)) => format!(
            "{} / {}",
            format_money(r, currency),
            format_money(w, currency)
        ),
    }
}

/// Parses an optional trimmed floating-point price field; empty string returns `Ok(None)`.
pub fn parse_opt_f64(raw: &str, field: &str) -> Result<Option<f64>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let val: f64 = trimmed
        .parse()
        .map_err(|_| format!("{field}: invalid number"))?;
    if !val.is_finite() || val < 0.0 {
        return Err(format!("{field}: must be >= 0"));
    }
    Ok(Some(val))
}

/// Parses an optional trimmed positive integer token count; empty string returns `Ok(None)`.
pub fn parse_opt_u64(raw: &str, field: &str) -> Result<Option<u64>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let val: u64 = trimmed
        .parse()
        .map_err(|_| format!("{field}: must be a positive integer"))?;
    if val == 0 {
        return Err(format!("{field}: must be > 0"));
    }
    Ok(Some(val))
}

/// Compact plain float string for form inputs (`2` -> `"2"`, `0.15` -> `"0.15"`).
#[must_use]
pub fn opt_f64_input(v: Option<f64>) -> String {
    v.map_or_else(String::new, |n| format!("{n}"))
}
