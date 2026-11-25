//! Headless Command Palette (`⌘K`) index, fuzzy matcher, and frecency ranker.

use std::collections::BTreeMap;

use pingdeck_core::model::settings::Language;

use super::shell::{NavModule, SettingsSection, UtilityDialog};

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
    pub const fn label(self, zh: bool) -> &'static str {
        match (self, zh) {
            (Self::NeedsYou, true) => "等待处理",
            (Self::NeedsYou, false) => "Needs Attention",
            (Self::Wizard, true) => "快速向导",
            (Self::Wizard, false) => "Quick Wizards",
            (Self::Navigation, true) => "页面导航",
            (Self::Navigation, false) => "Navigation",
            (Self::Entity, true) => "工作区与实体",
            (Self::Entity, false) => "Workspace & Items",
            (Self::Workspace, true) => "切换工作区",
            (Self::Workspace, false) => "Workspaces",
            (Self::Utility, true) => "实用工具",
            (Self::Utility, false) => "Utilities",
            (Self::Settings, true) => "设置",
            (Self::Settings, false) => "Settings",
            (Self::Command, true) => "快捷命令",
            (Self::Command, false) => "Commands",
        }
    }
}

/// Concrete intent dispatched when a palette item is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    Navigate(NavModule),
    OpenSettings(SettingsSection),
    OpenUtility(UtilityDialog),
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
        let zh = lang == Language::ZhCn;
        let mut items = Vec::with_capacity(96);

        for (session_id, title) in attention_sessions {
            items.push(PaletteItem {
                id: format!("attn:{session_id}"),
                category: PaletteCategory::NeedsYou,
                title: title.clone(),
                subtitle: Some(if zh { "等待批准" } else { "Needs approval" }.into()),
                badge_label: Some(if zh { "待批" } else { "GATE" }.into()),
                keywords: format!("approval gate chat {title}"),
                shortcut: None,
                action: PaletteAction::OpenChatSession(session_id.clone()),
            });
        }

        // Quick Wizards
        items.push(PaletteItem {
            id: "wiz:sniff-provider".into(),
            category: PaletteCategory::Wizard,
            title: if zh {
                "智能识别添加 AI 服务商…"
            } else {
                "Add AI Provider (Smart Sniff)…"
            }
            .into(),
            subtitle: Some(
                if zh {
                    "服务商向导"
                } else {
                    "Provider wizard"
                }
                .into(),
            ),
            badge_label: Some("AI".into()),
            keywords: "add provider sniff openai anthropic deepseek ollama 添加 服务商 嗅探".into(),
            shortcut: None,
            action: PaletteAction::OpenSniffWizard,
        });
        items.push(PaletteItem {
            id: "wiz:connect-client".into(),
            category: PaletteCategory::Wizard,
            title: if zh {
                "一键接入外部编程工具…"
            } else {
                "Connect Coding Client…"
            }
            .into(),
            subtitle: Some("Claude Code / Codex / Cursor / OpenCode".into()),
            badge_label: Some("GW".into()),
            keywords: "connect client claude code codex cursor zed gateway 接入 客户端 网关".into(),
            shortcut: None,
            action: PaletteAction::OpenConnectClientWizard,
        });
        items.push(PaletteItem {
            id: "wiz:gateway".into(),
            category: PaletteCategory::Wizard,
            title: if zh {
                "打开本地 AI 网关控制台"
            } else {
                "Open Local AI Gateway Cockpit"
            }
            .into(),
            subtitle: Some(
                if zh {
                    "服务商 › 网关"
                } else {
                    "Providers › Gateway"
                }
                .into(),
            ),
            badge_label: Some("GW".into()),
            keywords: "gateway 4141 proxy 网关 控制台".into(),
            shortcut: None,
            action: PaletteAction::OpenGateway,
        });

        for module in NavModule::ALL {
            let (zh_label, en_label) = module.label_pair();
            items.push(PaletteItem {
                id: format!("nav:{}", module.shortcut_digit()),
                category: PaletteCategory::Navigation,
                title: if zh { zh_label } else { en_label }.into(),
                subtitle: Some(if zh { en_label } else { zh_label }.into()),
                badge_label: None,
                keywords: format!("{zh_label} {en_label}"),
                shortcut: Some(format!("⌘{}", module.shortcut_digit())),
                action: PaletteAction::Navigate(module),
            });
        }

        // Dynamic Workspace & AI entities
        for (req_id, method, name) in dynamic.requests.iter().take(40) {
            items.push(PaletteItem {
                id: format!("req:{req_id}"),
                category: PaletteCategory::Entity,
                title: name.clone(),
                subtitle: Some(
                    if zh {
                        "已保存请求"
                    } else {
                        "Saved request"
                    }
                    .into(),
                ),
                badge_label: Some(method.clone()),
                keywords: format!("request 请求 {method} {name}"),
                shortcut: None,
                action: PaletteAction::OpenRequestById(req_id.clone()),
            });
        }
        for (sid, title) in dynamic.chats.iter().take(20) {
            items.push(PaletteItem {
                id: format!("chat:{sid}"),
                category: PaletteCategory::Entity,
                title: title.clone(),
                subtitle: Some(if zh { "AI 对话" } else { "AI Chat" }.into()),
                badge_label: Some(if zh { "对话" } else { "CHAT" }.into()),
                keywords: format!("chat session 对话 {title}"),
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
                        if zh {
                            "Mock 服务 · 运行中"
                        } else {
                            "Mock · Running"
                        }
                    } else if zh {
                        "Mock 服务"
                    } else {
                        "Mock server"
                    }
                    .into(),
                ),
                badge_label: Some(if *running { "LIVE" } else { "MOCK" }.into()),
                keywords: format!("mock server 模拟服务 {name}"),
                shortcut: None,
                action: PaletteAction::OpenMockServer(mid.clone()),
            });
        }
        for (fid, name) in dynamic.flows.iter().take(20) {
            items.push(PaletteItem {
                id: format!("flow:{fid}"),
                category: PaletteCategory::Entity,
                title: name.clone(),
                subtitle: Some(if zh { "接口流程" } else { "Flow" }.into()),
                badge_label: Some(if zh { "流程" } else { "FLOW" }.into()),
                keywords: format!("flow 流程 {name}"),
                shortcut: None,
                action: PaletteAction::OpenFlow(fid.clone()),
            });
        }
        for (pid, name) in dynamic.providers.iter().take(20) {
            items.push(PaletteItem {
                id: format!("prov:{pid}"),
                category: PaletteCategory::Entity,
                title: name.clone(),
                subtitle: Some(if zh { "AI 服务商" } else { "AI Provider" }.into()),
                badge_label: Some("AI".into()),
                keywords: format!("provider model 服务商 {name}"),
                shortcut: None,
                action: PaletteAction::OpenProvider(pid.clone()),
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
                badge_label: None,
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
                badge_label: None,
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
                badge_label: None,
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
            badge_label: None,
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
            badge_label: None,
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
            badge_label: None,
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
