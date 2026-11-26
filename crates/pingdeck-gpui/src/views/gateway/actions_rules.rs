//! Routing-rule editor & route-preview actions for `GatewayView`.

use gpui_kit::Context;
use pingdeck_desktop::core::app::{
    llm_model_fallback_candidates_impl, llm_model_fallbacks_set_impl, llm_route_preview_impl,
};
use pingdeck_desktop::core::llm::store::ModelRef;

use crate::bridge::spawn_blocking_into;
use crate::ui::Tone;

use super::{GatewayView, RuleEditorState};

impl GatewayView {
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

    pub fn save_rule_editor(&mut self, cx: &mut Context<Self>) {
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
                            crate::ui::tr(cx)
                                .providers()
                                .prov_actions_rules_fallback_rule_saved()
                                .into()
                        } else {
                            let ws: Vec<String> = saved
                                .warnings
                                .iter()
                                .map(|w| {
                                    format!(
                                        "{} ({:?})",
                                        this.format_model_ref(&format!(
                                            "{}/{}",
                                            w.provider_id, w.model
                                        )),
                                        w.capability
                                    )
                                })
                                .collect();
                            crate::ui::tr(cx)
                                .providers()
                                .prov_act_saved_cap_notes(ws.join(" · "))
                        };
                        this.status_msg = Some((
                            if saved.warnings.is_empty() {
                                Tone::Success
                            } else {
                                Tone::Warning
                            },
                            msg,
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

    pub fn delete_gateway_rule(&mut self, primary: ModelRef, cx: &mut Context<Self>) {
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
                            crate::ui::tr(cx)
                                .providers()
                                .prov_actions_rules_fallback_rule_deleted()
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
