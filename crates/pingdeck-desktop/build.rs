//! Compile-time typed i18n code generator for `locales/<area>/{en,zh-CN}.json`.
//!
//! Parses all 14 bilingual locale catalogues at build time and emits:
//! - Zero-allocation typed accessors (`Tr::new(lang).<area>().<key>(...)`),
//! - Typed plural helpers for `.one` / `.other` pairs, and
//! - A sorted static lookup table for genuine dynamic keys (e.g., built-in tool descriptions),
//!   eliminating runtime JSON parsing and `HashMap` heap allocations.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

struct AreaSpec {
    dir: &'static str,
    struct_name: &'static str,
    strip_prefix: Option<&'static str>,
}

const AREAS: &[AreaSpec] = &[
    AreaSpec {
        dir: "chat",
        struct_name: "Chat",
        strip_prefix: Some("chat."),
    },
    AreaSpec {
        dir: "common",
        struct_name: "Common",
        strip_prefix: Some("common."),
    },
    AreaSpec {
        dir: "environments",
        struct_name: "Environments",
        strip_prefix: Some("env."),
    },
    AreaSpec {
        dir: "exchange",
        struct_name: "Exchange",
        strip_prefix: Some("ex."),
    },
    AreaSpec {
        dir: "flows",
        struct_name: "Flows",
        strip_prefix: Some("flow."),
    },
    AreaSpec {
        dir: "history",
        struct_name: "History",
        strip_prefix: Some("hist."),
    },
    AreaSpec {
        dir: "mock",
        struct_name: "Mock",
        strip_prefix: Some("mock."),
    },
    AreaSpec {
        dir: "providers",
        struct_name: "Providers",
        strip_prefix: None,
    },
    AreaSpec {
        dir: "requests",
        struct_name: "Requests",
        strip_prefix: Some("req."),
    },
    AreaSpec {
        dir: "settings",
        struct_name: "Settings",
        strip_prefix: Some("set."),
    },
    AreaSpec {
        dir: "shared",
        struct_name: "Shared",
        strip_prefix: Some("field."),
    },
    AreaSpec {
        dir: "shell",
        struct_name: "Shell",
        strip_prefix: Some("shell."),
    },
    AreaSpec {
        dir: "usage",
        struct_name: "Usage",
        strip_prefix: None,
    },
    AreaSpec {
        dir: "utility",
        struct_name: "Utility",
        strip_prefix: Some("util."),
    },
];

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let locales_root = manifest_dir.join("../../locales");
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    let mut all_entries: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut area_codes: Vec<String> = Vec::with_capacity(AREAS.len());

    for area in AREAS {
        let en_rel = format!("../../locales/{}/en.json", area.dir);
        let zh_rel = format!("../../locales/{}/zh-CN.json", area.dir);
        println!("cargo:rerun-if-changed={en_rel}");
        println!("cargo:rerun-if-changed={zh_rel}");

        let en_path = locales_root.join(area.dir).join("en.json");
        let zh_path = locales_root.join(area.dir).join("zh-CN.json");

        let en_raw = fs::read_to_string(&en_path)
            .map_err(|e| format!("failed to read {}: {e}", en_path.display()))?;
        let zh_raw = fs::read_to_string(&zh_path)
            .map_err(|e| format!("failed to read {}: {e}", zh_path.display()))?;

        let en: BTreeMap<String, String> = serde_json::from_str(&en_raw)
            .map_err(|e| format!("failed to parse {}: {e}", en_path.display()))?;
        let zh: BTreeMap<String, String> = serde_json::from_str(&zh_raw)
            .map_err(|e| format!("failed to parse {}: {e}", zh_path.display()))?;

        let en_keys: BTreeSet<&String> = en.keys().collect();
        let zh_keys: BTreeSet<&String> = zh.keys().collect();
        if en_keys != zh_keys {
            return Err(format!(
                "locale area `{}` has mismatched keys between en.json and zh-CN.json",
                area.dir
            )
            .into());
        }

        let mut seen_idents: BTreeMap<String, String> = BTreeMap::new();
        let mut key_meta: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();

        let mut buf = String::with_capacity(en.len() * 192);
        writeln!(
            buf,
            "#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct {}(pub Language);\n",
            area.struct_name
        )?;
        writeln!(buf, "impl {} {{", area.struct_name)?;

        for (key, en_val) in &en {
            let Some(zh_val) = zh.get(key) else {
                continue;
            };
            if all_entries
                .insert(key.clone(), (zh_val.clone(), en_val.clone()))
                .is_some()
            {
                return Err(format!("duplicate i18n key across locale areas: `{key}`").into());
            }

            let en_ph = extract_placeholders(en_val);
            let zh_ph = extract_placeholders(zh_val);
            let en_set: BTreeSet<&str> = en_ph.iter().map(String::as_str).collect();
            let zh_set: BTreeSet<&str> = zh_ph.iter().map(String::as_str).collect();
            if en_set != zh_set {
                return Err(format!(
                    "placeholder mismatch for `{key}` in `{}`: en={en_ph:?}, zh-CN={zh_ph:?}",
                    area.dir
                )
                .into());
            }

            let stripped = area
                .strip_prefix
                .and_then(|p| key.strip_prefix(p))
                .unwrap_or(key.as_str());
            let ident = to_snake_ident(stripped);
            if let Some(prev) = seen_idents.insert(ident.clone(), key.clone()) {
                return Err(format!(
                    "i18n method name collision in `{}`: `{prev}` and `{key}` both map to `{ident}`",
                    area.dir
                )
                .into());
            }

            emit_method(&mut buf, &ident, en_val, zh_val, &en_ph)?;
            key_meta.insert(key.clone(), (ident, en_ph));
        }

        // Emit typed plural helpers for `<base>.one` + `<base>.other` pairs.
        for key in en.keys() {
            let Some(base) = key.strip_suffix(".one") else {
                continue;
            };
            let other_key = format!("{base}.other");
            let (Some((one_ident, one_ph)), Some((other_ident, other_ph))) =
                (key_meta.get(key), key_meta.get(&other_key))
            else {
                continue;
            };
            let stripped_base = area
                .strip_prefix
                .and_then(|p| base.strip_prefix(p))
                .unwrap_or(base);
            let base_ident = to_snake_ident(stripped_base);
            if seen_idents.contains_key(&base_ident) {
                continue;
            }
            // Only emit when `other` takes `count` and any extra params match `one`.
            if !other_ph.iter().any(|p| p == "count") {
                continue;
            }
            let one_extras: Vec<&String> = one_ph.iter().filter(|p| *p != "count").collect();
            let other_extras: Vec<&String> = other_ph.iter().filter(|p| *p != "count").collect();
            if one_extras != other_extras {
                continue;
            }
            seen_idents.insert(base_ident.clone(), format!("{base}.{{one,other}}"));
            emit_plural_helper(
                &mut buf,
                &base_ident,
                one_ident,
                one_ph,
                other_ident,
                other_ph,
            )?;
        }

        writeln!(buf, "}}\n")?;
        area_codes.push(buf);
    }

    let mut out = String::with_capacity(all_entries.len() * 256);
    writeln!(
        out,
        "// @generated by crates/pingdeck-desktop/build.rs from locales/<area>/{{en,zh-CN}}.json."
    )?;
    writeln!(out, "use pingdeck_core::model::settings::Language;\n")?;
    writeln!(
        out,
        "pub const TOTAL_KEYS: usize = {};\n",
        all_entries.len()
    )?;

    writeln!(out, "pub(crate) const ENTRIES: &[(&str, &str, &str)] = &[")?;
    for (key, (zh_val, en_val)) in &all_entries {
        writeln!(
            out,
            "    ({}, {}, {}),",
            quote_rust(key),
            quote_rust(zh_val),
            quote_rust(en_val)
        )?;
    }
    writeln!(out, "];\n")?;

    writeln!(
        out,
        "/// Typed zero-cost i18n facade bound to a [`Language`]."
    )?;
    writeln!(out, "#[derive(Debug, Clone, Copy, PartialEq, Eq)]")?;
    writeln!(out, "pub struct Tr(pub Language);\n")?;
    writeln!(out, "impl Tr {{")?;
    writeln!(out, "    #[must_use]")?;
    writeln!(
        out,
        "    pub const fn new(lang: Language) -> Self {{ Self(lang) }}"
    )?;
    writeln!(out, "    #[must_use]")?;
    writeln!(out, "    pub const fn lang(self) -> Language {{ self.0 }}")?;
    for area in AREAS {
        writeln!(out, "    #[must_use]")?;
        writeln!(
            out,
            "    pub const fn {}(self) -> {} {{ {}(self.0) }}",
            area.dir, area.struct_name, area.struct_name
        )?;
    }
    writeln!(out, "}}\n")?;

    for code in area_codes {
        out.push_str(&code);
    }

    fs::write(out_dir.join("i18n_generated.rs"), out)?;
    Ok(())
}

