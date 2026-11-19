//! Headless Command Palette (`⌘K`) index, fuzzy matcher, and frecency ranker.

use std::collections::BTreeMap;

use pingdeck_core::model::settings::Language;

use super::shell::{NavModule, SettingsSection, UtilityDialog};

/// Visual category in the command palette list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteCategory {
    NeedsYou,
    Navigation,
    Workspace,
    Settings,
    Utility,
    Command,
}

/// Concrete intent dispatched when a palette item is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    Navigate(NavModule),
    OpenSettings(SettingsSection),
    OpenUtility(UtilityDialog),
    SwitchWorkspace(String),
    OpenChatSession(String),
    ToggleSidebar,
    CycleThemeMode,
    ToggleLanguage,
}

/// One searchable entry in the command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    pub id: String,
    pub category: PaletteCategory,
    pub title: String,
    pub subtitle: Option<String>,
    pub keywords: String,
    pub shortcut: Option<String>,
    pub action: PaletteAction,
}

/// Pure-Rust command palette ranking engine (100% unit-testable without UI).
#[derive(Debug, Default)]
pub struct PaletteEngine {
    items: Vec<PaletteItem>,
}

impl PaletteEngine {
    #[must_use]
    pub fn build_default(
        lang: Language,
        workspaces: &[(String, String)],
        attention_sessions: &[(String, String)],
    ) -> Self {
        let zh = lang == Language::ZhCn;
        let mut items = Vec::with_capacity(64);

        for (session_id, title) in attention_sessions {
            items.push(PaletteItem {
                id: format!("attn:{session_id}"),
                category: PaletteCategory::NeedsYou,
                title: title.clone(),
                subtitle: Some(if zh { "等待批准" } else { "Needs approval" }.into()),
                keywords: format!("approval gate chat {title}"),
                shortcut: None,
                action: PaletteAction::OpenChatSession(session_id.clone()),
            });
        }

        for module in NavModule::ALL {
            let (zh_label, en_label) = module.label_pair();
            items.push(PaletteItem {
                id: format!("nav:{}", module.shortcut_digit()),
                category: PaletteCategory::Navigation,
                title: if zh { zh_label } else { en_label }.into(),
                subtitle: Some(if zh { en_label } else { zh_label }.into()),
                keywords: format!("{zh_label} {en_label}"),
                shortcut: Some(format!("⌘{}", module.shortcut_digit())),
                action: PaletteAction::Navigate(module),
            });
        }

        for (ws_id, ws_name) in workspaces {
            items.push(PaletteItem {
                id: format!("ws:{ws_id}"),
                category: PaletteCategory::Workspace,
                title: ws_name.clone(),
                subtitle: Some(
                    if zh {
                        "切换工作区"
                    } else {
                        "Switch workspace"
                    }
                    .into(),
                ),
                keywords: format!("workspace 工作区 {ws_name}"),
                shortcut: None,
                action: PaletteAction::SwitchWorkspace(ws_id.clone()),
            });
        }

        for utility in UtilityDialog::ALL {
            let (zh_label, en_label) = utility.label_pair();
            items.push(PaletteItem {
                id: format!("util:{en_label}"),
                category: PaletteCategory::Utility,
                title: if zh { zh_label } else { en_label }.into(),
                subtitle: Some(if zh { "工具" } else { "Utility" }.into()),
                keywords: format!("{zh_label} {en_label}"),
                shortcut: None,
                action: PaletteAction::OpenUtility(utility),
            });
        }

        for section in SettingsSection::ALL {
            let (zh_label, en_label) = section.label_pair();
            let (grp_zh, grp_en) = section.group().label_pair();
            items.push(PaletteItem {
                id: format!("settings:{en_label}"),
                category: PaletteCategory::Settings,
                title: if zh {
                    format!("{grp_zh} › {zh_label}")
                } else {
                    format!("{grp_en} › {en_label}")
                },
                subtitle: Some(if zh { "设置" } else { "Settings" }.into()),
                keywords: format!("settings 设置 {grp_zh} {grp_en} {zh_label} {en_label}"),
                shortcut: None,
                action: PaletteAction::OpenSettings(section),
            });
        }

        items.push(PaletteItem {
            id: "cmd:toggle-sidebar".into(),
            category: PaletteCategory::Command,
            title: if zh {
                "折叠/展开侧栏"
            } else {
                "Toggle Sidebar"
            }
            .into(),
            subtitle: None,
            keywords: "sidebar 侧栏 toggle 折叠".into(),
            shortcut: Some("⌘B".into()),
            action: PaletteAction::ToggleSidebar,
        });
        items.push(PaletteItem {
            id: "cmd:cycle-theme".into(),
            category: PaletteCategory::Command,
            title: if zh {
                "切换浅色/深色外观"
            } else {
                "Toggle Light / Dark Theme"
            }
            .into(),
            subtitle: None,
            keywords: "theme dark light 浅色 深色 主题 外观".into(),
            shortcut: None,
            action: PaletteAction::CycleThemeMode,
        });
        items.push(PaletteItem {
            id: "cmd:toggle-lang".into(),
            category: PaletteCategory::Command,
            title: if zh {
                "切换界面语言 (中/En)"
            } else {
                "Switch Language (Zh/En)"
            }
            .into(),
            subtitle: None,
            keywords: "language chinese english 语言 中文 英文".into(),
            shortcut: None,
            action: PaletteAction::ToggleLanguage,
        });

        Self { items }
    }

