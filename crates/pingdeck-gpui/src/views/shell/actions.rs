//! Shell actions for theme/language toggling and inline workspace creation.

use gpui_kit::{AppContext as _, Context, Window};
use pingdeck_desktop::core::app::{settings_update_impl, workspace_create_impl};
use pingdeck_desktop::core::model::settings::{
    Language, SettingsSection as CoreSettingsSection, Theme as SettingsTheme,
};
use pingdeck_desktop::state::ToastKind;

use crate::bridge::spawn_blocking_into;
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
                    this.state
                        .push_toast(ToastKind::Error, err.message().to_owned(), None);
                    cx.notify();
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
                    this.state
                        .push_toast(ToastKind::Error, err.message().to_owned(), None);
                    cx.notify();
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
                    this.state.push_toast(
                        ToastKind::Success,
                        if this.language() == Language::ZhCn {
                            "已创建工作区"
                        } else {
                            "Workspace created"
                        },
                        None,
                    );
                    cx.notify();
                }
                Err(err) => {
                    this.state
                        .push_toast(ToastKind::Error, err.message().to_owned(), None);
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
        cx.notify();
    }
}
