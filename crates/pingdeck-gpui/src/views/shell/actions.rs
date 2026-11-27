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
use pingdeck_desktop::state::{NavModule, ToastKind};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::{InputState, apply_language, palette as active_palette, tr};
use crate::views::runs::WorkflowTab;

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
                    apply_language(settings.general.language, cx);
                    this.state.settings = settings;
                    cx.refresh_windows();
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
        let placeholder = tr(cx).shell().workspace_new_placeholder();
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
                        tr(cx).shell().workspace_created(),
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
        let sh = tr(cx).shell();
        let label = if url.trim().is_empty() {
            sh.proxy_toast_system()
        } else if url.trim().eq_ignore_ascii_case(proxy_url::DIRECT) {
            sh.proxy_toast_direct()
        } else {
            sh.proxy_toast_custom()
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
            self.push_toast_auto(
                ToastKind::Warning,
                tr(cx).shell().proxy_toast_enter_url().to_owned(),
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

    /// Opens the shared 90% viewport Workflow Monitor modal on top of the active window.
    pub fn open_workflow_modal(
        &mut self,
        session_id: String,
        run_id: Option<String>,
        initial_tab: Option<WorkflowTab>,
        cx: &mut Context<Self>,
    ) {
        self.workflow_modal.update(cx, |m, cx| {
            m.open_for_session(session_id, run_id, initial_tab, cx);
        });
        cx.notify();
    }

    /// `⌘⇧A` shortcut: toggles the active chat's 90% Workflow Monitor modal when inside `Chat`,
    /// or jumps straight to the standalone global `Run Monitor` module from anywhere else.
    pub fn handle_toggle_run_monitor(&mut self, cx: &mut Context<Self>) {
        if self.workflow_modal.read(cx).is_open() {
            self.workflow_modal.update(cx, |m, cx| m.close(cx));
            cx.notify();
            return;
        }
        if self.state.active_module == NavModule::Chat
            && let Some((sid, rid)) = self.modules.active_chat_workflow_target(cx)
        {
            self.open_workflow_modal(sid, rid, None, cx);
            return;
        }
        self.navigate(NavModule::Runs, cx);
    }
}
