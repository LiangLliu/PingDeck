//! Compile-time embedded bilingual (`zh-CN` / `en`) copy catalogue for PingDeck.
//!
//! Reads directly from repository-root `locales/<area>/{en,zh-CN}.json` so zero translation
//! strings are duplicated and `xtask check-i18n` validates the exact same files.

use std::collections::HashMap;
use std::sync::OnceLock;

use pingdeck_core::model::settings::Language;

const EN_FILES: &[&str] = &[
    include_str!("../../../../locales/chat/en.json"),
    include_str!("../../../../locales/common/en.json"),
    include_str!("../../../../locales/environments/en.json"),
    include_str!("../../../../locales/exchange/en.json"),
    include_str!("../../../../locales/flows/en.json"),
    include_str!("../../../../locales/history/en.json"),
    include_str!("../../../../locales/mock/en.json"),
    include_str!("../../../../locales/providers/en.json"),
    include_str!("../../../../locales/requests/en.json"),
    include_str!("../../../../locales/settings/en.json"),
    include_str!("../../../../locales/shared/en.json"),
    include_str!("../../../../locales/shell/en.json"),
    include_str!("../../../../locales/usage/en.json"),
    include_str!("../../../../locales/utility/en.json"),
];

const ZH_FILES: &[&str] = &[
    include_str!("../../../../locales/chat/zh-CN.json"),
    include_str!("../../../../locales/common/zh-CN.json"),
    include_str!("../../../../locales/environments/zh-CN.json"),
    include_str!("../../../../locales/exchange/zh-CN.json"),
    include_str!("../../../../locales/flows/zh-CN.json"),
    include_str!("../../../../locales/history/zh-CN.json"),
    include_str!("../../../../locales/mock/zh-CN.json"),
    include_str!("../../../../locales/providers/zh-CN.json"),
    include_str!("../../../../locales/requests/zh-CN.json"),
    include_str!("../../../../locales/settings/zh-CN.json"),
    include_str!("../../../../locales/shared/zh-CN.json"),
    include_str!("../../../../locales/shell/zh-CN.json"),
    include_str!("../../../../locales/usage/zh-CN.json"),
    include_str!("../../../../locales/utility/zh-CN.json"),
];

struct Catalogues {
    en: HashMap<String, String>,
    zh: HashMap<String, String>,
}

static CATALOGUES: OnceLock<Catalogues> = OnceLock::new();

fn catalogues() -> &'static Catalogues {
    CATALOGUES.get_or_init(|| Catalogues {
        en: load_merged(EN_FILES),
        zh: load_merged(ZH_FILES),
    })
}

fn load_merged(sources: &[&str]) -> HashMap<String, String> {
    let mut map = HashMap::with_capacity(8192);
    for raw in sources {
        if let Ok(entries) = serde_json::from_str::<HashMap<String, String>>(raw) {
            map.extend(entries);
        }
    }
    map
}

/// Looks up a localized string for `lang`, falling back to English and then `key`.
#[must_use]
pub fn t(lang: Language, key: &str) -> &str {
    let cats = catalogues();
    let primary = match lang {
        Language::ZhCn => &cats.zh,
        Language::En => &cats.en,
    };
    if let Some(val) = primary.get(key) {
        return val.as_str();
    }
    if let Some(val) = cats.en.get(key) {
        return val.as_str();
    }
    key
}

/// Looks up a localized string and substitutes `{name}` placeholders.
#[must_use]
pub fn t_args(lang: Language, key: &str, args: &[(&str, &str)]) -> String {
    let mut out = t(lang, key).to_owned();
    for (name, value) in args {
        let token = format!("{{{name}}}");
        out = out.replace(&token, value);
    }
    out
}

/// Looks up a `.one` / `.other` plural pair and substitutes `{count}`.
#[must_use]
pub fn t_plural(lang: Language, base_key: &str, count: usize) -> String {
    let suffix = if count == 1 { "one" } else { "other" };
    let full_key = format!("{base_key}.{suffix}");
    let count_str = count.to_string();
    t_args(lang, &full_key, &[("count", &count_str)])
}

/// Total number of loaded English keys (diagnostics/tests).
#[must_use]
pub fn total_keys() -> usize {
    catalogues().en.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_full_bilingual_catalogues_and_interpolates() {
        assert!(total_keys() > 1000, "got {} keys", total_keys());
        let zh = t(Language::ZhCn, "shell.nav.requests");
        let en = t(Language::En, "shell.nav.requests");
        assert!(!zh.is_empty() && !en.is_empty());
        assert_ne!(zh, "shell.nav.requests");
        assert_ne!(en, "shell.nav.requests");
    }
}
