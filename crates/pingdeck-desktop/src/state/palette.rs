//! Headless Command Palette (`⌘K`) index, fuzzy matcher, and frecency ranker.

use std::collections::BTreeMap;

use pingdeck_core::model::settings::Language;

use crate::i18n::tr;

use super::shell::{EnvironmentResourceTarget, NavModule, SettingsSection, UtilityDialog};

/// Visual category in the command palette list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteCategory {
    NeedsYou,
    Wizard,
    Navigation,
    Entity,
    Workspace,
    Utility,
    Settings,
    Command,
}

impl PaletteCategory {
    #[must_use]
    pub const fn label(self, lang: Language) -> &'static str {
        let sh = tr(lang).shell();
        match self {
            Self::NeedsYou => sh.palette_cat_needs_you(),
            Self::Wizard => sh.palette_cat_wizard(),
            Self::Navigation => sh.palette_cat_navigation(),
            Self::Entity => sh.palette_cat_entity(),
            Self::Workspace => sh.palette_cat_workspace(),
            Self::Utility => sh.palette_cat_utility(),
            Self::Settings => sh.palette_cat_settings(),
            Self::Command => sh.palette_cat_command(),
        }
    }
}

/// Concrete intent dispatched when a palette item is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    Navigate(NavModule),
    OpenSettings(SettingsSection),
    OpenUtility(UtilityDialog),
    OpenEnvironmentResource(EnvironmentResourceTarget),
    OpenCollectionRunner,
    SwitchWorkspace(String),
    OpenChatSession(String),
    OpenRequestById(String),
    OpenMockServer(String),
    OpenFlow(String),
    OpenProvider(String),
    OpenGateway,
    OpenSniffWizard,
    OpenConnectClientWizard,
    ToggleSidebar,
    CycleThemeMode,
    ToggleLanguage,
}

/// Dynamic workspace and shared-AI entities indexed into the Command Palette.
#[derive(Debug, Default, Clone)]
pub struct PaletteDynamicItems {
    pub requests: Vec<(String, String, String)>,
    pub chats: Vec<(String, String)>,
    pub mocks: Vec<(String, String, bool)>,
    pub flows: Vec<(String, String)>,
    pub providers: Vec<(String, String)>,
}