fn emit_method(
    buf: &mut String,
    ident: &str,
    en_val: &str,
    zh_val: &str,
    placeholders: &[String],
) -> Result<(), Box<dyn Error>> {
    if placeholders.is_empty() {
        writeln!(buf, "    #[must_use]")?;
        writeln!(buf, "    pub const fn {ident}(self) -> &'static str {{")?;
        writeln!(buf, "        match self.0 {{")?;
        writeln!(buf, "            Language::ZhCn => {},", quote_rust(zh_val))?;
        writeln!(buf, "            Language::En => {},", quote_rust(en_val))?;
        writeln!(buf, "        }}")?;
        writeln!(buf, "    }}")?;
        return Ok(());
    }

    let param_map = build_param_map(placeholders);
    let params_sig: Vec<String> = placeholders
        .iter()
        .map(|raw| {
            let safe = &param_map[raw.as_str()];
            format!("{safe}: impl ::core::fmt::Display")
        })
        .collect();
    let zh_fmt = build_format_template(zh_val, &param_map);
    let en_fmt = build_format_template(en_val, &param_map);

    writeln!(buf, "    #[must_use]")?;
    writeln!(
        buf,
        "    pub fn {ident}(self, {}) -> String {{",
        params_sig.join(", ")
    )?;
    writeln!(buf, "        match self.0 {{")?;
    writeln!(
        buf,
        "            Language::ZhCn => format!({}),",
        quote_rust(&zh_fmt)
    )?;
    writeln!(
        buf,
        "            Language::En => format!({}),",
        quote_rust(&en_fmt)
    )?;
    writeln!(buf, "        }}")?;
    writeln!(buf, "    }}")?;
    Ok(())
}

