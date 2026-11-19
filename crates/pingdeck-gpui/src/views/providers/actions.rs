//! Gateway, Client & Routing-Rule actions for `ProvidersView`.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::{
    llm_client_kept_purge_impl, llm_client_restore_impl, llm_client_restore_original_impl,
    llm_client_rewrite_address_impl, llm_client_rotate_and_update_impl, llm_client_verify_impl,
    llm_gateway_client_create_impl, llm_gateway_client_delete_impl, llm_gateway_client_revoke_impl,
    llm_gateway_client_rotate_impl, llm_gateway_client_set_policy_impl,
    llm_gateway_client_update_impl, llm_gateway_reset_circuits_impl, llm_gateway_restart_impl,
    llm_gateway_start_impl, llm_gateway_stop_impl, llm_model_fallback_candidates_impl,
    llm_model_fallbacks_set_impl, llm_route_preview_impl, settings_update_impl,
};
use pingdeck_desktop::core::client_config::ClientApp;
use pingdeck_desktop::core::llm::store::{ClientInput, ClientLimits, ClientPolicyInput, ModelRef};
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::{ProvidersView, RuleEditorState};

impl ProvidersView {
    pub fn gateway_action(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| match action {
                "start" => llm_gateway_start_impl(state).map(|_| "Gateway started".to_owned()),
                "stop" => {
                    let _ = llm_gateway_stop_impl(state);
                    Ok("Gateway stopped".to_owned())
                }
                _ => llm_gateway_restart_impl(state).map(|_| "Gateway restarted".to_owned()),
            },
            |this, res, cx| {
                match res {
                    Ok(msg) => this.status_msg = Some((Tone::Success, msg)),
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                this.reload_all(cx);
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn patch_gateway_setting(
        &mut self,
        gw_patch: serde_json::Value,
        restart_if_running: bool,
        cx: &mut Context<Self>,
    ) {
        let patch = serde_json::json!({ "gateway": gw_patch });
        let running = self.overview.as_ref().is_some_and(|o| o.status.running);
        let task = spawn_blocking_into(
            cx,
            move |state| {
                settings_update_impl(state, CoreSection::Ai, &patch)?;
                if restart_if_running && running {
                    let _ = llm_gateway_restart_impl(state);
                }
                Ok::<(), pingdeck_desktop::core::error::AppError>(())
            },
            |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_msg = Some((Tone::Success, "Gateway settings saved".into()));
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                this.reload_all(cx);
            },
        );
        self._tasks.push(task);
    }

    pub fn reset_all_circuits(&mut self, zh: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(cx, llm_gateway_reset_circuits_impl, move |this, (), cx| {
            this.status_msg = Some((
                Tone::Success,
                if zh {
                    "已重置全部熔断器".into()
                } else {
                    "All circuits reset".into()
                },
            ));
            this.reload_all(cx);
        });
        self._tasks.push(task);
    }

    pub fn rewrite_all_client_addresses(&mut self, zh: bool, cx: &mut Context<Self>) {
        let apps: Vec<ClientApp> = self.managed_tools.iter().map(|t| t.app).collect();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                apps.into_iter()
                    .filter(|app| llm_client_rewrite_address_impl(state, *app).is_ok())
                    .count()
            },
            move |this, cnt, cx| {
                this.status_msg = Some((
                    Tone::Success,
                    if zh {
                        format!("已同步 {cnt} 个客户端的网关地址")
                    } else {
                        format!("Rewrote address for {cnt} client(s)")
                    },
                ));
                this.reload_all(cx);
            },
        );
        self._tasks.push(task);
    }

