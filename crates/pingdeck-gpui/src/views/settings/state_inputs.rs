//! Shared state types, input controllers, bilingual keyword search, and formatting helpers
//! for the refactored Settings workbench.

use gpui_kit::{AppContext as _, Context, Entity, Window};
use pingdeck_desktop::core::app::{DataClearReportDto, SyncFolderKind};
use pingdeck_desktop::core::model::settings::Settings;
use pingdeck_desktop::core::proxy_url;
use pingdeck_desktop::core::sync::SyncPreview;
use pingdeck_desktop::state::SettingsSection;

use crate::ui::{InputState, SliderState};

use super::SettingsView;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// User-facing proxy mode on Settings › Network (`system`, `custom`, `direct`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyModeChoice {
    System,
    Custom,
    Direct,
}

impl ProxyModeChoice {
    #[must_use]
    pub fn from_proxy_url(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            Self::System
        } else if trimmed.eq_ignore_ascii_case(proxy_url::DIRECT) {
            Self::Direct
        } else {
            Self::Custom
        }
    }
}

/// Active confirmation / preview dialog owned by [`SettingsView`].
#[derive(Debug, Clone)]
pub enum SettingsModal {
    ConfirmResetAll,
    ConfirmSyncSecrets {
        kind: SyncFolderKind,
        cloud_name: Option<String>,
        directory: String,
    },
    SyncImportPreview {
        preview: SyncPreview,
        apply_removals: bool,
    },
    ConfirmClearAttachments {
        older_than_days: Option<u32>,
        report: DataClearReportDto,
    },
    ConfirmClearCapturedBodies {
        count: u64,
        bytes: u64,
    },
}

/// Input and slider state controllers for all core & model Settings pages.
pub struct SettingsInputs {
    pub ui_font_slider: Entity<SliderState>,
    pub mono_font_slider: Entity<SliderState>,

    pub shortcut_filter: Entity<InputState>,
    pub shortcut_key: Entity<InputState>,

    pub req_timeout_ms: Entity<InputState>,
    pub req_retry_count: Entity<InputState>,
    pub req_max_redirects: Entity<InputState>,
    pub editor_highlight_kib: Entity<InputState>,
    pub viewer_highlight_kib: Entity<InputState>,
    pub resp_body_cap_mib: Entity<InputState>,
    pub resp_inline_kib: Entity<InputState>,
    pub resp_preview_kib: Entity<InputState>,

    pub proxy_url: Entity<InputState>,
    pub no_proxy_add: Entity<InputState>,
    pub ca_bundle_path: Entity<InputState>,

    pub mock_first_port: Entity<InputState>,
    pub mock_latency_ms: Entity<InputState>,
    pub mock_log_capacity: Entity<InputState>,
    pub mock_host_add: Entity<InputState>,

    pub history_retention: Entity<InputState>,
    pub sync_secrets_confirm: Entity<InputState>,

    pub gw_port: Entity<InputState>,
    pub gw_max_retries: Entity<InputState>,
    pub gw_first_byte_s: Entity<InputState>,
    pub gw_idle_s: Entity<InputState>,
    pub gw_cb_threshold: Entity<InputState>,
    pub gw_cb_window_s: Entity<InputState>,
    pub gw_cb_open_s: Entity<InputState>,
    pub gw_cb_open_max_s: Entity<InputState>,
    pub prov_probe_concurrency: Entity<InputState>,
    pub prov_probe_timeout_s: Entity<InputState>,
    pub prov_test_prompt: Entity<InputState>,
    pub prov_usage_days: Entity<InputState>,
    pub prov_balance_min: Entity<InputState>,

    pub adv_loop_limit: Entity<InputState>,
    pub adv_recursion_limit: Entity<InputState>,
    pub adv_send_requests: Entity<InputState>,
    pub adv_console_entries: Entity<InputState>,
    pub adv_console_msg_kib: Entity<InputState>,
    pub adv_test_results: Entity<InputState>,
}

fn new_input(
    val: String,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<SettingsView>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(val)
            .placeholder(placeholder)
    })
}

fn masked_custom_proxy(raw: &str) -> String {
    match ProxyModeChoice::from_proxy_url(raw) {
        ProxyModeChoice::Custom => proxy_url::mask(raw.trim()),
        ProxyModeChoice::System | ProxyModeChoice::Direct => String::new(),
    }
}

