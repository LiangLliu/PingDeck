//! Unified Provider Pricing & Rates card inside `Provider › Config`:
//! Combines catalog pricing basis (migrated from Settings), smart distributor suggestion,
//! relay pricing multiplier & billing currency, local-free state, and live formula preview.

use gpui_kit::{Context, Div, ParentElement as _, SharedString, Styled as _, Window, div, px};
use pingdeck_desktop::core::app::{llm_provider_pricing_save_impl, llm_provider_save_view_impl};
use pingdeck_desktop::core::llm::store::SaveProviderOptions;
use pingdeck_desktop::core::llm::wire::AuthStyle;
use pingdeck_desktop::core::model::model_catalog::{
    ModelCatalogFamily, RelayPricing, SUPPORTED_CURRENCIES,
};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::{
    Anchor, Disableable as _, ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_primary,
    btn_secondary, card, field_row, h_row, icon_xs, list_row, palette, popover, section_heading,
    select_pill, text_input, to_hsla, v_col,
};

use super::detail_config::ProviderConfigForm;

impl ProviderConfigForm {
    pub(super) fn parse_relay_pricing(
        &self,
        cx: &gpui_kit::App,
    ) -> Result<Option<RelayPricing>, &'static str> {
        let mult_raw = self.relay_mult_input.read(cx).value().trim().to_owned();
        if mult_raw.is_empty() {
            return Ok(self.relay_currency.clone().map(|curr| RelayPricing {
                multiplier: 1.0,
                currency: Some(curr),
            }));
        }
        if let Ok(mult) = mult_raw.parse::<f64>()
            && mult.is_finite()
            && mult > 0.0
            && mult <= 1000.0
        {
            Ok(Some(RelayPricing {
                multiplier: mult,
                currency: self.relay_currency.clone(),
            }))
        } else {
            Err(crate::ui::tr(cx).providers().prov_cfg_multiplier_range())
        }
    }

    pub(crate) fn relay_dirty(&self, cx: &gpui_kit::App) -> bool {
        let saved = &self.view.provider.quirks.relay_pricing;
        match (self.parse_relay_pricing(cx), saved) {
            (Ok(None), None) => false,
            (Ok(Some(cur)), Some(sav)) => {
                (cur.multiplier - sav.multiplier).abs() > 1e-9 || cur.currency != sav.currency
            }
            _ => true,
        }
    }

    pub(super) fn reset_relay_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (mult_str, curr) = self
            .view
            .provider
            .quirks
            .relay_pricing
            .as_ref()
            .map(|rp| (format!("{}", rp.multiplier), rp.currency.clone()))
            .unwrap_or_default();
        self.relay_mult_input
            .update(cx, |i, cx| i.set_value(mult_str, window, cx));
        self.relay_currency = curr;
        self.pricing_flash = None;
        cx.notify();
    }

    pub fn save_catalog_basis(&mut self, catalog_family: Option<String>, cx: &mut Context<Self>) {
        let pid = self.view.provider.id.clone();
        let parent = self.parent.clone();
        self.saving_pricing = true;
        self.pricing_flash = None;
        cx.notify();
        let task = spawn_async_into(
            cx,
            move |state| async move {
                llm_provider_pricing_save_impl(&state, pid, catalog_family).await
            },
            move |this, res, cx| {
                this.saving_pricing = false;
                match res {
                    Ok(views) => {
                        this.pricing_flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_pricing_saved_basis()
                                .into(),
                        ));
                        parent.update(cx, |p, cx| {
                            p.provider_pricing = views
                                .into_iter()
                                .map(|v| (v.provider_id.clone(), v))
                                .collect();
                            p.reload_all(cx);
                        });
                    }
                    Err(err) => {
                        this.pricing_flash = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn save_relay_pricing_only(&mut self, cx: &mut Context<Self>) {
        let next_relay = match self.parse_relay_pricing(cx) {
            Ok(v) => v,
            Err(msg) => {
                self.pricing_flash = Some((Tone::Danger, msg.into()));
                cx.notify();
                return;
            }
        };
        let mut next = self.view.provider.clone();
        next.quirks.relay_pricing = next_relay;
        let opts = SaveProviderOptions {
            expected_updated_at_ms: Some(self.view.provider.updated_at_ms),
            confirm_host_change: false,
            keep_former_name: true,
        };
        let parent = self.parent.clone();
        self.saving_pricing = true;
        self.pricing_flash = None;
        cx.notify();
        let task = spawn_blocking_into(
            cx,
            move |state| llm_provider_save_view_impl(state, next, opts),
            move |this, res, cx| {
                this.saving_pricing = false;
                match res {
                    Ok(saved) => {
                        this.relay_currency = saved
                            .provider
                            .quirks
                            .relay_pricing
                            .as_ref()
                            .and_then(|r| r.currency.clone());
                        this.view = saved;
                        this.pricing_flash = Some((
                            Tone::Success,
                            crate::ui::tr(cx)
                                .providers()
                                .prov_pricing_saved_relay()
                                .into(),
                        ));
                        parent.update(cx, |p, cx| p.reload_all(cx));
                    }
                    Err(err) => {
                        this.pricing_flash = Some((Tone::Danger, err.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}

pub(super) fn render_pricing_card(
    form: &ProviderConfigForm,
    cx: &mut Context<ProviderConfigForm>,
) -> Div {
    let p = palette(cx);
    let tr_p = crate::ui::tr(cx).providers();
    let prov = &form.view.provider;
    let is_local_free = prov.category == "local" && prov.auth == AuthStyle::None;

    if is_local_free {
        return card(cx).p(px(16.0)).child(
            v_col()
                .gap(px(8.0))
                .child(
                    h_row()
                        .justify_between()
                        .gap(px(8.0))
                        .child(section_heading(tr_p.prov_pricing_section_title(), cx))
                        .child(badge(
                            tr_p.prov_pricing_local_free_badge(),
                            Tone::Success,
                            cx,
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(tr_p.prov_pricing_local_free_desc()),
                ),
        );
    }

    let (pricing_view, families) = {
        let parent = form.parent.read(cx);
        (
            parent.pricing_of(&prov.id).cloned(),
            parent.catalog_families.clone(),
        )
    };

    let cur_fam = pricing_view
        .as_ref()
        .and_then(|pv| pv.catalog_family.clone());
    let cur_fam_label = pricing_view.as_ref().and_then(|pv| {
        pv.catalog_family_name
            .clone()
            .or_else(|| pv.catalog_family.clone())
    });
    let sug_fam = pricing_view
        .as_ref()
        .and_then(|pv| pv.suggested_family.clone());
    let sug_name = pricing_view.as_ref().and_then(|pv| {
        pv.suggested_family_name
            .clone()
            .or_else(|| pv.suggested_family.clone())
    });
    let overrides = pricing_view.as_ref().map_or(0, |pv| pv.overrides);
    let basis_trigger_label = cur_fam_label
        .clone()
        .unwrap_or_else(|| tr_p.prov_pricing_follow_official().into());
    let basis_picker = render_basis_picker(
        cur_fam.clone(),
        sug_fam.clone(),
        sug_name.clone(),
        basis_trigger_label,
        families,
        form.saving_pricing,
        cx,
    );
    let coverage_text = pricing_view.as_ref().map(|pv| {
        match pv
            .catalog_family_name
            .as_deref()
            .or(pv.catalog_family.as_deref())
        {
            Some(fam) => tr_p.prov_pricing_coverage_family(fam, pv.matched, pv.models),
            None => tr_p.prov_pricing_coverage_official(pv.models),
        }
    });
    let show_suggestion = sug_fam
        .as_ref()
        .is_some_and(|sf| cur_fam.as_deref() != Some(sf.as_str()));
    let currency_picker = render_currency_picker(form.relay_currency.clone(), cx);
    let relay_dirty = form.relay_dirty(cx);

    let mult_preview = form.relay_mult_input.read(cx).value().trim().to_owned();
    let mult_valid = if mult_preview.is_empty() {
        form.relay_currency.is_some().then_some(1.0)
    } else {
        mult_preview
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0 && *v <= 1000.0)
    };
    let curr_preview = form
        .relay_currency
        .as_deref()
        .unwrap_or_else(|| tr_p.prov_pricing_currency_auto());
    let formula_text = match (cur_fam_label.as_deref(), mult_valid) {
        (Some(fam), Some(m)) => {
            tr_p.prov_pricing_formula_family_mult(fam, format!("{m}"), curr_preview)
        }
        (None, Some(m)) => tr_p.prov_pricing_formula_official_mult(format!("{m}"), curr_preview),
        (Some(fam), None) => tr_p.prov_pricing_formula_family(fam),
        (None, None) => tr_p.prov_pricing_formula_official().into(),
    };

    let mut content = v_col()
        .gap(px(10.0))
        .child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .flex_wrap()
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(section_heading(tr_p.prov_pricing_section_title(), cx))
                        .children((overrides > 0).then(|| {
                            badge(
                                tr_p.prov_pricing_overrides_badge(overrides),
                                Tone::Accent,
                                cx,
                            )
                        })),
                )
                .children(
                    form.pricing_flash
                        .clone()
                        .map(|(tone, msg)| badge(msg, tone, cx)),
                ),
        )
        .child(field_row(
            tr_p.prov_pricing_basis_label(),
            Some(SharedString::from(tr_p.prov_pricing_basis_desc())),
            basis_picker,
            cx,
        ))
        .children(coverage_text.map(|cov| {
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(cov))
        }));

    if show_suggestion && let Some(sf) = sug_fam {
        let sname = sug_name.as_deref().unwrap_or(sf.as_str());
        content = content.child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.accent_soft))
                .border_1()
                .border_color(to_hsla(p.accent_line.composite_over(p.accent_soft)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.accent_ink))
                        .child(SharedString::from(
                            tr_p.prov_pricing_suggested_banner(sname),
                        )),
                )
                .child(
                    btn_secondary("cfg-pricing-use-sug", tr_p.prov_pricing_apply_suggested())
                        .disabled(form.saving_pricing)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_catalog_basis(Some(sf.clone()), cx);
                        })),
                ),
        );
    }

    let mut relay_controls = h_row()
        .gap(px(6.0))
        .flex_wrap()
        .child(div().w(px(108.0)).child(text_input(&form.relay_mult_input)))
        .child(currency_picker);
    if relay_dirty {
        relay_controls = relay_controls
            .child(
                btn_primary("cfg-relay-apply", tr_p.prov_pricing_apply_multiplier())
                    .disabled(form.saving_pricing)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_relay_pricing_only(cx);
                    })),
            )
            .child(
                btn_ghost("cfg-relay-reset", tr_p.prov_pricing_reset_multiplier()).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.reset_relay_inputs(window, cx);
                    }),
                ),
            );
    }

    content = content
        .child(field_row(
            tr_p.prov_detail_config_extra_relay_pricing_multiplier(),
            Some(SharedString::from(
                tr_p.prov_detail_config_extra_multiply_official_prices_by_a(),
            )),
            relay_controls,
            cx,
        ))
        .child(
            div()
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.pane))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(formula_text)),
        );

    card(cx).p(px(16.0)).child(content)
}

