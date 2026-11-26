//! Gateway, Client & Routing-Rule actions for `ProvidersView`.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::{
    llm_client_kept_purge_impl, llm_client_restore_impl, llm_client_restore_original_impl,
    llm_client_rewrite_address_impl, llm_client_rotate_and_update_impl, llm_client_verify_impl,
    llm_gateway_client_create_impl, llm_gateway_client_delete_impl, llm_gateway_client_revoke_impl,
    llm_gateway_client_rotate_impl, llm_gateway_client_set_policy_impl,
    llm_gateway_client_update_impl, llm_gateway_reset_circuits_impl, llm_gateway_restart_impl,
    llm_gateway_start_impl, llm_gateway_stop_impl, settings_update_impl,
};
use pingdeck_desktop::core::client_config::ClientApp;
use pingdeck_desktop::core::llm::store::{ClientInput, ClientLimits, ClientPolicyInput};
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::ProvidersView;

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

    pub fn reset_all_circuits(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(cx, llm_gateway_reset_circuits_impl, move |this, (), cx| {
            this.status_msg = Some((
                Tone::Success,
                crate::ui::tr(cx)
                    .providers()
                    .prov_actions_all_circuits_reset()
                    .into(),
            ));
            this.reload_all(cx);
        });
        self._tasks.push(task);
    }

    pub fn rewrite_all_client_addresses(&mut self, cx: &mut Context<Self>) {
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
                    crate::ui::tr(cx)
                        .providers()
                        .prov_actions_rewrote_address_for_client_s(cnt),
                ));
                this.reload_all(cx);
            },
        );
        self._tasks.push(task);
    }

    pub fn create_gateway_client(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .new_client_name_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        if name.is_empty() {
            self.status_msg = Some((
                Tone::Warning,
                crate::ui::tr(cx)
                    .providers()
                    .prov_actions_enter_a_client_name_first()
                    .into(),
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

    pub fn rotate_gateway_client(&mut self, cid: String, cx: &mut Context<Self>) {
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
                            crate::ui::tr(cx)
                                .providers()
                                .prov_actions_rotated_and_updated_file_s(name, n),
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

    pub fn revoke_gateway_client(&mut self, cid: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_gateway_client_revoke_impl(state, &cid),
            move |this, res, cx| {
                match res {
                    Ok(c) => {
                        this.status_msg = Some((
                            Tone::Warning,
                            crate::ui::tr(cx).providers().prov_actions_revoked(c.name),
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

    pub fn delete_gateway_client(&mut self, cid: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_gateway_client_delete_impl(state, &cid),
            move |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Neutral,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_actions_client_deleted()
                                .into(),
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
            self.client_models_page = 0;
            self.editing_client_policy = Some(cid);
        }
        cx.notify();
    }

    pub fn toggle_client_allow_all(&mut self, cid: String, cx: &mut Context<Self>) {
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
                        crate::ui::tr(cx)
                            .providers()
                            .prov_act_update_failed(err.message()),
                    ));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn toggle_client_allowed_model(
        &mut self,
        cid: String,
        model_ref: String,
        cx: &mut Context<Self>,
    ) {
        let Some(cl) = self.gateway_clients.iter().find(|c| c.id == cid).cloned() else {
            return;
        };
        let mut allowed = cl.allowed_models;
        if let Some(pos) = allowed.iter().position(|m| m == &model_ref) {
            allowed.remove(pos);
        } else {
            allowed.push(model_ref);
        }
        let input = ClientInput {
            name: cl.name,
            allow_all: cl.allow_all,
            allowed_aliases: cl.allowed_aliases,
            allowed_models: allowed,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_gateway_client_update_impl(state, &cid, input),
            move |this, res, cx| match res {
                Ok(_) => this.reload_all(cx),
                Err(err) => {
                    this.status_msg = Some((
                        Tone::Danger,
                        crate::ui::tr(cx)
                            .providers()
                            .prov_act_update_models_failed(err.message()),
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
                        crate::ui::tr(cx)
                            .providers()
                            .prov_act_save_failed(err.message()),
                    ));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn save_client_limits(&mut self, cid: String, cx: &mut Context<Self>) {
        let Some(cl) = self.gateway_clients.iter().find(|c| c.id == cid).cloned() else {
            return;
        };
        let rpm_raw = self.client_rpm_input.read(cx).value().trim().to_owned();
        let conc_raw = self.client_conc_input.read(cx).value().trim().to_owned();
        let mut limits: ClientLimits = cl.limits;
        limits.rpm = if rpm_raw.is_empty() {
            None
        } else {
            match rpm_raw.parse::<u32>() {
                Ok(v) if v > 0 => Some(v),
                _ => {
                    self.status_msg = Some((
                        Tone::Warning,
                        crate::ui::tr(cx)
                            .providers()
                            .prov_actions_rpm_must_be_a_positive()
                            .into(),
                    ));
                    cx.notify();
                    return;
                }
            }
        };
        limits.max_concurrent = if conc_raw.is_empty() {
            None
        } else {
            match conc_raw.parse::<u32>() {
                Ok(v) if v > 0 => Some(v),
                _ => {
                    self.status_msg = Some((
                        Tone::Warning,
                        crate::ui::tr(cx)
                            .providers()
                            .prov_actions_max_concurrency_must_be_a()
                            .into(),
                    ));
                    cx.notify();
                    return;
                }
            }
        };
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
                            crate::ui::tr(cx)
                                .providers()
                                .prov_actions_client_limits_saved()
                                .into(),
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
                            crate::ui::tr(cx)
                                .providers()
                                .prov_act_test_failed(err.message()),
                        ));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn restore_client_tool(&mut self, app: ClientApp, original: bool, cx: &mut Context<Self>) {
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
                            crate::ui::tr(cx)
                                .providers()
                                .prov_actions_restored_file_s_for(
                                    r.files.len(),
                                    app.display_name(),
                                ),
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

    pub fn purge_client_backups(&mut self, app: ClientApp, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_client_kept_purge_impl(state, app),
            move |this, res, cx| {
                match res {
                    Ok(n) => {
                        this.status_msg = Some((
                            Tone::Neutral,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_actions_purged_backup_s(n),
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
}