impl SettingsInputs {
    pub fn new(settings: &Settings, window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let ui_font_slider = cx.new(|_| {
            SliderState::new()
                .min(10.0)
                .max(26.0)
                .step(1.0)
                .default_value(f32::from(settings.appearance.ui_font_size.clamp(10, 26)))
        });
        let mono_font_slider = cx.new(|_| {
            SliderState::new()
                .min(10.0)
                .max(24.0)
                .step(1.0)
                .default_value(f32::from(settings.appearance.mono_font_size.clamp(10, 24)))
        });

        let net = &settings.network;
        let ed = &settings.editor;
        let data = &settings.data;
        let mock = &settings.mock;
        let gw = &settings.ai.gateway;
        let prov = &settings.ai.providers;
        let adv = &settings.advanced;

        Self {
            ui_font_slider,
            mono_font_slider,
            shortcut_filter: new_input(
                String::new(),
                "筛选命令或快捷键 / Filter commands…",
                window,
                cx,
            ),
            shortcut_key: new_input(String::new(), "cmd-shift-p / ctrl-k", window, cx),
            req_timeout_ms: new_input(net.timeout_ms.to_string(), "30000", window, cx),
            req_retry_count: new_input(net.retry_count.to_string(), "0", window, cx),
            req_max_redirects: new_input(net.max_redirects.to_string(), "10", window, cx),
            editor_highlight_kib: new_input(
                (ed.editor_highlight_limit_bytes / KIB).to_string(),
                "1024",
                window,
                cx,
            ),
            viewer_highlight_kib: new_input(
                (ed.viewer_highlight_limit_bytes / KIB).to_string(),
                "512",
                window,
                cx,
            ),
            resp_body_cap_mib: new_input(
                (data.response_body_cap_bytes / MIB).max(1).to_string(),
                "10",
                window,
                cx,
            ),
            resp_inline_kib: new_input(
                (data.inline_threshold_bytes / KIB).to_string(),
                "1024",
                window,
                cx,
            ),
            resp_preview_kib: new_input((data.preview_bytes / KIB).to_string(), "64", window, cx),
            proxy_url: new_input(
                masked_custom_proxy(&net.proxy_url),
                "http://127.0.0.1:7890 or socks5h://127.0.0.1:1080",
                window,
                cx,
            ),
            no_proxy_add: new_input(String::new(), "example.internal, *.corp", window, cx),
            ca_bundle_path: new_input(
                net.ca_bundle_path.clone(),
                "/path/to/company-ca.pem",
                window,
                cx,
            ),
            mock_first_port: new_input(mock.first_port.to_string(), "3001", window, cx),
            mock_latency_ms: new_input(mock.default_latency_ms.to_string(), "0", window, cx),
            mock_log_capacity: new_input(mock.log_capacity.to_string(), "200", window, cx),
            mock_host_add: new_input(
                String::new(),
                "host.docker.internal or 10.0.2.2",
                window,
                cx,
            ),
            history_retention: new_input(data.history_retention.to_string(), "2000", window, cx),
            sync_secrets_confirm: new_input(String::new(), "INCLUDE SECRETS", window, cx),
            gw_port: new_input(gw.port.to_string(), "4141", window, cx),
            gw_max_retries: new_input(gw.max_retries.to_string(), "4", window, cx),
            gw_first_byte_s: new_input(gw.first_byte_timeout_s.to_string(), "30", window, cx),
            gw_idle_s: new_input(gw.idle_timeout_s.to_string(), "60", window, cx),
            gw_cb_threshold: new_input(gw.circuit_failure_threshold.to_string(), "3", window, cx),
            gw_cb_window_s: new_input(gw.circuit_window_s.to_string(), "30", window, cx),
            gw_cb_open_s: new_input(gw.circuit_open_s.to_string(), "30", window, cx),
            gw_cb_open_max_s: new_input(gw.circuit_open_max_s.to_string(), "300", window, cx),
            prov_probe_concurrency: new_input(prov.probe_concurrency.to_string(), "3", window, cx),
            prov_probe_timeout_s: new_input(prov.probe_timeout_s.to_string(), "30", window, cx),
            prov_test_prompt: new_input(
                prov.default_test_prompt.clone(),
                "Reply with the single word: pong",
                window,
                cx,
            ),
            prov_usage_days: new_input(prov.usage_retention_days.to_string(), "365", window, cx),
            prov_balance_min: new_input(prov.balance_refresh_min.to_string(), "0", window, cx),
            adv_loop_limit: new_input(
                adv.script_loop_iteration_limit.to_string(),
                "5000000",
                window,
                cx,
            ),
            adv_recursion_limit: new_input(
                adv.script_recursion_limit.to_string(),
                "256",
                window,
                cx,
            ),
            adv_send_requests: new_input(
                adv.script_max_send_requests.to_string(),
                "50",
                window,
                cx,
            ),
            adv_console_entries: new_input(adv.console_max_entries.to_string(), "1000", window, cx),
            adv_console_msg_kib: new_input(
                (u64::from(adv.console_max_message_bytes) / KIB)
                    .max(1)
                    .to_string(),
                "64",
                window,
                cx,
            ),
            adv_test_results: new_input(adv.max_test_results.to_string(), "1000", window, cx),
        }
    }

