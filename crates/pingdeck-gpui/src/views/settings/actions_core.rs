//! Settings mutations, validation helpers, live proxy/CA checks, list chip mutations,
//! and live keyboard shortcut editing with conflict detection.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::{
    network_ca_test_impl, network_proxy_test_impl, settings_reset_all_impl,
    settings_reset_section_impl, settings_undo_last_reset_impl, settings_update_impl,
};
use pingdeck_desktop::core::model::settings::{Language, Settings, SettingsSection as CoreSection};
use pingdeck_desktop::core::proxy_url;
use pingdeck_desktop::services::{FileFilter, FilePickerSpec, HostDialog as _, RfdDialog};

use crate::app::actions::{SHORTCUT_SPECS, effective_key, normalize_keystroke};
use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::SettingsView;
use super::state_inputs::ProxyModeChoice;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

impl SettingsView {
    pub(crate) fn is_zh(&self) -> bool {
        self.settings.general.language == Language::ZhCn
    }

    pub fn set_flash(&mut self, tone: Tone, message: impl Into<String>, cx: &mut Context<Self>) {
        self.last_save_error = matches!(tone, Tone::Danger);
        self.flash = Some((tone, message.into()));
        cx.notify();
    }

    pub(crate) fn apply_updated_settings(&mut self, next: Settings, cx: &mut Context<Self>) {
        let prev_proxy = self.settings.network.proxy_url.clone();
        self.settings = next.clone();
        if self.settings.network.proxy_url != prev_proxy {
            self.proxy_mode = ProxyModeChoice::from_proxy_url(&self.settings.network.proxy_url);
        }
        self.pending_sync_inputs = true;
        if let Some(ag) = &self.agent_view {
            ag.update(cx, |v, cx| {
                v.sync_from_external_settings(next.clone(), cx);
            });
        }
        if let Some(cat) = &self.catalog_view {
            cat.update(cx, |v, cx| {
                v.sync_from_settings(&next, cx);
            });
        }
    }

