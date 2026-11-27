//! Persistence mutations (save, reset, delete) for [`CatalogDrawer`].

use gpui_kit::{Context, Window};
use spanbay_desktop::core::app::{
    llm_model_alias_save_impl, llm_model_catalog_user_delete_impl,
    llm_model_catalog_user_save_impl, llm_model_pricing_delete_impl, llm_model_pricing_save_impl,
};
use spanbay_desktop::core::model::model_catalog::{
    ModelAlias, ModelPricing, PricingMode, UserModelInput,
};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::drawer::{CatalogDrawer, DrawerKind};
use super::format::{parse_opt_f64, parse_opt_u64};

impl CatalogDrawer {
    fn build_pricing(
        &self,
        provider_id: &str,
        model_id: &str,
        cx: &gpui_kit::App,
    ) -> Result<ModelPricing, String> {
        let mut p = ModelPricing::dynamic(provider_id, model_id);
        p.mode = self.pricing_mode;
        p.currency.clone_from(&self.currency);
        match self.pricing_mode {
            PricingMode::Dynamic => {
                p.catalog_family.clone_from(&self.selected_catalog_family);
            }
            PricingMode::Fixed => {
                let per = parse_opt_f64(&self.per_call_price.read(cx).value(), "Per-call")?
                    .ok_or_else(|| "Per-call price is required".to_owned())?;
                p.per_call = Some(per);
            }
            PricingMode::Custom => {
                p.input_per_mtok = Some(
                    parse_opt_f64(&self.in_price.read(cx).value(), "Input")?
                        .ok_or_else(|| "Input price is required".to_owned())?,
                );
                p.output_per_mtok = Some(
                    parse_opt_f64(&self.out_price.read(cx).value(), "Output")?
                        .ok_or_else(|| "Output price is required".to_owned())?,
                );
                p.cache_read_per_mtok =
                    parse_opt_f64(&self.cache_read_price.read(cx).value(), "Cache read")?;
                p.cache_write_per_mtok =
                    parse_opt_f64(&self.cache_write_price.read(cx).value(), "Cache write")?;
                p.reasoning_per_mtok =
                    parse_opt_f64(&self.reasoning_price.read(cx).value(), "Reasoning")?;
            }
        }
        Ok(p)
    }

    fn build_user_model_input(&self, cx: &gpui_kit::App) -> Result<UserModelInput, String> {
        let family = self.user_family.read(cx).value().trim().to_owned();
        let model_id = self.user_model_id.read(cx).value().trim().to_owned();
        if family.is_empty() || model_id.is_empty() {
            return Err("Family and Model ID are required".into());
        }
        Ok(UserModelInput {
            family,
            model_id,
            name: self.user_name.read(cx).value().trim().to_owned(),
            kind: self.user_kind,
            context_tokens: parse_opt_u64(&self.user_context.read(cx).value(), "Context")?,
            max_output_tokens: parse_opt_u64(&self.user_max_output.read(cx).value(), "Max output")?,
            reasoning: self.user_reasoning,
            vision: self.user_vision,
            tools: self.user_tools,
            input_per_mtok: parse_opt_f64(&self.in_price.read(cx).value(), "Input")?,
            output_per_mtok: parse_opt_f64(&self.out_price.read(cx).value(), "Output")?,
            cache_read_per_mtok: parse_opt_f64(
                &self.cache_read_price.read(cx).value(),
                "Cache read",
            )?,
            cache_write_per_mtok: parse_opt_f64(
                &self.cache_write_price.read(cx).value(),
                "Cache write",
            )?,
            reasoning_per_mtok: None,
            currency: self.currency.clone(),
        })
    }

    pub fn save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.error_msg = None;