    /// Re-synchronizes every input & slider controller from a freshly persisted [`Settings`].
    pub fn sync_from_settings(
        &self,
        settings: &Settings,
        window: &mut Window,
        cx: &mut Context<SettingsView>,
    ) {
        let ui_val = f32::from(settings.appearance.ui_font_size.clamp(10, 26));
        self.ui_font_slider
            .update(cx, |s, cx| s.set_value(ui_val, window, cx));
        let mono_val = f32::from(settings.appearance.mono_font_size.clamp(10, 24));
        self.mono_font_slider
            .update(cx, |s, cx| s.set_value(mono_val, window, cx));

        let set = |ent: &Entity<InputState>,
                   v: String,
                   window: &mut Window,
                   cx: &mut Context<SettingsView>| {
            if ent.read(cx).value().as_ref() != v {
                ent.update(cx, |i, cx| i.set_value(v, window, cx));
            }
        };

        let net = &settings.network;
        let ed = &settings.editor;
        let data = &settings.data;
        let mock = &settings.mock;
        let gw = &settings.ai.gateway;
        let prov = &settings.ai.providers;
        let adv = &settings.advanced;

        set(&self.req_timeout_ms, net.timeout_ms.to_string(), window, cx);
        set(
            &self.req_retry_count,
            net.retry_count.to_string(),
            window,
            cx,
        );
        set(
            &self.req_max_redirects,
            net.max_redirects.to_string(),
            window,
            cx,
        );
        set(
            &self.editor_highlight_kib,
            (ed.editor_highlight_limit_bytes / KIB).to_string(),
            window,
            cx,
        );
        set(
            &self.viewer_highlight_kib,
            (ed.viewer_highlight_limit_bytes / KIB).to_string(),
            window,
            cx,
        );
        set(
            &self.resp_body_cap_mib,
            (data.response_body_cap_bytes / MIB).max(1).to_string(),
            window,
            cx,
        );
        set(
            &self.resp_inline_kib,
            (data.inline_threshold_bytes / KIB).to_string(),
            window,
            cx,
        );
        set(
            &self.resp_preview_kib,
            (data.preview_bytes / KIB).to_string(),
            window,
            cx,
        );
        set(
            &self.proxy_url,
            masked_custom_proxy(&net.proxy_url),
            window,
            cx,
        );
        set(&self.ca_bundle_path, net.ca_bundle_path.clone(), window, cx);

        set(
            &self.mock_first_port,
            mock.first_port.to_string(),
            window,
            cx,
        );
        set(
            &self.mock_latency_ms,
            mock.default_latency_ms.to_string(),
            window,
            cx,
        );
        set(
            &self.mock_log_capacity,
            mock.log_capacity.to_string(),
            window,
            cx,
        );

        set(
            &self.history_retention,
            data.history_retention.to_string(),
            window,
            cx,
        );

        set(&self.gw_port, gw.port.to_string(), window, cx);
        set(&self.gw_max_retries, gw.max_retries.to_string(), window, cx);
        set(
            &self.gw_first_byte_s,
            gw.first_byte_timeout_s.to_string(),
            window,
            cx,
        );
        set(&self.gw_idle_s, gw.idle_timeout_s.to_string(), window, cx);
        set(
            &self.gw_cb_threshold,
            gw.circuit_failure_threshold.to_string(),
            window,
            cx,
        );
        set(
            &self.gw_cb_window_s,
            gw.circuit_window_s.to_string(),
            window,
            cx,
        );
        set(
            &self.gw_cb_open_s,
            gw.circuit_open_s.to_string(),
            window,
            cx,
        );
        set(
            &self.gw_cb_open_max_s,
            gw.circuit_open_max_s.to_string(),
            window,
            cx,
        );
        set(
            &self.prov_probe_concurrency,
            prov.probe_concurrency.to_string(),
            window,
            cx,
        );
        set(
            &self.prov_probe_timeout_s,
            prov.probe_timeout_s.to_string(),
            window,
            cx,
        );
        set(
            &self.prov_test_prompt,
            prov.default_test_prompt.clone(),
            window,
            cx,
        );
        set(
            &self.prov_usage_days,
            prov.usage_retention_days.to_string(),
            window,
            cx,
        );
        set(
            &self.prov_balance_min,
            prov.balance_refresh_min.to_string(),
            window,
            cx,
        );

        set(
            &self.adv_loop_limit,
            adv.script_loop_iteration_limit.to_string(),
            window,
            cx,
        );
        set(
            &self.adv_recursion_limit,
            adv.script_recursion_limit.to_string(),
            window,
            cx,
        );
        set(
            &self.adv_send_requests,
            adv.script_max_send_requests.to_string(),
            window,
            cx,
        );
        set(
            &self.adv_console_entries,
            adv.console_max_entries.to_string(),
            window,
            cx,
        );
        set(
            &self.adv_console_msg_kib,
            (u64::from(adv.console_max_message_bytes) / KIB)
                .max(1)
                .to_string(),
            window,
            cx,
        );
        set(
            &self.adv_test_results,
            adv.max_test_results.to_string(),
            window,
            cx,
        );
    }
}

