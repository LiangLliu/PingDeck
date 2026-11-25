//! Settings patch, numeric commit, fallback/role, and search actions for `AgentSettingsView`.

use gpui_kit::{Context, PathPromptOptions, SharedString, Window};
use pingdeck_desktop::core::agent::model::SubagentRole;
use pingdeck_desktop::core::app::{
    SettingsSecret, ai_search_test_impl, settings_reset_section_impl, settings_secret_set_impl,
    settings_update_impl,
};
use pingdeck_desktop::core::model::settings::{SearchBackend, SettingsSection as CoreSection};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::AgentSettingsView;

impl AgentSettingsView {
    /// Patches the `ai` settings section on Tokio's blocking pool and reloads derived views.
    pub(super) fn patch_ai(
        &mut self,
        patch: serde_json::Value,
        flash_ok: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let task = spawn_blocking_into(
            cx,
            move |state| settings_update_impl(state, CoreSection::Ai, &patch),
            move |this, res, cx| match res {
                Ok(next) => {
                    this.settings = next;
                    if let Some(msg) = flash_ok {
                        this.flash = Some((Tone::Success, msg));
                    } else {
                        this.flash = None;
                    }
                    this.reload_all(cx);
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    /// Resets specific dotted keys under `ai` back to factory defaults and syncs inputs.
    pub(super) fn reset_ai_keys(
        &mut self,
        keys: &[&str],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key_vec: Vec<String> = keys.iter().map(|k| (*k).to_owned()).collect();
        let win_handle = window.window_handle();
        let task = spawn_blocking_into(
            cx,
            move |state| settings_reset_section_impl(state, CoreSection::Ai, Some(key_vec)),
            move |this, res, cx| match res {
                Ok(next) => {
                    this.settings = next;
                    this.flash = Some((
                        Tone::Success,
                        crate::ui::tr(cx)
                            .settings()
                            .actions_restored_section_defaults()
                            .to_owned(),
                    ));
                    let _ = win_handle.update(cx, |_, window, cx| {
                        this.sync_inputs_from_settings(window, cx);
                    });
                    this.reload_all(cx);
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn commit_compact_pct(&mut self, cx: &mut Context<Self>) {
        self.commit_u64_field(
            &self.compact_pct_input.clone(),
            "compact_threshold_pct",
            50,
            95,
            cx,
        );
    }

    pub(super) fn commit_keep_turns(&mut self, cx: &mut Context<Self>) {
        self.commit_u64_field(
            &self.keep_turns_input.clone(),
            "keep_recent_turns",
            1,
            20,
            cx,
        );
    }

    pub(super) fn commit_max_steps(&mut self, cx: &mut Context<Self>) {
        self.commit_u64_field(
            &self.max_steps_input.clone(),
            "max_tool_iterations",
            1,
            100,
            cx,
        );
    }

    pub(super) fn commit_max_runs(&mut self, cx: &mut Context<Self>) {
        self.commit_u64_field(
            &self.max_runs_input.clone(),
            "max_concurrent_runs",
            1,
            8,
            cx,
        );
    }

    pub(super) fn commit_max_image(&mut self, cx: &mut Context<Self>) {
        self.commit_u64_field(&self.max_image_input.clone(), "max_image_mb", 1, 20, cx);
    }

    pub(super) fn commit_max_file(&mut self, cx: &mut Context<Self>) {
        self.commit_u64_field(&self.max_file_input.clone(), "max_file_mb", 1, 100, cx);
    }

    pub(super) fn commit_max_edge(&mut self, cx: &mut Context<Self>) {
        self.commit_u64_field(
            &self.max_edge_input.clone(),
            "image_max_edge_px",
            512,
            2048,
            cx,
        );
    }

    pub(super) fn commit_max_subagents(&mut self, cx: &mut Context<Self>) {
        self.commit_u64_field(&self.max_subagents_input.clone(), "max_subagents", 1, 8, cx);
    }

    pub(super) fn commit_approval_timeout(&mut self, cx: &mut Context<Self>) {
        self.commit_u64_field(
            &self.approval_timeout_input.clone(),
            "approval_timeout_s",
            60,
            3600,
            cx,
        );
    }

    pub(super) fn commit_shell_timeout(&mut self, cx: &mut Context<Self>) {
        self.commit_u64_field(
            &self.shell_timeout_input.clone(),
            "shell_timeout_s",
            1,
            3600,
            cx,
        );
    }

    fn commit_u64_field(
        &mut self,
        input: &gpui_kit::Entity<crate::ui::InputState>,
        key: &'static str,
        min: u64,
        max: u64,
        cx: &mut Context<Self>,
    ) {
        let raw = input.read(cx).value().trim().to_owned();
        let Ok(val) = raw.parse::<u64>() else {
            self.flash = Some((
                Tone::Danger,
                crate::ui::tr(cx)
                    .settings()
                    .actions_enter_a_whole_number_between(min, max),
            ));
            cx.notify();
            return;
        };
        if val < min || val > max {
            self.flash = Some((
                Tone::Danger,
                crate::ui::tr(cx)
                    .settings()
                    .actions_value_must_be_between_and(min, max),
            ));
            cx.notify();
            return;
        }
        self.patch_ai(serde_json::json!({ "agent": { key: val } }), None, cx);
    }

    pub(super) fn commit_cost_usd(&mut self, cx: &mut Context<Self>) {
        self.commit_opt_f64(&self.cost_usd_input.clone(), "cost_cap_usd", cx);
    }

    pub(super) fn commit_cost_cny(&mut self, cx: &mut Context<Self>) {
        self.commit_opt_f64(&self.cost_cny_input.clone(), "cost_cap_cny", cx);
    }

    fn commit_opt_f64(
        &mut self,
        input: &gpui_kit::Entity<crate::ui::InputState>,
        key: &'static str,
        cx: &mut Context<Self>,
    ) {
        let raw = input.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            self.patch_ai(serde_json::json!({ "agent": { key: null } }), None, cx);
            return;
        }
        match raw.parse::<f64>() {
            Ok(v) if v.is_finite() && v > 0.0 => {
                self.patch_ai(serde_json::json!({ "agent": { key: v } }), None, cx);
            }
            _ => {
                self.flash = Some((
                    Tone::Danger,
                    crate::ui::tr(cx)
                        .settings()
                        .actions_cost_cap_must_be_above()
                        .to_owned(),
                ));
                cx.notify();
            }
        }
    }

    pub(super) fn commit_max_tokens(&mut self, cx: &mut Context<Self>) {
        let raw = self.max_tokens_input.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            self.patch_ai(
                serde_json::json!({ "agent": { "max_run_tokens": null } }),
                None,
                cx,
            );
            return;
        }
        match raw.parse::<u64>() {
            Ok(v) if (1_000..=1_000_000_000).contains(&v) => {
                self.patch_ai(
                    serde_json::json!({ "agent": { "max_run_tokens": v } }),
                    None,
                    cx,
                );
            }
            _ => {
                self.flash = Some((
                    Tone::Danger,
                    crate::ui::tr(cx)
                        .settings()
                        .actions_token_limit_must_be_1()
                        .to_owned(),
                ));
                cx.notify();
            }
        }
    }

    /// Picks a default project workspace folder via GPUI's native async OS dialog.
    pub(super) fn pick_default_workspace(&mut self, cx: &mut Context<Self>) {
        let prompt = SharedString::from(
            crate::ui::tr(cx)
                .settings()
                .actions_choose_default_folder_for_new(),
        );
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt),
        });
        let task = cx.spawn(async move |weak, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(dir) = paths.into_iter().next()
            {
                let path_str = dir.to_string_lossy().into_owned();
                let _ = weak.update(cx, |this, cx| {
                    this.patch_ai(
                        serde_json::json!({ "agent": { "default_workspace": path_str } }),
                        None,
                        cx,
                    );
                });
            }
        });
        self._tasks.push(task);
    }

    pub(super) fn add_fallback(
        &mut self,
        site: &'static str,
        id_ref: String,
        cx: &mut Context<Self>,
    ) {
        let mut list = match site {
            "fast" => self.settings.ai.agent.fallbacks.fast.clone(),
            "deep" => self.settings.ai.agent.fallbacks.deep.clone(),
            "vision" => self.settings.ai.agent.fallbacks.vision.clone(),
            _ => self.settings.ai.agent.fallbacks.main.clone(),
        };
        if list.len() >= 5 || list.contains(&id_ref) {
            return;
        }
        list.push(id_ref);
        self.patch_ai(
            serde_json::json!({ "agent": { "fallbacks": { site: list } } }),
            None,
            cx,
        );
    }

    pub(super) fn remove_fallback(
        &mut self,
        site: &'static str,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let mut list = match site {
            "fast" => self.settings.ai.agent.fallbacks.fast.clone(),
            "deep" => self.settings.ai.agent.fallbacks.deep.clone(),
            "vision" => self.settings.ai.agent.fallbacks.vision.clone(),
            _ => self.settings.ai.agent.fallbacks.main.clone(),
        };
        if index < list.len() {
            list.remove(index);
            self.patch_ai(
                serde_json::json!({ "agent": { "fallbacks": { site: list } } }),
                None,
                cx,
            );
        }
    }

    pub(super) fn set_role_model(
        &mut self,
        role: SubagentRole,
        id_ref: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let key = role.as_str();
        self.patch_ai(
            serde_json::json!({ "agent": { "role_models": { key: id_ref } } }),
            None,
            cx,
        );
    }

    pub(super) fn apply_role_suggestions(&mut self, cx: &mut Context<Self>) {
        let mut patch_map = serde_json::Map::new();
        for r in &self.roles {
            if r.model.is_none()
                && let Some(sug) = &r.suggestion
            {
                patch_map.insert(
                    r.role.as_str().to_owned(),
                    serde_json::Value::String(sug.clone()),
                );
            }
        }
        if patch_map.is_empty() {
            self.flash = Some((
                Tone::Neutral,
                crate::ui::tr(cx)
                    .settings()
                    .agent_roles_nothing()
                    .to_owned(),
            ));
            cx.notify();
            return;
        }
        let msg = crate::ui::tr(cx)
            .settings()
            .actions_applied_capability_suggestions_to_role(patch_map.len());
        self.patch_ai(
            serde_json::json!({ "agent": { "role_models": patch_map } }),
            Some(msg),
            cx,
        );
    }

    pub(super) fn save_global_instructions(&mut self, cx: &mut Context<Self>) {
        let text = self.instructions_input.read(cx).value().to_string();
        let trimmed_chars = text.trim().chars().count();
        if trimmed_chars > 4000 {
            self.flash = Some((
                Tone::Danger,
                crate::ui::tr(cx)
                    .settings()
                    .actions_at_most_4_000_characters(trimmed_chars),
            ));
            cx.notify();
            return;
        }
        let synced = text.clone();
        let patch = serde_json::json!({ "agent": { "global_instructions": text } });
        let task = spawn_blocking_into(
            cx,
            move |state| settings_update_impl(state, CoreSection::Ai, &patch),
            move |this, res, cx| match res {
                Ok(next) => {
                    this.settings = next;
                    this.instructions_synced_text = synced;
                    this.instructions_dirty = false;
                    this.flash = Some((
                        Tone::Success,
                        crate::ui::tr(cx)
                            .settings()
                            .actions_saved_global_instructions()
                            .to_owned(),
                    ));
                    cx.notify();
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn clear_global_instructions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.instructions_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.save_global_instructions(cx);
    }

    pub(super) fn commit_searxng_url(&mut self, cx: &mut Context<Self>) {
        let url = self.searxng_url_input.read(cx).value().trim().to_owned();
        self.patch_ai(
            serde_json::json!({ "search": { "searxng_base_url": url } }),
            None,
            cx,
        );
    }

    pub(super) fn save_search_secret(
        &mut self,
        secret: SettingsSecret,
        clear: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let val = if clear {
            String::new()
        } else {
            let ent = match secret {
                SettingsSecret::SearchTavily => &self.tavily_key_input,
                SettingsSecret::SearchBrave => &self.brave_key_input,
            };
            let typed = ent.read(cx).value().trim().to_owned();
            if typed.is_empty() {
                return;
            }
            ent.update(cx, |s, cx| s.set_value("", window, cx));
            typed
        };
        let task = spawn_blocking_into(
            cx,
            move |state| settings_secret_set_impl(state, secret, &val),
            move |this, res, cx| match res {
                Ok(_) => {
                    this.flash = Some((
                        Tone::Success,
                        if clear {
                            crate::ui::tr(cx).settings().actions_search_key_removed()
                        } else {
                            crate::ui::tr(cx)
                                .settings()
                                .actions_search_key_saved_locally()
                        }
                        .to_owned(),
                    ));
                    this.reload_all(cx);
                }
                Err(err) => {
                    this.flash = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn test_search_backend(&mut self, cx: &mut Context<Self>) {
        if self.search_testing {
            return;
        }
        let backend: SearchBackend = self.settings.ai.search.backend;
        self.search_testing = true;
        self.search_test_result = None;
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move { ai_search_test_impl(&state, backend).await },
            |this, res, cx| {
                this.search_testing = false;
                match res {
                    Ok(dto) => this.search_test_result = Some(dto),
                    Err(err) => this.flash = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}
