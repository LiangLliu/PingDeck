//! Provider-scoped actions for `ProvidersView`:
//! Provider toggle/review/key-policy, API Keys CRUD/reveal/cooldown, Balance, and Circuits.
//! Model refresh/drift/bulk toggle/undo/add/delete live in `actions_models.rs`.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::{
    SaveProviderKeyInput, llm_health_reset_impl, llm_key_clear_cooldown_impl, llm_key_delete_impl,
    llm_key_reveal_impl, llm_key_save_guarded_impl, llm_provider_balance_impl,
    llm_provider_mark_reviewed_impl, llm_provider_save_impl,
};
use pingdeck_desktop::core::llm::providers::KeyStrategy;
use pingdeck_desktop::core::llm::store::ProviderKey;

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::ProvidersView;

impl ProvidersView {
    pub fn toggle_provider_enabled(
        &mut self,
        pid: String,
        enabled: bool,
        zh: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(mut prov) = self
            .providers
            .iter()
            .find(|p| p.provider.id == pid)
            .map(|p| p.provider.clone())
        else {
            return;
        };
        prov.enabled = enabled;
        let task = spawn_blocking_into(
            cx,
            move |state| llm_provider_save_impl(state, prov),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                "服务商状态已更新".into()
                            } else {
                                "Provider updated".into()
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

    pub fn update_provider_key_policy(
        &mut self,
        pid: String,
        strategy: Option<KeyStrategy>,
        cooldown: Option<bool>,
        zh: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(mut prov) = self
            .providers
            .iter()
            .find(|p| p.provider.id == pid)
            .map(|p| p.provider.clone())
        else {
            return;
        };
        if let Some(s) = strategy {
            prov.key_strategy = Some(s);
        }
        if let Some(c) = cooldown {
            prov.key_cooldown = Some(c);
        }
        let task = spawn_blocking_into(
            cx,
            move |state| llm_provider_save_impl(state, prov),
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

    pub fn mark_provider_reviewed(&mut self, pid: String, zh: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_provider_mark_reviewed_impl(state, &pid),
            move |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                "已标记为已复核".into()
                            } else {
                                "Marked reviewed".into()
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

    pub fn add_provider_key(
        &mut self,
        pid: String,
        zh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let secret = self.new_key_secret_input.read(cx).value().trim().to_owned();
        if secret.is_empty() {
            self.status_msg = Some((
                Tone::Warning,
                if zh {
                    "请输入 API 密钥".into()
                } else {
                    "Enter an API key".into()
                },
            ));
            cx.notify();
            return;
        }
        let raw_name = self.new_key_name_input.read(cx).value().trim().to_owned();
        let count = self.keys.iter().filter(|k| k.provider_id == pid).count();
        let name = if raw_name.is_empty() {
            if zh {
                format!("密钥 {}", count + 1)
            } else {
                format!("Key {}", count + 1)
            }
        } else {
            raw_name
        };
        let group = self.new_key_group_input.read(cx).value().trim().to_owned();
        let uid = self
            .new_key_user_id_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        self.new_key_name_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.new_key_secret_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.new_key_group_input
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.new_key_user_id_input
            .update(cx, |i, cx| i.set_value("", window, cx));

        let input = SaveProviderKeyInput {
            id: String::new(),
            provider_id: pid,
            name,
            secret: Some(secret),
            enabled: true,
            position: count as i64,
            api_user_id: (!uid.is_empty()).then_some(uid),
            management_token: None,
            notes: None,
            group_name: (!group.is_empty()).then_some(group),
            protocols: None,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_key_save_guarded_impl(state, input, None),
            move |this, res, cx| {
                match res {
                    Ok(k) => {
                        this.adding_key_open = false;
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                format!("已添加密钥「{}」", k.name)
                            } else {
                                format!("Added key {}", k.name)
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

    pub fn toggle_key_enabled(&mut self, key: ProviderKey, zh: bool, cx: &mut Context<Self>) {
        let input = SaveProviderKeyInput {
            id: key.id,
            provider_id: key.provider_id,
            name: key.name,
            secret: None,
            enabled: !key.enabled,
            position: key.position,
            api_user_id: None,
            management_token: None,
            notes: None,
            group_name: None,
            protocols: None,
        };
        let ver = Some(key.updated_at_ms);
        let task = spawn_blocking_into(
            cx,
            move |state| llm_key_save_guarded_impl(state, input, ver),
            move |this, res, cx| match res {
                Ok(_) => this.reload_all(cx),
                Err(err) => {
                    this.status_msg = Some((
                        Tone::Danger,
                        if zh {
                            format!("切换失败：{}", err.message())
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

    pub fn toggle_reveal_key(&mut self, key_id: String, zh: bool, cx: &mut Context<Self>) {
        if self
            .revealed_provider_key
            .as_ref()
            .is_some_and(|(id, _)| id == &key_id)
        {
            self.revealed_provider_key = None;
            cx.notify();
            return;
        }
        let kid = key_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_key_reveal_impl(state, &kid),
            move |this, res, cx| {
                match res {
                    Ok(secret) => this.revealed_provider_key = Some((key_id, secret)),
                    Err(err) => {
                        this.status_msg = Some((
                            Tone::Danger,
                            if zh {
                                format!("无法读取密钥：{}", err.message())
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

    pub fn clear_key_cooldown(&mut self, key_id: String, zh: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| {
                llm_key_clear_cooldown_impl(state, &key_id);
            },
            move |this, (), cx| {
                this.status_msg = Some((
                    Tone::Success,
                    if zh {
                        "已清除冷却".into()
                    } else {
                        "Cooldown cleared".into()
                    },
                ));
                this.reload_all(cx);
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_provider_key(&mut self, key_id: String, zh: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| llm_key_delete_impl(state, &key_id),
            move |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Neutral,
                            if zh {
                                "已删除密钥".into()
                            } else {
                                "Key deleted".into()
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

    pub fn refresh_provider_balance(&mut self, pid: String, zh: bool, cx: &mut Context<Self>) {
        self.refreshing_balance = true;
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move { llm_provider_balance_impl(&state, &pid, None, true, None).await },
            move |this, res, cx| {
                this.refreshing_balance = false;
                match res {
                    Ok(_) => {
                        this.status_msg = Some((
                            Tone::Success,
                            if zh {
                                "余额已刷新".into()
                            } else {
                                "Balance refreshed".into()
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

    pub fn reset_provider_circuits(&mut self, pid: String, zh: bool, cx: &mut Context<Self>) {
        let key_ids: Vec<String> = self
            .keys
            .iter()
            .filter(|k| k.provider_id == pid)
            .map(|k| k.id.clone())
            .collect();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                for kid in key_ids {
                    llm_health_reset_impl(state, Some(&kid));
                }
            },
            move |this, (), cx| {
                this.status_msg = Some((
                    Tone::Success,
                    if zh {
                        "已重置该服务商熔断器".into()
                    } else {
                        "Provider circuits reset".into()
                    },
                ));
                this.reload_all(cx);
            },
        );
        self._tasks.push(task);
    }
}