/// Bilingual keyword search covering all 20 Settings sections so users can type setting topics
/// (e.g. "proxy", "代理", "CA", "backup", "熔断", "shortcut") directly in the left nav search.
#[must_use]
pub fn section_matches_query(section: SettingsSection, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let (zl, el) = section.label_pair();
    if zl.to_lowercase().contains(query) || el.to_lowercase().contains(query) {
        return true;
    }
    let keywords: &'static str = match section {
        SettingsSection::General => {
            "language locale startup restore session update notification dock bounce 语言 启动 恢复 标签 通知 跳动 更新"
        }
        SettingsSection::Appearance => {
            "theme dark light palette oklch color font mono density compact motion sidebar rail 主题 深色 浅色 护眼 色板 字体 等宽 密度 动效 侧栏"
        }
        SettingsSection::Shortcuts => {
            "hotkey keybinding keyboard command cmd ctrl 快捷键 键位 绑定 按键"
        }
        SettingsSection::Requests => {
            "timeout redirect tls ssl http2 editor tab word wrap highlight response body preview inline 超时 重定向 证书 编辑器 缩进 换行 高亮 响应体 预览"
        }
        SettingsSection::Network => {
            "proxy socks http no_proxy bypass ca pem bundle certificate mtls 代理 绕过 证书 根证书 直连"
        }
        SettingsSection::Mock => {
            "port latency cors log capacity allowed host docker emulator 端口 延迟 跨域 日志 主机"
        }
        SettingsSection::Data => {
            "storage disk vacuum compact backup database attachment capture history chat retention sync folder git cloud export import secrets 存储 空间 备份 压缩 数据库 附件 捕获 历史 对话 保留 同步 文件夹 导出 导入 秘密"
        }
        SettingsSection::Agent => {
            "model target reasoning effort workspace title compact cost cap tokens iterations subagent 默认 模型 推理 预算 上限 压缩"
        }
        SettingsSection::Personalization => {
            "global instructions system prompt persona 个性 全局 指令 提示词"
        }
        SettingsSection::Memory => {
            "project trust folder instruction claude.md 项目 指令 信任 文件夹"
        }
        SettingsSection::Subagents => "subagent role fast deep vision fallback 子智能体 角色 降级",
        SettingsSection::Tools => {
            "web search tavily brave searxng duckduckgo approval tool 工具 搜索 审批"
        }
        SettingsSection::Permissions => {
            "sandbox shell rule block allow ask env passthrough timeout 权限 沙箱 规则 拦截 环境变量"
        }
        SettingsSection::Mcp => "mcp server stdio sse http oauth timeout 服务器 工具",
        SettingsSection::Skills => "skill directory scan markdown 技能 目录",
        SettingsSection::Routing => {
            "gateway retry failover timeout circuit breaker probe concurrency test prompt 网关 重试 故障转移 熔断 探测 并发"
        }
        SettingsSection::Catalog => "catalog models.dev price pricing refresh 模型 目录 价格 同步",
        SettingsSection::Usage => {
            "usage retention capture body balance refresh log rollup 用量 保留 捕获 余额 日志"
        }
        SettingsSection::Advanced => {
            "script sandbox loop recursion pm.sendrequest console test results reset all factory 脚本 沙箱 循环 递归 控制台 重置"
        }
        SettingsSection::About => {
            "version license diagnostics environment log update export 关于 版本 诊断 许可 日志"
        }
    };
    keywords.to_lowercase().contains(query)
}

/// Formats a byte size into compact human-readable units (`B`, `KiB`, `MiB`, `GiB`).
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}