    pub fn patch_section(
        &mut self,
        section: CoreSection,
        patch: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let zh = self.is_zh();
        let task = spawn_blocking_into(
            cx,
            move |state| settings_update_impl(state, section, &patch),
            move |this, res, cx| {
                match res {
                    Ok(next) => {
                        this.can_undo_reset_all = false;
                        this.last_save_error = false;
                        this.apply_updated_settings(next, cx);
                    }
                    Err(err) => {
                        this.pending_sync_inputs = true;
                        this.set_flash(
                            Tone::Danger,
                            if zh {
                                format!("保存失败：{}", err.message())
                            } else {
                                format!("Save failed: {}", err.message())
                            },
                            cx,
                        );
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn reset_section(&mut self, section: CoreSection, cx: &mut Context<Self>) {
        self.reset_keys(section, None, cx);
    }

    pub fn reset_keys(
        &mut self,
        section: CoreSection,
        keys: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        let zh = self.is_zh();
        let task = spawn_blocking_into(
            cx,
            move |state| settings_reset_section_impl(state, section, keys),
            move |this, res, cx| match res {
                Ok(next) => {
                    this.can_undo_reset_all = false;
                    this.apply_updated_settings(next, cx);
                    this.set_flash(
                        Tone::Success,
                        if zh {
                            "已恢复该分区默认值"
                        } else {
                            "Restored defaults for this section"
                        },
                        cx,
                    );
                }
                Err(err) => {
                    this.set_flash(Tone::Danger, err.message().to_owned(), cx);
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn reset_all(&mut self, cx: &mut Context<Self>) {
        let zh = self.is_zh();
        let task = spawn_blocking_into(
            cx,
            settings_reset_all_impl,
            move |this, res, cx| match res {
                Ok(next) => {
                    this.can_undo_reset_all = true;
                    this.apply_updated_settings(next, cx);
                    this.set_flash(
                        Tone::Success,
                        if zh {
                            "已恢复全部默认设置（60 秒内可撤销）"
                        } else {
                            "All settings restored to defaults (undo available for 60s)"
                        },
                        cx,
                    );
                }
                Err(err) => {
                    this.set_flash(Tone::Danger, err.message().to_owned(), cx);
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn undo_last_reset(&mut self, cx: &mut Context<Self>) {
        let zh = self.is_zh();
        let task = spawn_blocking_into(cx, settings_undo_last_reset_impl, move |this, res, cx| {
            this.can_undo_reset_all = false;
            match res {
                Ok(next) => {
                    this.apply_updated_settings(next, cx);
                    this.set_flash(
                        Tone::Success,
                        if zh {
                            "已撤销上次全部重置"
                        } else {
                            "Undid last reset-all"
                        },
                        cx,
                    );
                }
                Err(err) => {
                    this.set_flash(Tone::Warning, err.message().to_owned(), cx);
                }
            }
        });
        self._tasks.push(task);
    }

    // --------------------------------------------------------- numeric commits

    pub fn commit_u64(
        &mut self,
        raw: &str,
        min: u64,
        max: u64,
        section: CoreSection,
        patch_fn: impl FnOnce(u64) -> serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let zh = self.is_zh();
        let Ok(val) = raw.trim().parse::<u64>() else {
            self.set_flash(
                Tone::Danger,
                if zh {
                    format!("请输入有效整数（范围 {min}–{max}）")
                } else {
                    format!("Enter a valid integer ({min}–{max})")
                },
                cx,
            );
            return;
        };
        if val < min || val > max {
            self.set_flash(
                Tone::Danger,
                if zh {
                    format!("数值超出允许范围：{min}–{max}")
                } else {
                    format!("Value out of range: must be between {min} and {max}")
                },
                cx,
            );
            return;
        }
        self.patch_section(section, patch_fn(val), cx);
    }

    pub fn commit_kib(
        &mut self,
        raw: &str,
        min_kib: u64,
        max_kib: u64,
        section: CoreSection,
        field: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.commit_u64(
            raw,
            min_kib,
            max_kib,
            section,
            |v| serde_json::json!({ field: v.saturating_mul(KIB) }),
            cx,
        );
    }

    pub fn commit_mib(
        &mut self,
        raw: &str,
        min_mib: u64,
        max_mib: u64,
        section: CoreSection,
        field: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.commit_u64(
            raw,
            min_mib,
            max_mib,
            section,
            |v| serde_json::json!({ field: v.saturating_mul(MIB) }),
            cx,
        );
    }

    // --------------------------------------------------- Network proxy & CA

    pub fn select_proxy_mode(&mut self, mode: ProxyModeChoice, cx: &mut Context<Self>) {
        self.proxy_mode = mode;
        self.proxy_test_result = None;
        match mode {
            ProxyModeChoice::System => {
                self.patch_section(
                    CoreSection::Network,
                    serde_json::json!({ "proxy_url": "" }),
                    cx,
                );
            }
            ProxyModeChoice::Direct => {
                self.patch_section(
                    CoreSection::Network,
                    serde_json::json!({ "proxy_url": proxy_url::DIRECT }),
                    cx,
                );
            }
            ProxyModeChoice::Custom => {
                cx.notify();
            }
        }
    }

    pub fn save_custom_proxy(&mut self, cx: &mut Context<Self>) {
        let raw = self.inputs.proxy_url.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            let zh = self.is_zh();
            self.set_flash(
                Tone::Warning,
                if zh {
                    "请输入代理地址（如 http://127.0.0.1:7890），或切换到「跟随系统 / 直连」"
                } else {
                    "Enter a proxy URL (e.g. http://127.0.0.1:7890), or switch to System / Direct"
                },
                cx,
            );
            return;
        }
        self.proxy_mode = ProxyModeChoice::Custom;
        self.patch_section(
            CoreSection::Network,
            serde_json::json!({ "proxy_url": raw }),
            cx,
        );
    }

    pub fn test_proxy_connection(&mut self, cx: &mut Context<Self>) {
        if self.proxy_testing {
            return;
        }
        let target_url = match self.proxy_mode {
            ProxyModeChoice::System => String::new(),
            ProxyModeChoice::Direct => proxy_url::DIRECT.to_owned(),
            ProxyModeChoice::Custom => self.inputs.proxy_url.read(cx).value().trim().to_owned(),
        };
        if self.proxy_mode == ProxyModeChoice::Custom && target_url.is_empty() {
            let zh = self.is_zh();
            self.set_flash(
                Tone::Warning,
                if zh {
                    "请先输入自定义代理地址再测试连接"
                } else {
                    "Enter a custom proxy URL before testing"
                },
                cx,
            );
            return;
        }
        self.proxy_testing = true;
        self.proxy_test_result = None;
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move { network_proxy_test_impl(&state, &target_url).await },
            |this, res, cx| {
                this.proxy_testing = false;
                match res {
                    Ok(dto) => this.proxy_test_result = Some(dto),
                    Err(err) => this.set_flash(Tone::Danger, err.message().to_owned(), cx),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn add_no_proxy_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.inputs.no_proxy_add.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            return;
        }
        let mut list = self.settings.network.no_proxy.clone();
        for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if !list.iter().any(|x| x.eq_ignore_ascii_case(item)) {
                list.push(item.to_owned());
            }
        }
        self.inputs
            .no_proxy_add
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.patch_section(
            CoreSection::Network,
            serde_json::json!({ "no_proxy": list }),
            cx,
        );
    }

    pub fn remove_no_proxy_entry(&mut self, idx: usize, cx: &mut Context<Self>) {
        let mut list = self.settings.network.no_proxy.clone();
        if idx < list.len() {
            list.remove(idx);
            self.patch_section(
                CoreSection::Network,
                serde_json::json!({ "no_proxy": list }),
                cx,
            );
        }
    }

    pub fn pick_ca_bundle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let zh = self.is_zh();
        let spec = FilePickerSpec {
            title: Some(if zh {
                "选择 PEM 根证书包".to_owned()
            } else {
                "Choose PEM CA bundle".to_owned()
            }),
            filters: vec![FileFilter {
                name: "PEM / CRT".to_owned(),
                extensions: vec!["pem".into(), "crt".into(), "cer".into()],
            }],
            starting_dir: None,
        };
        let Some(path) = RfdDialog.pick_file(&spec) else {
            return;
        };
        let s = path.to_string_lossy().to_string();
        self.inputs
            .ca_bundle_path
            .update(cx, |i, cx| i.set_value(s.clone(), window, cx));
        self.ca_test_result = None;
        self.patch_section(
            CoreSection::Network,
            serde_json::json!({ "ca_bundle_path": s }),
            cx,
        );
        self.test_ca_bundle(cx);
    }

    pub fn save_ca_bundle(&mut self, cx: &mut Context<Self>) {
        let path = self
            .inputs
            .ca_bundle_path
            .read(cx)
            .value()
            .trim()
            .to_owned();
        self.ca_test_result = None;
        self.patch_section(
            CoreSection::Network,
            serde_json::json!({ "ca_bundle_path": path }),
            cx,
        );
    }

    pub fn clear_ca_bundle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs
            .ca_bundle_path
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.ca_test_result = None;
        self.patch_section(
            CoreSection::Network,
            serde_json::json!({ "ca_bundle_path": "" }),
            cx,
        );
    }

    pub fn test_ca_bundle(&mut self, cx: &mut Context<Self>) {
        let path = self
            .inputs
            .ca_bundle_path
            .read(cx)
            .value()
            .trim()
            .to_owned();
        if path.is_empty() {
            let zh = self.is_zh();
            self.set_flash(
                Tone::Warning,
                if zh {
                    "请先输入或选择 CA 证书文件路径"
                } else {
                    "Enter or choose a CA bundle path first"
                },
                cx,
            );
            return;
        }
        self.ca_testing = true;
        self.ca_test_result = None;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |_| network_ca_test_impl(&path),
            |this, res, cx| {
                this.ca_testing = false;
                match res {
                    Ok(info) => this.ca_test_result = Some(info),
                    Err(err) => this.set_flash(Tone::Danger, err.message().to_owned(), cx),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    // --------------------------------------------------- Mock extra hosts

    pub fn add_mock_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.inputs.mock_host_add.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            return;
        }
        let mut list = self.settings.mock.extra_allowed_hosts.clone();
        if !list.iter().any(|h| h.eq_ignore_ascii_case(&raw)) {
            list.push(raw);
        }
        self.inputs
            .mock_host_add
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.patch_section(
            CoreSection::Mock,
            serde_json::json!({ "extra_allowed_hosts": list }),
            cx,
        );
    }

    pub fn remove_mock_host(&mut self, idx: usize, cx: &mut Context<Self>) {
        let mut list = self.settings.mock.extra_allowed_hosts.clone();
        if idx < list.len() {
            list.remove(idx);
            self.patch_section(
                CoreSection::Mock,
                serde_json::json!({ "extra_allowed_hosts": list }),
                cx,
            );
        }
    }

    // --------------------------------------------------- Shortcuts editing

    pub fn start_edit_shortcut(
        &mut self,
        spec_id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = SHORTCUT_SPECS
            .iter()
            .find(|s| s.id == spec_id)
            .map(|s| effective_key(s, &self.settings.shortcuts.overrides))
            .unwrap_or_default();
        self.editing_shortcut = Some(spec_id);
        self.inputs
            .shortcut_key
            .update(cx, |i, cx| i.set_value(current, window, cx));
        cx.notify();
    }

    pub fn cancel_edit_shortcut(&mut self, cx: &mut Context<Self>) {
        self.editing_shortcut = None;
        cx.notify();
    }

    pub fn save_shortcut_override(
        &mut self,
        spec_id: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let zh = self.is_zh();
        let raw = self.inputs.shortcut_key.read(cx).value().to_string();
        let normalized = match normalize_keystroke(&raw, zh, cx) {
            Ok(k) => k,
            Err(msg) => {
                self.set_flash(Tone::Danger, msg, cx);
                return;
            }
        };

        // Detect conflicts against all other commands.
        let overrides = &self.settings.shortcuts.overrides;
        if let Some(clash) = SHORTCUT_SPECS
            .iter()
            .filter(|s| s.id != spec_id)
            .find(|s| effective_key(s, overrides).eq_ignore_ascii_case(&normalized))
        {
            let name = if zh { clash.title_zh } else { clash.title_en };
            self.set_flash(
                Tone::Warning,
                if zh {
                    format!("快捷键「{normalized}」与「{name}」冲突，请更换其他组合")
                } else {
                    format!("Shortcut \"{normalized}\" conflicts with \"{name}\"")
                },
                cx,
            );
            return;
        }

        let default_key = SHORTCUT_SPECS
            .iter()
            .find(|s| s.id == spec_id)
            .map(|s| s.default_key)
            .unwrap_or_default();

        let patch = if normalized == default_key {
            serde_json::json!({ "overrides": { spec_id: serde_json::Value::Null } })
        } else {
            serde_json::json!({ "overrides": { spec_id: normalized } })
        };
        self.editing_shortcut = None;
        self.inputs
            .shortcut_key
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.patch_section(CoreSection::Shortcuts, patch, cx);
    }

    pub fn reset_shortcut_override(&mut self, spec_id: &'static str, cx: &mut Context<Self>) {
        self.editing_shortcut = None;
        self.patch_section(
            CoreSection::Shortcuts,
            serde_json::json!({ "overrides": { spec_id: serde_json::Value::Null } }),
            cx,
        );
    }

    pub fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.editing_shortcut = None;
        self.reset_section(CoreSection::Shortcuts, cx);
    }
}