/// One searchable entry in the command palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    pub id: String,
    pub category: PaletteCategory,
    pub title: String,
    pub subtitle: Option<String>,
    pub badge_label: Option<String>,
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
        Self::build_full(
            lang,
            workspaces,
            attention_sessions,
            &PaletteDynamicItems::default(),
        )
    }

    #[must_use]
    pub fn build_full(
        lang: Language,
        workspaces: &[(String, String)],
        attention_sessions: &[(String, String)],
        dynamic: &PaletteDynamicItems,
    ) -> Self {
        let sh = tr(lang).shell();
        let other_lang = match lang {
            Language::ZhCn => Language::En,
            Language::En => Language::ZhCn,
        };
        let mut items = Vec::with_capacity(96);

        for (session_id, title) in attention_sessions {
            items.push(PaletteItem {
                id: format!("attn:{session_id}"),
                category: PaletteCategory::NeedsYou,
                title: title.clone(),
                subtitle: Some(sh.palette_needs_approval().into()),
                badge_label: Some(sh.palette_badge_gate().into()),
                keywords: format!("approval gate chat {title}"),
                shortcut: None,
                action: PaletteAction::OpenChatSession(session_id.clone()),
            });
        }

        // Quick Wizards
        items.push(PaletteItem {
            id: "wiz:sniff-provider".into(),
            category: PaletteCategory::Wizard,
            title: sh.palette_wiz_sniff_title().into(),
            subtitle: Some(sh.palette_wiz_sniff_sub().into()),
            badge_label: Some("AI".into()),
            keywords: sh.palette_wiz_sniff_kw().into(),
            shortcut: None,
            action: PaletteAction::OpenSniffWizard,
        });
        items.push(PaletteItem {
            id: "wiz:connect-client".into(),
            category: PaletteCategory::Wizard,
            title: sh.palette_wiz_connect_title().into(),
            subtitle: Some("Claude Code / Codex / Cursor / OpenCode".into()),
            badge_label: Some("GW".into()),
            keywords: sh.palette_wiz_connect_kw().into(),
            shortcut: None,
            action: PaletteAction::OpenConnectClientWizard,
        });
        items.push(PaletteItem {
            id: "wiz:gateway".into(),
            category: PaletteCategory::Wizard,
            title: sh.palette_wiz_gateway_title().into(),
            subtitle: Some(sh.palette_wiz_gateway_sub().into()),
            badge_label: Some("GW".into()),
            keywords: sh.palette_wiz_gateway_kw().into(),
            shortcut: None,
            action: PaletteAction::OpenGateway,
        });

        for module in NavModule::ALL {
            let zh_label = module.label(Language::ZhCn);
            let en_label = module.label(Language::En);
            items.push(PaletteItem {
                id: format!("nav:{}", module.id_slug()),
                category: PaletteCategory::Navigation,
                title: module.label(lang).into(),
                subtitle: Some(module.label(other_lang).into()),
                badge_label: None,
                keywords: format!("{zh_label} {en_label}"),
                shortcut: Some(module.shortcut_label().into()),
                action: PaletteAction::Navigate(module),
            });
        }

        // Dynamic Workspace & AI entities
        for (req_id, method, name) in dynamic.requests.iter().take(40) {
            items.push(PaletteItem {
                id: format!("req:{req_id}"),
                category: PaletteCategory::Entity,
                title: name.clone(),
                subtitle: Some(sh.palette_entity_request().into()),
                badge_label: Some(method.clone()),
                keywords: format!("{} {method} {name}", sh.palette_entity_request_kw()),
                shortcut: None,
                action: PaletteAction::OpenRequestById(req_id.clone()),
            });
        }
        for (sid, title) in dynamic.chats.iter().take(20) {
            items.push(PaletteItem {
                id: format!("chat:{sid}"),
                category: PaletteCategory::Entity,
                title: title.clone(),
                subtitle: Some(sh.palette_entity_chat().into()),
                badge_label: Some(sh.palette_badge_chat().into()),
                keywords: format!("{} {title}", sh.palette_entity_chat_kw()),
                shortcut: None,
                action: PaletteAction::OpenChatSession(sid.clone()),
            });
        }
        for (mid, name, running) in dynamic.mocks.iter().take(20) {
            items.push(PaletteItem {
                id: format!("mock:{mid}"),
                category: PaletteCategory::Entity,
                title: name.clone(),
                subtitle: Some(
                    if *running {
                        sh.palette_entity_mock_running()
                    } else {
                        sh.palette_entity_mock()
                    }
                    .into(),
                ),
                badge_label: Some(if *running { "LIVE" } else { "MOCK" }.into()),
                keywords: format!("{} {name}", sh.palette_entity_mock_kw()),
                shortcut: None,
                action: PaletteAction::OpenMockServer(mid.clone()),
            });
        }
        for (fid, name) in dynamic.flows.iter().take(20) {
            items.push(PaletteItem {
                id: format!("flow:{fid}"),
                category: PaletteCategory::Entity,
                title: name.clone(),
                subtitle: Some(sh.palette_entity_flow().into()),
                badge_label: Some(sh.palette_badge_flow().into()),
                keywords: format!("{} {name}", sh.palette_entity_flow_kw()),
                shortcut: None,
                action: PaletteAction::OpenFlow(fid.clone()),
            });
        }
        for (pid, name) in dynamic.providers.iter().take(20) {
            items.push(PaletteItem {
                id: format!("prov:{pid}"),
                category: PaletteCategory::Entity,
                title: name.clone(),
                subtitle: Some(sh.palette_entity_provider().into()),
                badge_label: Some("AI".into()),
                keywords: format!("{} {name}", sh.palette_entity_provider_kw()),
                shortcut: None,
                action: PaletteAction::OpenProvider(pid.clone()),
            });
        }

        for (ws_id, ws_name) in workspaces {
            items.push(PaletteItem {
                id: format!("ws:{ws_id}"),
                category: PaletteCategory::Workspace,
                title: ws_name.clone(),
                subtitle: Some(sh.palette_switch_workspace().into()),
                badge_label: None,
                keywords: format!("{} {ws_name}", sh.palette_workspace_kw()),
                shortcut: None,
                action: PaletteAction::SwitchWorkspace(ws_id.clone()),
            });
        }

        for utility in UtilityDialog::ALL {
            let zh_label = utility.label(Language::ZhCn);
            let en_label = utility.label(Language::En);
            items.push(PaletteItem {
                id: format!("util:{en_label}"),
                category: PaletteCategory::Utility,
                title: utility.label(lang).into(),
                subtitle: Some(sh.palette_utility_sub().into()),
                badge_label: None,
                keywords: format!("{zh_label} {en_label}"),
                shortcut: None,
                action: PaletteAction::OpenUtility(utility),
            });
        }

        // Canonical deep-links for workspace tools previously duplicated inside UtilityPanel.
        let zh_sh = tr(Language::ZhCn).shell();
        let en_sh = tr(Language::En).shell();
        items.push(PaletteItem {
            id: "deep:cookies".into(),
            category: PaletteCategory::Utility,
            title: sh.util_cookies().into(),
            subtitle: Some(sh.palette_deep_cookies_sub().into()),
            badge_label: None,
            keywords: format!(
                "{} {} cookie jar",
                zh_sh.util_cookies(),
                en_sh.util_cookies()
            ),
            shortcut: None,
            action: PaletteAction::OpenEnvironmentResource(EnvironmentResourceTarget::Cookies),
        });
        items.push(PaletteItem {
            id: "deep:certificates".into(),
            category: PaletteCategory::Utility,
            title: sh.util_certificates().into(),
            subtitle: Some(sh.palette_deep_certs_sub().into()),
            badge_label: None,
            keywords: format!(
                "{} {} tls mtls ca cert",
                zh_sh.util_certificates(),
                en_sh.util_certificates()
            ),
            shortcut: None,
            action: PaletteAction::OpenEnvironmentResource(EnvironmentResourceTarget::Certificates),
        });
        items.push(PaletteItem {
            id: "deep:sync".into(),
            category: PaletteCategory::Utility,
            title: sh.util_sync().into(),
            subtitle: Some(sh.palette_deep_sync_sub().into()),
            badge_label: None,
            keywords: format!(
                "{} {} sync git folder",
                zh_sh.util_sync(),
                en_sh.util_sync()
            ),
            shortcut: None,
            action: PaletteAction::OpenSettings(SettingsSection::Data),
        });
        items.push(PaletteItem {
            id: "deep:runner".into(),
            category: PaletteCategory::Utility,
            title: sh.util_runner().into(),
            subtitle: Some(sh.palette_deep_runner_sub().into()),
            badge_label: None,
            keywords: format!(
                "{} {} collection run batch",
                zh_sh.util_runner(),
                en_sh.util_runner()
            ),
            shortcut: None,
            action: PaletteAction::OpenCollectionRunner,
        });

        for section in SettingsSection::ALL {
            let zh_label = section.label(Language::ZhCn);
            let en_label = section.label(Language::En);
            let grp_zh = section.group().label(Language::ZhCn);
            let grp_en = section.group().label(Language::En);
            items.push(PaletteItem {
                id: format!("settings:{en_label}"),
                category: PaletteCategory::Settings,
                title: format!("{} › {}", section.group().label(lang), section.label(lang)),
                subtitle: Some(sh.palette_cat_settings().into()),
                badge_label: None,
                keywords: format!(
                    "{} {grp_zh} {grp_en} {zh_label} {en_label}",
                    sh.palette_settings_kw()
                ),
                shortcut: None,
                action: PaletteAction::OpenSettings(section),
            });
        }

        items.push(PaletteItem {
            id: "cmd:toggle-sidebar".into(),
            category: PaletteCategory::Command,
            title: sh.palette_cmd_toggle_sidebar().into(),
            subtitle: None,
            badge_label: None,
            keywords: sh.palette_cmd_toggle_sidebar_kw().into(),
            shortcut: Some("⌘B".into()),
            action: PaletteAction::ToggleSidebar,
        });
        items.push(PaletteItem {
            id: "cmd:cycle-theme".into(),
            category: PaletteCategory::Command,
            title: sh.palette_cmd_cycle_theme().into(),
            subtitle: None,
            badge_label: None,
            keywords: sh.palette_cmd_cycle_theme_kw().into(),
            shortcut: None,
            action: PaletteAction::CycleThemeMode,
        });
        items.push(PaletteItem {
            id: "cmd:toggle-lang".into(),
            category: PaletteCategory::Command,
            title: sh.palette_cmd_toggle_lang().into(),
            subtitle: None,
            badge_label: None,
            keywords: sh.palette_cmd_toggle_lang_kw().into(),
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
        PaletteCategory::Navigation => 340,
        PaletteCategory::Wizard => 300,
        PaletteCategory::Entity => 260,
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
