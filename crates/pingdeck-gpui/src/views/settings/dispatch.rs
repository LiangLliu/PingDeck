//! Active settings section page dispatcher for [`SettingsView`].

use gpui_kit::{AnyElement, AppContext as _, Context, IntoElement as _, Window};
use pingdeck_desktop::state::SettingsSection;

use super::{
    SettingsView, about, agent, appearance, catalog, page_advanced, page_data, page_general,
    page_mock, page_network, page_requests, page_routing, page_shortcuts, page_usage,
};

pub(super) fn page_subtitle(section: SettingsSection, zh: bool) -> &'static str {
    match section {
        SettingsSection::General => {
            if zh {
                "管理界面语言、启动恢复行为以及失焦时的桌面提醒策略。"
            } else {
                "Manage interface language, startup session restore, and background notifications."
            }
        }
        SettingsSection::Appearance => {
            if zh {
                "配置明暗模式、8 套护眼 OKLCH 色系、字号密度与界面动效偏好。"
            } else {
                "Configure light/dark mode, 8 eye-care OKLCH palettes, font scale, density, and motion."
            }
        }
        SettingsSection::Shortcuts => {
            if zh {
                "搜索并自定义全部桌面快捷键绑定，自动检测冲突并支持一键恢复。"
            } else {
                "Search and rebind desktop keyboard shortcuts with live conflict detection."
            }
        }
        SettingsSection::Requests => {
            if zh {
                "配置新建 HTTP 请求的传输默认值、编辑器偏好与大响应体阈值。"
            } else {
                "Set default HTTP transport limits, editor preferences, and large-body thresholds."
            }
        }
        SettingsSection::Network => {
            if zh {
                "统一管理全局出站代理、绕过主机列表以及自定义 PEM 根证书包。"
            } else {
                "Configure global outbound proxy, bypass hosts (NO_PROXY), and custom PEM CA bundles."
            }
        }
        SettingsSection::Mock => {
            if zh {
                "设置新建本地 Mock 服务器的起始端口、默认延迟、CORS 与流量日志上限。"
            } else {
                "Configure default port allocation, latency, CORS, and ring-buffer log limits for mock servers."
            }
        }
        SettingsSection::Data => {
            if zh {
                "查看本机 SQLite 存储占用、清理缓存并配置非秘密数据的文件夹同步。"
            } else {
                "Inspect local SQLite storage, clear caches, and manage folder sync for non-secret definitions."
            }
        }
        SettingsSection::Agent => {
            if zh {
                "配置新对话默认模型、思考力度、自动标题、上下文压缩与安全运行上限。"
            } else {
                "Set default chat model, reasoning effort, auto-titling, compaction, and run safety limits."
            }
        }
        SettingsSection::Personalization => {
            if zh {
                "编写所有对话共用的全局个人偏好说明，附加在系统提示词之后。"
            } else {
                "Provide global personal preferences appended after the system prompt for every conversation."
            }
        }
        SettingsSection::Memory => {
            if zh {
                "管理明确授权读取项目指令（AGENTS.md / CLAUDE.md）的本地文件夹信任列表。"
            } else {
                "Manage folders trusted to load project instructions (AGENTS.md / CLAUDE.md) and local skills."
            }
        }
        SettingsSection::Subagents => {
            if zh {
                "配置并发子智能体上限以及按角色分配的专用模型与备用链。"
            } else {
                "Configure sub-agent concurrency limits and per-role model assignments and fallbacks."
            }
        }
        SettingsSection::Tools => {
            if zh {
                "调整内置工具的默认审批策略与联网搜索提供方配置。"
            } else {
                "Adjust default approval policies for built-in tools and configure web search backends."
            }
        }
        SettingsSection::Permissions => {
            if zh {
                "审阅不可绕过的 6 层权限评估顺序、OS 沙箱策略与已记住的作用域规则。"
            } else {
                "Review the 6-layer permission evaluation order, OS sandbox settings, and remembered rules."
            }
        }
        SettingsSection::Mcp => {
            if zh {
                "管理外部 MCP 工具服务器的连接配置、实时状态与工具权限审查。"
            } else {
                "Configure external MCP tool servers, live connection status, and tool reviews."
            }
        }
        SettingsSection::Skills => {
            if zh {
                "扫描、导入与启停本地智能体技能目录（SKILL.md）。"
            } else {
                "Scan, import, and toggle local agent skill directories (SKILL.md)."
            }
        }
        SettingsSection::Routing => {
            if zh {
                "配置模型调用与本地网关共用的重试次数、退避上限与故障转移超时。"
            } else {
                "Configure shared retry counts, Retry-After caps, and failover timeouts for AI calls and Gateway."
            }
        }
        SettingsSection::Catalog => {
            if zh {
                "同步模型上下文窗口与单价目录，或手动维护自定义与未收录模型报价。"
            } else {
                "Sync model specs and pricing catalog, or manage custom and unlisted model rates."
            }
        }
        SettingsSection::Usage => {
            if zh {
                "设置单次调用用量留存周期、服务商余额自动刷新频率与网关抓包策略。"
            } else {
                "Control per-call usage retention, balance polling frequency, and gateway body capture."
            }
        }
        SettingsSection::Advanced => {
            if zh {
                "调整脚本沙箱执行预算、控制台日志上限，或在必要时一键重置全部设置。"
            } else {
                "Tune script sandbox budgets, console log caps, or reset all settings to factory defaults."
            }
        }
        SettingsSection::About => {
            if zh {
                "查看版本、运行环境与脱敏诊断摘要。"
            } else {
                "View application version, runtime environment, and redacted diagnostics."
            }
        }
    }
}