        match &self.kind {
            DrawerKind::ModelDetail { row, .. } => {
                if let Some((ovr_pid, ovr_pname, ovr_mid)) = self.editing_provider_override.clone()
                {
                    let pricing = match self.build_pricing(&ovr_pid, &ovr_mid, cx) {
                        Ok(p) => p,
                        Err(msg) => {
                            self.error_msg = Some(msg);
                            cx.notify();
                            return;
                        }
                    };
                    let key_reload = row.model_key.clone();
                    self.busy = true;
                    cx.notify();
                    let task = spawn_blocking_into(
                        cx,
                        move |state| llm_model_pricing_save_impl(state, pricing),
                        move |this, res, cx| {
                            this.busy = false;
                            match res {
                                Ok(_) => {
                                    this.editing_provider_override = None;
                                    this.load_detail(key_reload, cx);
                                    let msg = crate::ui::tr(cx)
                                        .settings()
                                        .drawer_save_saved_override_for(ovr_pname);
                                    this.parent.update(cx, |p, cx| {
                                        p.set_flash(Tone::Success, msg, cx);
                                        p.reload_all(cx);
                                    });
                                }
                                Err(err) => this.error_msg = Some(err.message().to_owned()),
                            }
                            cx.notify();
                        },
                    );
                    self._tasks.push(task);
                    return;
                }
                let model_id = row.model_key.clone();
                let name = row.name.clone();
                let user_update = if row.source
                    == spanbay_desktop::core::model::model_catalog::CatalogSource::User
                {
                    match self.build_user_model_input(cx) {
                        Ok(u) => Some(u),
                        Err(msg) => {
                            self.error_msg = Some(msg);
                            cx.notify();
                            return;
                        }
                    }
                } else {
                    None
                };
                let pricing = match self.build_pricing("", &model_id, cx) {
                    Ok(p) => p,
                    Err(msg) => {
                        self.error_msg = Some(msg);
                        cx.notify();
                        return;
                    }
                };
                self.busy = true;
                cx.notify();
                let task = spawn_blocking_into(
                    cx,
                    move |state| {
                        if let Some(u) = user_update {
                            llm_model_catalog_user_save_impl(state, u)?;
                        }
                        llm_model_pricing_save_impl(state, pricing)
                    },
                    move |this, res, cx| {
                        this.busy = false;
                        match res {
                            Ok(_) => {
                                let msg = crate::ui::tr(cx)
                                    .settings()
                                    .drawer_save_saved_pricing_for(name);
                                this.parent.update(cx, |p, cx| {
                                    p.set_flash(Tone::Success, msg, cx);
                                    p.reload_all(cx);
                                });
                                this.pending_close = true;
                            }
                            Err(err) => this.error_msg = Some(err.message().to_owned()),
                        }
                        cx.notify();
                    },
                );
                self._tasks.push(task);
            }
            DrawerKind::AddUserModel => {
                let input = match self.build_user_model_input(cx) {
                    Ok(v) => v,
                    Err(msg) => {
                        self.error_msg = Some(msg);
                        cx.notify();
                        return;
                    }
                };
                let label = input.model_id.clone();
                self.busy = true;
                cx.notify();
                let task = spawn_blocking_into(
                    cx,
                    move |state| llm_model_catalog_user_save_impl(state, input),
                    move |this, res, cx| {
                        this.busy = false;
                        match res {
                            Ok(_) => {
                                let msg = crate::ui::tr(cx)
                                    .settings()
                                    .drawer_save_added_custom_model(label);
                                this.parent.update(cx, |p, cx| {
                                    p.set_flash(Tone::Success, msg, cx);
                                    p.reload_all(cx);
                                });
                                this.pending_close = true;
                            }
                            Err(err) => this.error_msg = Some(err.message().to_owned()),
                        }
                        cx.notify();
                    },
                );
                self._tasks.push(task);
            }
            DrawerKind::Unlisted { item } => {
                let pid = item.provider_id.clone();
                let mid = item.model_id.clone();
                let alias_key = self.selected_alias_key.clone();
                let pricing = if self.pricing_mode != PricingMode::Dynamic {
                    match self.build_pricing(&pid, &mid, cx) {
                        Ok(p) => Some(p),
                        Err(msg) => {
                            self.error_msg = Some(msg);
                            cx.notify();
                            return;
                        }
                    }
                } else {
                    None
                };

                if alias_key.is_none() && pricing.is_none() {
                    self.error_msg = Some(
                        crate::ui::tr(cx)
                            .settings()
                            .drawer_save_pick_a_catalog_model_to()
                            .into(),
                    );
                    cx.notify();
                    return;
                }

                self.busy = true;
                cx.notify();
                let label = mid.clone();
                let task = spawn_async_into(
                    cx,
                    move |state| async move {
                        if let Some(target_key) = alias_key {
                            llm_model_alias_save_impl(
                                &state,
                                ModelAlias {
                                    provider_id: pid.clone(),
                                    model_id: mid.clone(),
                                    model_key: target_key,
                                    updated_at_ms: 0,
                                },
                            )
                            .await?;
                        }
                        if let Some(p) = pricing {
                            llm_model_pricing_save_impl(&state, p)?;
                        }
                        Ok::<_, spanbay_desktop::core::error::AppError>(())
                    },
                    move |this, res, cx| {
                        this.busy = false;
                        match res {
                            Ok(()) => {
                                let msg = crate::ui::tr(cx)
                                    .settings()
                                    .drawer_save_saved_match_pricing_for(label);
                                this.parent.update(cx, |p, cx| {
                                    p.set_flash(Tone::Success, msg, cx);
                                    p.reload_all(cx);
                                });
                                this.pending_close = true;
                            }
                            Err(err) => this.error_msg = Some(err.message().to_owned()),
                        }
                        cx.notify();
                    },
                );
                self._tasks.push(task);
            }
        }
    }

    pub fn reset_pricing(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let (provider_id, model_id) = match &self.kind {
            DrawerKind::ModelDetail { row, .. } => (String::new(), row.model_key.clone()),
            DrawerKind::Unlisted { item } => (item.provider_id.clone(), item.model_id.clone()),
            DrawerKind::AddUserModel => return,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_model_pricing_delete_impl(state, &provider_id, &model_id),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.parent.update(cx, |p, cx| {
                            p.set_flash(
                                Tone::Success,
                                crate::ui::tr(cx)
                                    .settings()
                                    .drawer_save_reset_pricing_to_default()
                                    .into(),
                                cx,
                            );
                            p.reload_all(cx);
                        });
                        this.pending_close = true;
                    }
                    Err(err) => this.error_msg = Some(err.message().to_owned()),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_provider_override(
        &mut self,
        provider_id: String,
        model_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key_reload = match &self.kind {
            DrawerKind::ModelDetail { row, .. } => row.model_key.clone(),
            _ => return,
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_model_pricing_delete_impl(state, &provider_id, &model_id),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.load_detail(key_reload, cx);
                        this.parent.update(cx, |p, cx| p.reload_all(cx));
                    }
                    Err(err) => this.error_msg = Some(err.message().to_owned()),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_user_model(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let DrawerKind::ModelDetail { row, .. } = &self.kind else {
            return;
        };
        let fam = row.family.clone();
        let mid = row.model_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_model_catalog_user_delete_impl(state, &fam, &mid),
            move |this, res, cx| {
                match res {
                    Ok(_) => {
                        this.parent.update(cx, |p, cx| {
                            p.set_flash(
                                Tone::Success,
                                crate::ui::tr(cx)
                                    .settings()
                                    .drawer_save_deleted_custom_model()
                                    .into(),
                                cx,
                            );
                            p.reload_all(cx);
                        });
                        this.pending_close = true;
                    }
                    Err(err) => this.error_msg = Some(err.message().to_owned()),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}
