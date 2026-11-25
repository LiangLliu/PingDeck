//! i18n catalogue and source-level verification (`cargo xtask check-i18n`).
//!
//! Enforces three invariants across `locales/<area>/{en,zh-CN}.json` and the desktop/GPUI crates:
//! 1. Catalogue parity, placeholder/plural consistency, CJK spacing, and copy/security rules.
//! 2. Zero dead/unused locale keys (every key must be referenced by a compile-time typed accessor
//!    or an explicitly allowlisted dynamic prefix).
//! 3. Zero hardcoded bilingual branches (`if zh`, `fn is_zh`, `label_zh`/`label_en`/`label_pair`)
//!    or raw CJK UI string literals in non-test Rust code.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use regex::Regex;

use crate::{err_line, out_line};

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

type Catalogue = BTreeMap<String, String>;

fn load_catalogue(path: &Path) -> Result<Catalogue, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str::<Catalogue>(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

fn placeholders(value: &str, re: &Regex) -> Vec<String> {
    let stripped = strip_double_brace_templates(value);
    let mut names: Vec<String> = re
        .captures_iter(&stripped)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_owned()))
        .collect();
    names.sort();
    names
}

/// Removes literal `{{...}}` template examples (e.g. `{{variable}}`) so placeholder checks only
/// inspect real `{name}` interpolation holes, matching `crates/pingdeck-desktop/build.rs`.
fn strip_double_brace_templates(value: &str) -> String {
    let bytes = value.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        if bytes[i] == b'{' && i + 1 < len && bytes[i + 1] == b'{' {
            if let Some(rel_end) = value[i + 2..].find("}}") {
                i = i + 2 + rel_end + 2;
                continue;
            }
            i += 2;
            continue;
        }
        if let Some(ch) = value[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

/// Converts a locale key suffix into its generated Rust method identifier.
/// Must stay in sync with `crates/pingdeck-desktop/build.rs::to_snake_ident`.
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
    let mut s = if trimmed.is_empty() {
        "empty".to_owned()
    } else if trimmed.as_bytes()[0].is_ascii_digit() {
        format!("n_{trimmed}")
    } else {
        trimmed.to_owned()
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

/// Keys legitimately looked up dynamically via `pingdeck_desktop::i18n::lookup`.
fn is_dynamic_allowlisted(area: &str, key: &str) -> bool {
    if area != "settings" {
        return false;
    }
    let Some(rest) = key.strip_prefix("set.f.tools.") else {
        return false;
    };
    let Some(tool_name) = rest.strip_suffix(".d") else {
        return false;
    };
    !tool_name.is_empty() && !tool_name.contains('.')
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn non_test_prefix(content: &str) -> &str {
    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        if line.trim() == "#[cfg(test)]" {
            return &content[..offset];
        }
        offset += line.len();
    }
    content
}

struct ScannedRust {
    /// Source with comments and string/char literals replaced by spaces (preserving `\n`).
    code: String,
    /// `(1_based_line, literal_contents)` for every normal and raw string literal.
    strings: Vec<(usize, String)>,
}

fn scan_rust_source(src: &str) -> ScannedRust {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut code = String::with_capacity(len);
    let mut strings = Vec::new();

    while i < len {
        let b = bytes[i];

        // Line comment: `// ...`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Block comment (supports Rust nesting): `/* ... */`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let mut depth = 1usize;
            i += 2;
            code.push(' ');
            while i < len && depth > 0 {
                if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    if bytes[i] == b'\n' {
                        line += 1;
                        code.push('\n');
                    }
                    i += 1;
                }
            }
            continue;
        }

        // Raw string literal: `r"..."`, `r#"..."#`, `br#"..."#`
        let raw_start = if b == b'r' {
            Some(i + 1)
        } else if b == b'b' && i + 1 < len && bytes[i + 1] == b'r' {
            Some(i + 2)
        } else {
            None
        };
        if let Some(after_r) = raw_start {
            let prev_is_ident =
                i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            if !prev_is_ident {
                let mut hashes = 0usize;
                let mut j = after_r;
                while j < len && bytes[j] == b'#' {
                    hashes += 1;
                    j += 1;
                }
                if j < len && bytes[j] == b'"' {
                    let start_line = line;
                    j += 1;
                    let content_start = j;
                    code.push(' ');
                    let mut closed = false;
                    while j < len {
                        if bytes[j] == b'"' {
                            let mut matched = 0usize;
                            while matched < hashes
                                && j + 1 + matched < len
                                && bytes[j + 1 + matched] == b'#'
                            {
                                matched += 1;
                            }
                            if matched == hashes {
                                strings.push((start_line, src[content_start..j].to_owned()));
                                j += 1 + hashes;
                                closed = true;
                                break;
                            }
                        }
                        if bytes[j] == b'\n' {
                            line += 1;
                            code.push('\n');
                        }
                        j += 1;
                    }
                    if !closed {
                        strings.push((start_line, src[content_start..len].to_owned()));
                    }
                    i = j;
                    continue;
                }
            }
        }

        // Normal or byte string literal: `"..."`, `b"..."`
        let str_quote = if b == b'"' {
            Some(i)
        } else if b == b'b' && i + 1 < len && bytes[i + 1] == b'"' {
            let prev_is_ident =
                i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            (!prev_is_ident).then_some(i + 1)
        } else {
            None
        };
        if let Some(q) = str_quote {
            let start_line = line;
            let mut j = q + 1;
            let content_start = j;
            code.push(' ');
            while j < len {
                if bytes[j] == b'\\' {
                    j = (j + 2).min(len);
                    continue;
                }
                if bytes[j] == b'"' {
                    strings.push((start_line, src[content_start..j].to_owned()));
                    j += 1;
                    break;
                }
                if bytes[j] == b'\n' {
                    line += 1;
                    code.push('\n');
                }
                j += 1;
            }
            i = j;
            continue;
        }

        // Character literal (distinguished from lifetimes `'a` / `'static`)
        if b == b'\'' {
            if i + 1 < len && bytes[i + 1] == b'\\' {
                let mut j = i + 2;
                while j < len && bytes[j] != b'\'' && bytes[j] != b'\n' {
                    j += 1;
                }
                if j < len && bytes[j] == b'\'' {
                    code.push(' ');
                    i = j + 1;
                    continue;
                }
            } else if let Some(ch) = src[i + 1..].chars().next() {
                let after = i + 1 + ch.len_utf8();
                if after < len && bytes[after] == b'\'' {
                    code.push(' ');
                    i = after + 1;
                    continue;
                }
            }
        }

        if let Some(ch) = src[i..].chars().next() {
            if ch == '\n' {
                line += 1;
            }
            code.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    ScannedRust { code, strings }
}

fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c))
}

