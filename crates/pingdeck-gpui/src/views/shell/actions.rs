//! Shell actions for theme/language toggling, global outbound proxy control, and inline
//! workspace creation.

use gpui_kit::{AppContext as _, Context, Window};
use pingdeck_desktop::core::app::{
    network_proxy_test_impl, settings_update_impl, workspace_create_impl,
};
use pingdeck_desktop::core::model::settings::{
    Language, SettingsSection as CoreSettingsSection, Theme as SettingsTheme,
};
use pingdeck_desktop::core::proxy_url;
use pingdeck_desktop::state::ToastKind;

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::{InputState, palette as active_palette};

use super::AppShell;

impl AppShell {
    pub fn toggle_theme_mode(&mut self, cx: &mut Context<Self>) {
        let next = if active_palette(cx).is_dark {
            SettingsTheme::Light
        } else {
            SettingsTheme::Dark
        };
        let patch = serde_json::json!({
            "theme": match next {
                SettingsTheme::Light => "light",
                SettingsTheme::Dark => "dark",
                SettingsTheme::System => "system",
            }
        });
        let task = spawn_blocking_into(
            cx,
            move |state| settings_update_impl(state, CoreSettingsSection::Appearance, &patch),
            |this, res, cx| match res {
                Ok(settings) => {
                    this.state.settings = settings;
                    this.reapply_theme(cx);
                    cx.notify();
                }
                Err(err) => {
                    this.push_toast_auto(ToastKind::Error, err.message().to_owned(), None, cx);
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn toggle_language(&mut self, cx: &mut Context<Self>) {
        let next = match self.language() {
            Language::ZhCn => "en",
            Language::En => "zh-CN",
        };
        let patch = serde_json::json!({ "language": next });
        let task = spawn_blocking_into(
            cx,
            move |state| settings_update_impl(state, CoreSettingsSection::General, &patch),
            |this, res, cx| match res {
                Ok(settings) => {
                    this.state.settings = settings;
                    cx.notify();
                }
                Err(err) => {
                    this.push_toast_auto(ToastKind::Error, err.message().to_owned(), None, cx);
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn start_create_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = if self.language() == Language::ZhCn {
            "新工作区名称…"
        } else {
            "New workspace name…"
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        self.new_workspace_input = Some(input);
        self.workspace_menu_open = true;
        cx.notify();
    }

    pub fn submit_create_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.new_workspace_input.take() else {
            return;
        };
        let name = input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            cx.notify();
            return;
        }
        let id = format!("ws-{}", pingdeck_desktop::core::app::current_time_ms());
        let task = spawn_blocking_into(
            cx,
            move |state| workspace_create_impl(state, id, name),
            |this, res, cx| match res {
                Ok(ws) => {
                    let ws_id = ws.id.as_str().to_owned();
                    this.state.workspaces.insert(0, ws);
                    this.state.active_workspace_id = Some(ws_id.clone());
                    this.modules.set_workspace(Some(ws_id), cx);
                    this.workspace_menu_open = false;
                    this.persist_local(cx);
                    this.push_toast_auto(
                        ToastKind::Success,
                        if this.language() == Language::ZhCn {
                            "已创建工作区"
                        } else {
                            "Workspace created"
                        },
                        None,
                        cx,
                    );
                }
                Err(err) => {
                    this.push_toast_auto(ToastKind::Error, err.message().to_owned(), None, cx);
                }
            },
        );
        self._tasks.push(task);
        cx.notify();
    }

    pub fn set_global_proxy_url(&mut self, url: String, cx: &mut Context<Self>) {
        self.proxy_test_result = None;
        let zh = self.language() == Language::ZhCn;
        let label = if url.trim().is_empty() {
            if zh {
                "已切换为跟随系统代理"
            } else {
                "Switched to system proxy"
            }
        } else if url.trim().eq_ignore_ascii_case(proxy_url::DIRECT) {
            if zh {
                "已切换为直连（无代理）"
            } else {
                "Switched to direct connection"
            }
        } else if zh {
            "已应用全局自定义代理"
        } else {
            "Applied custom global proxy"
        };
        let patch = serde_json::json!({ "proxy_url": url });
        let task = spawn_blocking_into(
            cx,
            move |state| settings_update_impl(state, CoreSettingsSection::Network, &patch),
            move |this, res, cx| match res {
                Ok(settings) => {
                    this.state.settings = settings;
                    this.push_toast_auto(ToastKind::Success, label.to_owned(), None, cx);
                    cx.notify();
                }
                Err(err) => {
                    this.push_toast_auto(ToastKind::Error, err.message().to_owned(), None, cx);
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn apply_custom_global_proxy(&mut self, cx: &mut Context<Self>) {
        let raw = self.proxy_url_input.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            let zh = self.language() == Language::ZhCn;
            self.push_toast_auto(
                ToastKind::Warning,
                if zh {
                    "请输入代理地址（如 http://127.0.0.1:7890）"
                } else {
                    "Enter a proxy URL (e.g. http://127.0.0.1:7890)"
                }
                .to_owned(),
                None,
                cx,
            );
            return;
        }
        self.set_global_proxy_url(raw, cx);
    }

    pub fn test_global_proxy(&mut self, cx: &mut Context<Self>) {
        if self.proxy_testing {
            return;
        }
        let target_url = self.state.settings.network.proxy_url.clone();
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
                    Err(err) => {
                        this.push_toast_auto(ToastKind::Error, err.message().to_owned(), None, cx);
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}