pub(super) fn render_page_body(
    view: &mut SettingsView,
    zh: bool,
    window: &mut Window,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    match view.active {
        SettingsSection::About => view
            .about_view
            .get_or_insert_with(|| cx.new(about::AboutSettingsView::new))
            .clone()
            .into_any_element(),
        SettingsSection::Agent
        | SettingsSection::Personalization
        | SettingsSection::Memory
        | SettingsSection::Subagents
        | SettingsSection::Tools
        | SettingsSection::Permissions
        | SettingsSection::Mcp
        | SettingsSection::Skills => {
            let sec = view.active;
            let ent = if let Some(existing) = &view.agent_view {
                existing.clone()
            } else {
                let created = cx.new(|cx| agent::AgentSettingsView::new(sec, window, cx));
                let sub = cx.observe(&created, |this, ent, cx| {
                    this.settings = ent.read(cx).settings.clone();
                    cx.notify();
                });
                view._subscriptions.push(sub);
                view.agent_view = Some(created.clone());
                created
            };
            if ent.read(cx).active != sec {
                ent.update(cx, |v, cx| v.set_section(sec, cx));
            }
            ent.into_any_element()
        }
        SettingsSection::Catalog => view
            .catalog_view
            .get_or_insert_with(|| cx.new(|cx| catalog::CatalogSettingsView::new(window, cx)))
            .clone()
            .into_any_element(),
        SettingsSection::General => page_general::render_general(view, zh, cx).into_any_element(),
        SettingsSection::Appearance => {
            appearance::render_appearance(view, zh, cx).into_any_element()
        }
        SettingsSection::Shortcuts => {
            page_shortcuts::render_shortcuts(view, zh, cx).into_any_element()
        }
        SettingsSection::Requests => {
            page_requests::render_requests(view, zh, cx).into_any_element()
        }
        SettingsSection::Network => page_network::render_network(view, zh, cx).into_any_element(),
        SettingsSection::Mock => page_mock::render_mock(view, zh, cx).into_any_element(),
        SettingsSection::Data => page_data::render_data(view, zh, cx).into_any_element(),
        SettingsSection::Routing => page_routing::render_routing(view, zh, cx).into_any_element(),
        SettingsSection::Usage => page_usage::render_usage(view, zh, cx).into_any_element(),
        SettingsSection::Advanced => {
            page_advanced::render_advanced(view, zh, cx).into_any_element()
        }
    }
}