fn collect_source_usage_and_check_hardcoding(
    root: &Path,
    problems: &mut Vec<String>,
) -> Result<BTreeMap<&'static str, BTreeSet<String>>, String> {
    let area_pat = AREAS.iter().map(|a| a.dir).collect::<Vec<_>>().join("|");
    let struct_pat = AREAS
        .iter()
        .map(|a| a.struct_name)
        .collect::<Vec<_>>()
        .join("|");

    let direct_re = Regex::new(&format!(
        r"\.({area_pat})\s*\(\s*\)\s*\.\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\("
    ))
    .map_err(|e| format!("internal regex error: {e}"))?;
    let bind_area_re = Regex::new(&format!(
        r"\blet\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*=\s*[^;{{}}]*?\.({area_pat})\s*\(\s*\)\s*;"
    ))
    .map_err(|e| format!("internal regex error: {e}"))?;
    let alias_re = Regex::new(&format!(
        r"\b({struct_pat})\s+as\s+([A-Za-z_][A-Za-z0-9_]*)\b"
    ))
    .map_err(|e| format!("internal regex error: {e}"))?;
    let path_call_re = Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)::([a-zA-Z_][a-zA-Z0-9_]*)\b")
        .map_err(|e| format!("internal regex error: {e}"))?;
    let dot_call_re =
        Regex::new(r"\b([a-zA-Z_][a-zA-Z0-9_]*)\s*\.\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*\(")
            .map_err(|e| format!("internal regex error: {e}"))?;

    let forbidden_if_zh_re = Regex::new(
        r"\bif\s+!?\s*(?:self\.|view\.|this\.)?(?:zh|is_zh)\b|\bfn\s+(?:is_zh|label_zh|label_en|label_pair)\s*\(",
    )
    .map_err(|e| format!("internal regex error: {e}"))?;

    let mut used_by_area: BTreeMap<&'static str, BTreeSet<String>> =
        AREAS.iter().map(|a| (a.dir, BTreeSet::new())).collect();
    let struct_to_area: BTreeMap<&'static str, &'static str> =
        AREAS.iter().map(|a| (a.struct_name, a.dir)).collect();
    let dir_lookup: BTreeMap<&'static str, &'static str> =
        AREAS.iter().map(|a| (a.dir, a.dir)).collect();

    let mut files = Vec::new();
    for rel_dir in ["crates/pingdeck-desktop/src", "crates/pingdeck-gpui/src"] {
        let dir = root.join(rel_dir);
        if dir.exists() {
            collect_rs_files(&dir, &mut files);
        }
    }
    files.sort();

    for file in files {
        let rel = file.strip_prefix(root).unwrap_or(&file);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.starts_with("crates/pingdeck-desktop/src/i18n/")
            || rel_str.ends_with("/ui_tests.rs")
        {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&file) else {
            continue;
        };
        let prod = non_test_prefix(&raw);
        let scanned = scan_rust_source(prod);

        // Guard A: anti-hardcoded bilingual branches & raw CJK UI string literals.
        for (idx, line) in scanned.code.lines().enumerate() {
            if let Some(m) = forbidden_if_zh_re.find(line) {
                problems.push(format!(
                    "{rel_str}:{}: hardcoded bilingual helper/branch `{}` is forbidden (use typed `tr(cx)` / `locales/`)",
                    idx + 1,
                    m.as_str().trim()
                ));
            }
        }
        for (line_no, lit) in &scanned.strings {
            if contains_cjk(lit) {
                let preview: String = lit.chars().take(40).collect();
                problems.push(format!(
                    "{rel_str}:{line_no}: raw CJK UI literal \"{preview}\" is forbidden in Rust source (move to `locales/`)"
                ));
            }
        }

        // Guard B: collect every typed locale method referenced by this file.
        for cap in direct_re.captures_iter(&scanned.code) {
            if let (Some(area_m), Some(method_m)) = (cap.get(1), cap.get(2))
                && let Some(&area_key) = dir_lookup.get(area_m.as_str())
                && let Some(set) = used_by_area.get_mut(area_key)
            {
                set.insert(method_m.as_str().to_owned());
            }
        }

        let mut var_areas: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
        for cap in bind_area_re.captures_iter(&scanned.code) {
            if let (Some(var_m), Some(area_m)) = (cap.get(1), cap.get(2))
                && let Some(&area_key) = dir_lookup.get(area_m.as_str())
            {
                var_areas
                    .entry(var_m.as_str().to_owned())
                    .or_default()
                    .insert(area_key);
            }
        }
        if !var_areas.is_empty() {
            for cap in dot_call_re.captures_iter(&scanned.code) {
                if let (Some(var_m), Some(method_m)) = (cap.get(1), cap.get(2))
                    && let Some(areas) = var_areas.get(var_m.as_str())
                {
                    for &area_key in areas {
                        if let Some(set) = used_by_area.get_mut(area_key) {
                            set.insert(method_m.as_str().to_owned());
                        }
                    }
                }
            }
        }

        let mut symbol_to_area: BTreeMap<String, &'static str> = struct_to_area
            .iter()
            .map(|(&s, &a)| (s.to_owned(), a))
            .collect();
        for cap in alias_re.captures_iter(&scanned.code) {
            if let (Some(orig), Some(alias)) = (cap.get(1), cap.get(2))
                && let Some(&area_key) = struct_to_area.get(orig.as_str())
            {
                symbol_to_area.insert(alias.as_str().to_owned(), area_key);
            }
        }
        for cap in path_call_re.captures_iter(&scanned.code) {
            if let (Some(sym), Some(method_m)) = (cap.get(1), cap.get(2))
                && let Some(&area_key) = symbol_to_area.get(sym.as_str())
                && let Some(set) = used_by_area.get_mut(area_key)
            {
                set.insert(method_m.as_str().to_owned());
            }
        }
    }

    Ok(used_by_area)
}