fn emit_plural_helper(
    buf: &mut String,
    base_ident: &str,
    one_ident: &str,
    one_ph: &[String],
    other_ident: &str,
    other_ph: &[String],
) -> Result<(), Box<dyn Error>> {
    let param_map = build_param_map(other_ph);
    let mut sig_parts = Vec::with_capacity(other_ph.len());
    for raw in other_ph {
        let safe = &param_map[raw.as_str()];
        if raw == "count" {
            sig_parts.push(format!("{safe}: usize"));
        } else {
            sig_parts.push(format!("{safe}: impl ::core::fmt::Display"));
        }
    }
    let one_args: Vec<&str> = one_ph
        .iter()
        .map(|raw| param_map[raw.as_str()].as_str())
        .collect();
    let other_args: Vec<&str> = other_ph
        .iter()
        .map(|raw| param_map[raw.as_str()].as_str())
        .collect();

    writeln!(buf, "    #[must_use]")?;
    writeln!(
        buf,
        "    pub fn {base_ident}(self, {}) -> String {{",
        sig_parts.join(", ")
    )?;
    if one_args.is_empty() {
        writeln!(buf, "        if count == 1 {{")?;
        writeln!(buf, "            self.{one_ident}().to_owned()")?;
        writeln!(buf, "        }} else {{")?;
        writeln!(
            buf,
            "            self.{other_ident}({})",
            other_args.join(", ")
        )?;
        writeln!(buf, "        }}")?;
    } else {
        writeln!(buf, "        if count == 1 {{")?;
        writeln!(buf, "            self.{one_ident}({})", one_args.join(", "))?;
        writeln!(buf, "        }} else {{")?;
        writeln!(
            buf,
            "            self.{other_ident}({})",
            other_args.join(", ")
        )?;
        writeln!(buf, "        }}")?;
    }
    writeln!(buf, "    }}")?;
    Ok(())
}