fn check_mark(cx: &gpui_kit::App) -> Div {
    div()
        .text_color(to_hsla(palette(cx).accent))
        .child(icon_xs(UiIcon::Check))
}

fn render_basis_picker(
    cur_fam: Option<String>,
    sug_fam: Option<String>,
    sug_name: Option<String>,
    trigger_label: String,
    families: Vec<ModelCatalogFamily>,
    disabled: bool,
    cx: &mut Context<ProviderConfigForm>,
) -> impl gpui_kit::IntoElement + use<> {
    let ent = cx.entity().clone();
    let active = cur_fam.is_some();

    popover("cfg-pricing-basis-pop")
        .anchor(Anchor::TopRight)
        .trigger(
            select_pill(
                "cfg-pricing-basis-btn",
                SharedString::from(trigger_label),
                active,
                cx,
            )
            .disabled(disabled),
        )
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let tr_pop = crate::ui::tr(pop_cx).providers();
            let mut list = v_col()
                .w(px(260.0))
                .max_h(px(300.0))
                .gap(px(2.0))
                .overflow_y_scrollbar();

            let sel_off = cur_fam.is_none();
            let (e0, ph0) = (ent.clone(), pop_cx.entity().clone());
            list = list.child(
                list_row("cfg-pb-off", sel_off, pop_cx, move |_, window, cx| {
                    e0.update(cx, |this, cx| this.save_catalog_basis(None, cx));
                    ph0.update(cx, |st, cx| st.dismiss(window, cx));
                })
                .justify_between()
                .child(div().text_xs().child(tr_pop.prov_pricing_follow_official()))
                .children(sel_off.then(|| check_mark(pop_cx))),
            );

            if let Some(sf) = &sug_fam {
                let sname = sug_name.as_deref().unwrap_or(sf.as_str());
                let label = tr_pop.prov_pricing_suggested_option(sname);
                let sel = cur_fam.as_deref() == Some(sf.as_str());
                let (e1, sf_v, ph1) = (ent.clone(), sf.clone(), pop_cx.entity().clone());
                list = list.child(
                    list_row("cfg-pb-sug", sel, pop_cx, move |_, window, cx| {
                        e1.update(cx, |this, cx| {
                            this.save_catalog_basis(Some(sf_v.clone()), cx)
                        });
                        ph1.update(cx, |st, cx| st.dismiss(window, cx));
                    })
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(pop_p.accent_ink))
                            .child(SharedString::from(label)),
                    )
                    .children(sel.then(|| check_mark(pop_cx))),
                );
            }

            for (fi, fam) in families.iter().enumerate() {
                if sug_fam.as_deref() == Some(fam.id.as_str()) {
                    continue;
                }
                let sel = cur_fam.as_deref() == Some(fam.id.as_str());
                let (ef, fid, phf) = (ent.clone(), fam.id.clone(), pop_cx.entity().clone());
                list =
                    list.child(
                        list_row(
                            SharedString::from(format!("cfg-pb-f-{fi}")),
                            sel,
                            pop_cx,
                            move |_, window, cx| {
                                ef.update(cx, |this, cx| {
                                    this.save_catalog_basis(Some(fid.clone()), cx)
                                });
                                phf.update(cx, |st, cx| st.dismiss(window, cx));
                            },
                        )
                        .justify_between()
                        .child(
                            div().flex_1().min_w_0().text_xs().truncate().child(
                                SharedString::from(format!("{} ({})", fam.name, fam.models)),
                            ),
                        )
                        .children(sel.then(|| check_mark(pop_cx))),
                    );
            }
            list
        })
}