pub fn check_all(root: &Path) -> Result<usize, Vec<String>> {
    let locales_dir = root.join("locales");
    let mut problems = Vec::new();
    let mut total_keys = 0usize;

    let Ok(placeholder_re) = Regex::new(r"\{(\w+)\}") else {
        return Err(vec!["internal error: invalid placeholder regex".into()]);
    };
    let Ok(err_key_re) = Regex::new(r"(?i)error|fail|crash|unsupported|offline|blocked") else {
        return Err(vec!["internal error: invalid err_key regex".into()]);
    };
    let Ok(bad_err_copy_re) = Regex::new(r"(?i)\b(?:sorry|oops|please)\b|!|抱歉|对不起|请稍后")
    else {
        return Err(vec!["internal error: invalid bad_err_copy regex".into()]);
    };
    let Ok(cjk_spacing_re) = Regex::new(
        r"[\u{4e00}-\u{9fff}][A-Za-z]|[A-Za-z][\u{4e00}-\u{9fff}]|\d[ \t]+(?:毫秒|分钟|小时|字节|个月|[个项条行列步次张份位只台秒分天周月年轮帧])",
    ) else {
        return Err(vec!["internal error: invalid cjk_spacing regex".into()]);
    };
    let Ok(negation_re) = Regex::new(
        r"(?i)unencrypted|without encryption|not encrypted|未加密|不加密|没有加密|添加密",
    ) else {
        return Err(vec!["internal error: invalid negation regex".into()]);
    };
    let Ok(claim_re) = Regex::new(r"(?i)keychain|钥匙串|encrypt|加密") else {
        return Err(vec!["internal error: invalid claim regex".into()]);
    };

    let used_by_area = match collect_source_usage_and_check_hardcoding(root, &mut problems) {
        Ok(map) => map,
        Err(e) => return Err(vec![e]),
    };

    for area in AREAS {
        let en_path = locales_dir.join(area.dir).join("en.json");
        let zh_path = locales_dir.join(area.dir).join("zh-CN.json");

        let en = match load_catalogue(&en_path) {
            Ok(c) => c,
            Err(e) => {
                problems.push(e);
                continue;
            }
        };
        let zh = match load_catalogue(&zh_path) {
            Ok(c) => c,
            Err(e) => {
                problems.push(e);
                continue;
            }
        };

        if en.is_empty() {
            problems.push(format!("area {}: en.json is empty", area.dir));
        }

        let en_keys: BTreeSet<&String> = en.keys().collect();
        let zh_keys: BTreeSet<&String> = zh.keys().collect();
        if en_keys != zh_keys {
            let only_en: Vec<&str> = en_keys.difference(&zh_keys).map(|k| k.as_str()).collect();
            let only_zh: Vec<&str> = zh_keys.difference(&en_keys).map(|k| k.as_str()).collect();
            problems.push(format!(
                "area {}: keys differ: only_en=[{}] | only_zh=[{}]",
                area.dir,
                only_en.join(", "),
                only_zh.join(", ")
            ));
        }

        total_keys += en.len();

        let used_methods = used_by_area.get(area.dir);
        let mut live_keys: BTreeSet<&str> = BTreeSet::new();

        for (key, en_val) in &en {
            if en_val.trim().is_empty() {
                problems.push(format!("{}: {key} (en) is empty", area.dir));
            }
            if let Some(zh_val) = zh.get(key) {
                if zh_val.trim().is_empty() {
                    problems.push(format!("{}: {key} (zh-CN) is empty", area.dir));
                }
                if placeholders(en_val, &placeholder_re) != placeholders(zh_val, &placeholder_re) {
                    problems.push(format!("{}: {key} placeholders differ", area.dir));
                }
                let zh_sub = placeholder_re.replace_all(zh_val, "9");
                if cjk_spacing_re.is_match(&zh_sub) {
                    problems.push(format!(
                        "{}: {key} (zh-CN) CJK/Latin spacing violation: {zh_val}",
                        area.dir
                    ));
                }
            }
            if let Some(prefix) = key.strip_suffix(".one") {
                let other = format!("{prefix}.other");
                if !en.contains_key(&other) {
                    problems.push(format!("{}: {key} needs {other} (en)", area.dir));
                }
                if !zh.contains_key(&other) {
                    problems.push(format!("{}: {key} needs {other} (zh-CN)", area.dir));
                }
            }

            let stripped = area
                .strip_prefix
                .and_then(|p| key.strip_prefix(p))
                .unwrap_or(key.as_str());
            let ident = to_snake_ident(stripped);
            if is_dynamic_allowlisted(area.dir, key)
                || used_methods.is_some_and(|m| m.contains(&ident))
            {
                live_keys.insert(key.as_str());
            }
        }

        // Propagate plural helper references (`<base>(count)` -> `<base>.one` + `<base>.other`).
        for key in en.keys() {
            let Some(base) = key.strip_suffix(".one") else {
                continue;
            };
            let other_key = format!("{base}.other");
            if let Some((other_k, _)) = en.get_key_value(&other_key) {
                let stripped_base = area
                    .strip_prefix
                    .and_then(|p| base.strip_prefix(p))
                    .unwrap_or(base);
                let base_ident = to_snake_ident(stripped_base);
                if used_methods.is_some_and(|m| m.contains(&base_ident))
                    || live_keys.contains(key.as_str())
                    || live_keys.contains(other_k.as_str())
                {
                    live_keys.insert(key.as_str());
                    live_keys.insert(other_k.as_str());
                }
            }
        }

        for key in en.keys() {
            if !live_keys.contains(key.as_str()) {
                problems.push(format!("{}: unused locale key `{key}`", area.dir));
            }
        }

        for (lang, cat) in [("en", &en), ("zh-CN", &zh)] {
            for (key, val) in cat {
                if err_key_re.is_match(key) && bad_err_copy_re.is_match(val) {
                    problems.push(format!(
                        "{}: {key} ({lang}) banned error copy: {val}",
                        area.dir
                    ));
                }
                let stripped = negation_re.replace_all(val, "");
                if claim_re.is_match(&stripped) {
                    problems.push(format!(
                        "{}: {key} ({lang}) mentions keychain/encryption: {val}",
                        area.dir
                    ));
                }
            }
        }
    }

    if problems.is_empty() {
        Ok(total_keys)
    } else {
        Err(problems)
    }
}