    pub fn create_gateway_client(&mut self, zh: bool, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .new_client_name_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        if name.is_empty() {
            self.status_msg = Some((
                Tone::Warning,
                if zh {
                    "请先输入客户端名称".into()
                } else {
                    "Enter a client name first".into()
                },
            ));
            cx.notify();
            return;
        }
        self.new_client_name_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        let input = ClientInput {
            name,
            allow_all: true,
            allowed_aliases: Vec::new(),
            allowed_models: Vec::new(),
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_gateway_client_create_impl(state, input),
            move |this, res, cx| {
                match res {
                    Ok(created) => {
                        this.revealed_client_token = Some((created.client.name, created.token));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn rotate_gateway_client(&mut self, cid: String, zh: bool, cx: &mut Context<Self>) {
        let has_managed = self
            .managed_tools
            .iter()
            .any(|t| t.files.iter().any(|f| f.client_id == cid));
        let task = spawn_blocking_into(
            cx,
            move |state| {
                if has_managed {
                    llm_client_rotate_and_update_impl(state, &cid)
                        .map(|r| (r.client.name, None, r.files.updated.len()))
                } else {
                    llm_gateway_client_rotate_impl(state, &cid)
                        .map(|r| (r.client.name, Some(r.token), 0))
                }
            },
            move |this, res, cx| {
                match res {
                    Ok((name, Some(tok), _)) => {
                        this.revealed_client_token = Some((name, tok));
                        this.reload_all(cx);
                    }
                    Ok((name, None, n)) => {
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                format!("已为「{name}」轮换令牌并同步写入 {n} 个配置文件")
                            } else {
                                format!("Rotated \"{name}\" and updated {n} file(s)")
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn revoke_gateway_client(&mut self, cid: String, zh: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_gateway_client_revoke_impl(state, &cid),
            move |this, res, cx| {
                match res {
                    Ok(c) => {
                        this.status_msg = Some((
                            Tone::Warning,
                            if zh {
                                format!("已吊销「{}」", c.name)
                            } else {
                                format!("Revoked {}", c.name)
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_gateway_client(&mut self, cid: String, zh: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_gateway_client_delete_impl(state, &cid),
            move |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Neutral,
                            if zh {
                                "已删除客户端".into()
                            } else {
                                "Client deleted".into()
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn toggle_edit_client_policy(
        &mut self,
        cid: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing_client_policy.as_deref() == Some(cid.as_str()) {
            self.editing_client_policy = None;
        } else {
            if let Some(cl) = self.gateway_clients.iter().find(|c| c.id == cid) {
                let rpm = cl.limits.rpm.map_or_else(String::new, |v| v.to_string());
                let conc = cl
                    .limits
                    .max_concurrent
                    .map_or_else(String::new, |v| v.to_string());
                self.client_rpm_input
                    .update(cx, |i, cx| i.set_value(rpm, window, cx));
                self.client_conc_input
                    .update(cx, |i, cx| i.set_value(conc, window, cx));
            }
            self.editing_client_policy = Some(cid);
        }
        cx.notify();
    }

    pub fn toggle_client_allow_all(&mut self, cid: String, zh: bool, cx: &mut Context<Self>) {
        let Some(cl) = self.gateway_clients.iter().find(|c| c.id == cid).cloned() else {
            return;
        };
        let input = ClientInput {
            name: cl.name,
            allow_all: !cl.allow_all,
            allowed_aliases: cl.allowed_aliases,
            allowed_models: cl.allowed_models,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_gateway_client_update_impl(state, &cid, input),
            move |this, res, cx| match res {
                Ok(_) => this.reload_all(cx),
                Err(err) => {
                    this.status_msg = Some((
                        Tone::Danger,
                        if zh {
                            format!("更新失败：{}", err.message())
                        } else {
                            err.message().to_owned()
                        },
                    ));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn toggle_client_unknown_default(
        &mut self,
        cid: String,
        next: bool,
        zh: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(cl) = self.gateway_clients.iter().find(|c| c.id == cid).cloned() else {
            return;
        };
        let policy = ClientPolicyInput {
            limits: cl.limits,
            unknown_model_to_default: Some(next),
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_gateway_client_set_policy_impl(state, &cid, policy),
            move |this, res, cx| match res {
                Ok(_) => this.reload_all(cx),
                Err(err) => {
                    this.status_msg = Some((
                        Tone::Danger,
                        if zh {
                            format!("保存失败：{}", err.message())
                        } else {
                            err.message().to_owned()
                        },
                    ));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn save_client_limits(&mut self, cid: String, zh: bool, cx: &mut Context<Self>) {
        let Some(cl) = self.gateway_clients.iter().find(|c| c.id == cid).cloned() else {
            return;
        };
        let rpm_raw = self.client_rpm_input.read(cx).value().trim().to_owned();
        let conc_raw = self.client_conc_input.read(cx).value().trim().to_owned();
        let mut limits: ClientLimits = cl.limits;
        limits.rpm = (!rpm_raw.is_empty()).then(|| rpm_raw.parse().unwrap_or(0));
        limits.max_concurrent = (!conc_raw.is_empty()).then(|| conc_raw.parse().unwrap_or(0));
        let policy = ClientPolicyInput {
            limits,
            unknown_model_to_default: None,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_gateway_client_set_policy_impl(state, &cid, policy),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                "客户端限额已保存".into()
                            } else {
                                "Client limits saved".into()
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn verify_client_tool(
        &mut self,
        client_id: String,
        app: ClientApp,
        zh: bool,
        cx: &mut Context<Self>,
    ) {
        let task = spawn_async_into(
            cx,
            move |state| async move { llm_client_verify_impl(&state, &client_id, app).await },
            move |this, res, cx| {
                match res {
                    Ok(v) => {
                        this.client_verify_results.insert(app, v);
                    }
                    Err(err) => {
                        this.status_msg = Some((
                            Tone::Danger,
                            if zh {
                                format!("测试失败：{}", err.message())
                            } else {
                                err.message().to_owned()
                            },
                        ));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn restore_client_tool(
        &mut self,
        app: ClientApp,
        original: bool,
        zh: bool,
        cx: &mut Context<Self>,
    ) {
        let task = spawn_blocking_into(
            cx,
            move |state| {
                if original {
                    llm_client_restore_original_impl(state, app, true)
                } else {
                    llm_client_restore_impl(state, app, true)
                }
            },
            move |this, res, cx| {
                match res {
                    Ok(r) => {
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                format!("已恢复 {} 的 {} 个文件", app.display_name(), r.files.len())
                            } else {
                                format!(
                                    "Restored {} file(s) for {}",
                                    r.files.len(),
                                    app.display_name()
                                )
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn purge_client_backups(&mut self, app: ClientApp, zh: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_client_kept_purge_impl(state, app),
            move |this, res, cx| {
                match res {
                    Ok(n) => {
                        this.status_msg = Some((
                            Tone::Neutral,
                            if zh {
                                format!("已清理 {n} 份旧备份")
                            } else {
                                format!("Purged {n} backup(s)")
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn start_edit_rule(
        &mut self,
        primary: ModelRef,
        chain: Vec<ModelRef>,
        cx: &mut Context<Self>,
    ) {
        let pid = primary.provider_id.clone();
        let mid = primary.model.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_model_fallback_candidates_impl(state, &pid, &mid).unwrap_or_default(),
            move |this, candidates, cx| {
                this.rule_editor = Some(RuleEditorState {
                    primary,
                    chain,
                    candidates,
                });
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn save_rule_editor(&mut self, zh: bool, cx: &mut Context<Self>) {
        let Some(ed) = self.rule_editor.take() else {
            return;
        };
        let pid = ed.primary.provider_id;
        let mid = ed.primary.model;
        let chain = ed.chain;
        let task = spawn_blocking_into(
            cx,
            move |state| llm_model_fallbacks_set_impl(state, &pid, &mid, chain),
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        let msg = if saved.warnings.is_empty() {
                            if zh {
                                "备用规则已保存".into()
                            } else {
                                "Fallback rule saved".into()
                            }
                        } else if zh {
                            format!("已保存（{} 项能力差异提醒）", saved.warnings.len())
                        } else {
                            format!("Saved ({} capability warning(s))", saved.warnings.len())
                        };
                        this.status_msg = Some((Tone::Success, msg));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_gateway_rule(&mut self, primary: ModelRef, zh: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| {
                llm_model_fallbacks_set_impl(
                    state,
                    &primary.provider_id,
                    &primary.model,
                    Vec::new(),
                )
            },
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.status_msg = Some((
                            Tone::Neutral,
                            if zh {
                                "备用规则已删除".into()
                            } else {
                                "Fallback rule deleted".into()
                            },
                        ));
                        this.reload_all(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn load_route_preview(
        &mut self,
        requested: String,
        client_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_route_preview_impl(state, &requested, client_id.as_deref()),
            |this, res, cx| {
                match res {
                    Ok(prev) => this.route_preview = Some(prev),
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}