/// Extracts unique `{ident}` placeholders in order of first appearance, treating `{{...}}` as
/// literal double-brace template text (e.g. `{{variable}}`).
fn extract_placeholders(s: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();

    while i < len {
        if bytes[i] == b'{' {
            if i + 1 < len && bytes[i + 1] == b'{' {
                i += 2;
                continue;
            }
            if let Some((name, end)) = match_placeholder(s, i) {
                if seen.insert(name.to_owned()) {
                    out.push(name.to_owned());
                }
                i = end;
                continue;
            }
        } else if bytes[i] == b'}' && i + 1 < len && bytes[i + 1] == b'}' {
            i += 2;
            continue;
        }
        i += 1;
    }
    out
}

/// Converts a locale message into a Rust `format!` template string, mapping `{name}` placeholders
/// to `{safe_ident}` and escaping all literal `{` / `}` (including `{{variable}}`).
fn build_format_template(s: &str, param_map: &BTreeMap<String, String>) -> String {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut out = String::with_capacity(len + 16);

    while i < len {
        if bytes[i] == b'{' {
            if i + 1 < len && bytes[i + 1] == b'{' {
                out.push_str("{{{{");
                i += 2;
                continue;
            }
            if let Some((name, end)) = match_placeholder(s, i)
                && let Some(safe) = param_map.get(name)
            {
                out.push('{');
                out.push_str(safe);
                out.push('}');
                i = end;
                continue;
            }
            out.push_str("{{");
            i += 1;
            continue;
        }
        if bytes[i] == b'}' {
            if i + 1 < len && bytes[i + 1] == b'}' {
                out.push_str("}}}}");
                i += 2;
                continue;
            }
            out.push_str("}}");
            i += 1;
            continue;
        }
        if let Some(ch) = s[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

fn match_placeholder(s: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    if start >= len || bytes[start] != b'{' {
        return None;
    }
    let mut j = start + 1;
    if j >= len || !(bytes[j].is_ascii_alphabetic() || bytes[j] == b'_') {
        return None;
    }
    j += 1;
    while j < len && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        j += 1;
    }
    if j < len && bytes[j] == b'}' && !(j + 1 < len && bytes[j + 1] == b'}') {
        Some((&s[start + 1..j], j + 1))
    } else {
        None
    }
}

fn build_param_map(placeholders: &[String]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut used = BTreeSet::new();
    for raw in placeholders {
        let base = to_snake_ident(raw);
        let mut candidate = base.clone();
        let mut n = 2usize;
        while !used.insert(candidate.clone()) {
            candidate = format!("{base}_{n}");
            n += 1;
        }
        map.insert(raw.clone(), candidate);
    }
    map
}

fn to_snake_ident(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    let chars: Vec<char> = raw.chars().collect();
    let mut prev_underscore = true;

    for (idx, &ch) in chars.iter().enumerate() {
        if !ch.is_ascii_alphanumeric() {
            if !prev_underscore {
                out.push('_');
                prev_underscore = true;
            }
            continue;
        }
        if ch.is_ascii_uppercase() {
            let prev_is_lower_or_digit =
                idx > 0 && (chars[idx - 1].is_ascii_lowercase() || chars[idx - 1].is_ascii_digit());
            let prev_is_upper_and_next_lower = idx > 0
                && chars[idx - 1].is_ascii_uppercase()
                && idx + 1 < chars.len()
                && chars[idx + 1].is_ascii_lowercase();
            if !prev_underscore && (prev_is_lower_or_digit || prev_is_upper_and_next_lower) {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else {
            out.push(ch);
            prev_underscore = false;
        }
    }

    let trimmed = out.trim_matches('_');
    sanitize_ident(trimmed)
}

fn sanitize_ident(raw: &str) -> String {
    let mut s = if raw.is_empty() {
        "empty".to_owned()
    } else if raw.as_bytes()[0].is_ascii_digit() {
        format!("n_{raw}")
    } else {
        raw.to_owned()
    };
    if is_rust_keyword(&s) {
        s.push('_');
    }
    s
}

fn is_rust_keyword(s: &str) -> bool {
    matches!(
        s,
        "as" | "break"
            | "const"
            | "continue"
            | "crate"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "async"
            | "await"
            | "dyn"
            | "abstract"
            | "become"
            | "box"
            | "do"
            | "final"
            | "macro"
            | "override"
            | "priv"
            | "try"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
            | "gen"
    )
}

fn quote_rust(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