fn render_currency_picker(
    current: Option<String>,
    cx: &mut Context<ProviderConfigForm>,
) -> impl gpui_kit::IntoElement + use<> {
    let ent = cx.entity().clone();
    let label = current.clone().unwrap_or_else(|| {
        crate::ui::tr(cx)
            .providers()
            .prov_pricing_currency_auto()
            .into()
    });
    let active = current.is_some();

    popover("cfg-relay-curr-pop")
        .anchor(Anchor::TopRight)
        .trigger(select_pill(
            "cfg-relay-curr-btn",
            SharedString::from(label),
            active,
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let mut list = v_col().w(px(160.0)).gap(px(2.0));
            let sel_auto = current.is_none();
            let (e0, ph0) = (ent.clone(), pop_cx.entity().clone());
            list = list.child(
                list_row("cfg-rc-auto", sel_auto, pop_cx, move |_, window, cx| {
                    e0.update(cx, |this, cx| {
                        this.relay_currency = None;
                        this.pricing_flash = None;
                        cx.notify();
                    });
                    ph0.update(cx, |st, cx| st.dismiss(window, cx));
                })
                .justify_between()
                .child(
                    div().text_xs().child(
                        crate::ui::tr(pop_cx)
                            .providers()
                            .prov_pricing_currency_auto(),
                    ),
                )
                .children(sel_auto.then(|| check_mark(pop_cx))),
            );
            for code in SUPPORTED_CURRENCIES {
                let sel = current.as_deref() == Some(code);
                let (ec, phc) = (ent.clone(), pop_cx.entity().clone());
                list = list.child(
                    list_row(
                        SharedString::from(format!("cfg-rc-{code}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            ec.update(cx, |this, cx| {
                                this.relay_currency = Some(code.to_owned());
                                this.pricing_flash = None;
                                cx.notify();
                            });
                            phc.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(code))
                    .children(sel.then(|| check_mark(pop_cx))),
                );
            }
            list
        })
}