pub fn run(root: &Path, args: &[String]) -> ExitCode {
    if !args.is_empty() {
        err_line("usage: cargo xtask check-i18n");
        return ExitCode::from(2);
    }
    match check_all(root) {
        Ok(count) => {
            out_line(&format!(
                "i18n ok: {} areas, {} live bilingual keys verified (0 unused, 0 hardcoded)",
                AREAS.len(),
                count
            ));
            ExitCode::SUCCESS
        }
        Err(problems) => {
            for p in &problems {
                err_line(&format!("i18n error: {p}"));
            }
            err_line(&format!(
                "check-i18n failed with {} issue(s)",
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
    fn repo_locales_pass_all_invariants() {
        let count = check_all(&repo_root()).expect("locales must pass all i18n invariants");
        assert!(
            count > 1000,
            "expected full bilingual catalogue, got {count}"
        );
    }

    #[test]
    fn rust_scanner_splits_comments_strings_and_code() {
        let sample = r##"
// 中文注释不应该触发
/* 嵌套 /* 中文 */ 注释 */
let url = "https://example.com/a";
let bad = "硬编码中文";
let raw = r#"原始中文"#;
if zh { let x = '中'; }
"##;
        let scanned = scan_rust_source(sample);
        assert!(scanned.code.contains("if zh"));
        assert!(!scanned.code.contains("中文注释"));
        assert_eq!(scanned.strings.len(), 3);
        assert!(!contains_cjk(&scanned.strings[0].1));
        assert!(contains_cjk(&scanned.strings[1].1));
        assert!(contains_cjk(&scanned.strings[2].1));
    }
}