    /// Searches and ranks items by fuzzy score + frecency boost.
    #[must_use]
    pub fn search<'a>(
        &'a self,
        query: &str,
        frecency: &BTreeMap<String, (u32, i64)>,
        limit: usize,
    ) -> Vec<&'a PaletteItem> {
        let q = query.trim().to_lowercase();
        let mut scored: Vec<(i64, usize, &PaletteItem)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                let base = if q.is_empty() {
                    category_base_priority(item.category)
                } else {
                    fuzzy_score(&q, &item.title, &item.keywords)?
                };
                let boost = frecency
                    .get(&item.id)
                    .map_or(0_i64, |(count, _)| i64::from((*count).min(50)) * 8);
                Some((base + boost, idx, item))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, _, item)| item)
            .collect()
    }
}

fn category_base_priority(cat: PaletteCategory) -> i64 {
    match cat {
        PaletteCategory::NeedsYou => 500,
        PaletteCategory::Navigation => 300,
        PaletteCategory::Command => 220,
        PaletteCategory::Workspace => 200,
        PaletteCategory::Utility => 160,
        PaletteCategory::Settings => 100,
    }
}

fn fuzzy_score(query: &str, title: &str, keywords: &str) -> Option<i64> {
    let title_l = title.to_lowercase();
    let kw_l = keywords.to_lowercase();
    if title_l == query {
        return Some(1_000);
    }
    if title_l.starts_with(query) {
        return Some(800);
    }
    if title_l.contains(query) {
        return Some(600);
    }
    if kw_l.contains(query) {
        return Some(400);
    }
    // Subsequence match over combined haystack.
    let mut chars = query.chars().peekable();
    let mut consecutive = 0_i64;
    let mut score = 100_i64;
    for ch in title_l
        .chars()
        .chain(std::iter::once(' '))
        .chain(kw_l.chars())
    {
        if chars.peek() == Some(&ch) {
            chars.next();
            consecutive += 1;
            score += 10 + consecutive * 4;
            if chars.peek().is_none() {
                return Some(score);
            }
        } else {
            consecutive = 0;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_needs_you_first_and_matches_bilingual_queries() {
        let engine = PaletteEngine::build_default(
            Language::ZhCn,
            &[("ws1".into(), "Payment API".into())],
            &[("chat1".into(), "Fix 502".into())],
        );
        let empty = BTreeMap::new();
        let top = engine.search("", &empty, 5);
        assert_eq!(top[0].category, PaletteCategory::NeedsYou);

        let hits = engine.search("mcp", &empty, 5);
        assert!(
            hits.iter()
                .any(|h| h.action == PaletteAction::OpenSettings(SettingsSection::Mcp))
        );

        let ws_hits = engine.search("payment", &empty, 5);
        assert!(
            ws_hits
                .iter()
                .any(|h| h.action == PaletteAction::SwitchWorkspace("ws1".into()))
        );
    }
}
