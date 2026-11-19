//! i18n catalogue verification for `locales/<area>/{en,zh-CN}.json`.
//!
//! Ports every invariant previously enforced by `src/repo/i18n.test.ts` into pure Rust so
//! bilingual copy guarantees remain active with zero Node/Vitest dependency.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use regex::Regex;

use crate::{err_line, out_line};

const AREAS: &[&str] = &[
    "chat",
    "common",
    "environments",
    "exchange",
    "flows",
    "history",
    "mock",
    "providers",
    "requests",
    "settings",
    "shared",
    "shell",
    "usage",
    "utility",
];

type Catalogue = BTreeMap<String, String>;

fn load_catalogue(path: &Path) -> Result<Catalogue, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str::<Catalogue>(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

fn placeholders(value: &str, re: &Regex) -> Vec<String> {
    let mut names: Vec<String> = re
        .captures_iter(value)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_owned()))
        .collect();
    names.sort();
    names
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

    for &area in AREAS {
        let en_path = locales_dir.join(area).join("en.json");
        let zh_path = locales_dir.join(area).join("zh-CN.json");

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
            problems.push(format!("area {area}: en.json is empty"));
        }

        let en_keys: BTreeSet<&String> = en.keys().collect();
        let zh_keys: BTreeSet<&String> = zh.keys().collect();
        if en_keys != zh_keys {
            let only_en: Vec<&str> = en_keys.difference(&zh_keys).map(|k| k.as_str()).collect();
            let only_zh: Vec<&str> = zh_keys.difference(&en_keys).map(|k| k.as_str()).collect();
            problems.push(format!(
                "area {area}: keys differ: only_en=[{}] | only_zh=[{}]",
                only_en.join(", "),
                only_zh.join(", ")
            ));
        }

        total_keys += en.len();

        for (key, en_val) in &en {
            if en_val.trim().is_empty() {
                problems.push(format!("{area}: {key} (en) is empty"));
            }
            if let Some(zh_val) = zh.get(key) {
                if zh_val.trim().is_empty() {
                    problems.push(format!("{area}: {key} (zh-CN) is empty"));
                }
                if placeholders(en_val, &placeholder_re) != placeholders(zh_val, &placeholder_re) {
                    problems.push(format!("{area}: {key} placeholders differ"));
                }
                let zh_sub = placeholder_re.replace_all(zh_val, "9");
                if cjk_spacing_re.is_match(&zh_sub) {
                    problems.push(format!(
                        "{area}: {key} (zh-CN) CJK/Latin spacing violation: {zh_val}"
                    ));
                }
            }
            if let Some(prefix) = key.strip_suffix(".one") {
                let other = format!("{prefix}.other");
                if !en.contains_key(&other) {
                    problems.push(format!("{area}: {key} needs {other} (en)"));
                }
                if !zh.contains_key(&other) {
                    problems.push(format!("{area}: {key} needs {other} (zh-CN)"));
                }
            }
        }

        for (lang, cat) in [("en", &en), ("zh-CN", &zh)] {
            for (key, val) in cat {
                if err_key_re.is_match(key) && bad_err_copy_re.is_match(val) {
                    problems.push(format!("{area}: {key} ({lang}) banned error copy: {val}"));
                }
                let stripped = negation_re.replace_all(val, "");
                if claim_re.is_match(&stripped) {
                    problems.push(format!(
                        "{area}: {key} ({lang}) mentions keychain/encryption: {val}"
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
                "i18n ok: {} areas, {} bilingual keys verified",
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
}
