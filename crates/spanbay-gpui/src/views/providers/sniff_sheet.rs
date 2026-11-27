//! Centered 3-step Smart Sniff & Add / Re-Sniff Provider Modal Dialog (`SniffProviderSheet`):
//! Struct, initialization, circular stepper, pinned modal footer, and `modal_dialog` host.
//! Step 1 lives in `sniff_sheet_step1.rs`; Steps 2 & 3 live in `sniff_sheet_steps.rs`;
//! async actions live in `sniff_sheet_actions.rs`.

use std::collections::BTreeSet;

use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Subscription, Task, Window, div, px,
};
use spanbay_desktop::core::app::{DuplicateInfo, NormalizedUrlDto, llm_url_normalize_impl};
use spanbay_desktop::core::llm::presets::ProviderPreset;
use spanbay_desktop::core::llm::sniff::SniffReport;
use spanbay_desktop::core::llm::wire::Protocol;

use crate::ui::{
    Disableable as _, InputEvent, InputState, ScrollableElement as _, TextareaState, Tone,
    alert_banner, btn_ghost, btn_primary, btn_secondary, h_row, modal_dialog, palette,
    responsive_modal_bounds, to_hsla, tr, v_col,
};

use super::ProvidersView;
use super::sniff_sheet_step1::render_step_one;
use super::sniff_sheet_steps::{render_step_three, render_step_two};

#[derive(Debug, Clone)]
pub(super) struct LiveRow {
    pub(super) id: u32,
    pub(super) label: String,
    pub(super) status: Option<u16>,
    pub(super) latency_ms: Option<u64>,
    pub(super) note: String,
    pub(super) done: bool,
}

pub struct SniffProviderSheet {
    pub(super) parent: Entity<ProvidersView>,
    pub(super) presets: Vec<ProviderPreset>,
    pub(super) resniff_provider_id: Option<String>,
    pub(crate) step: u8,
    pub(super) selected_preset_id: Option<String>,
    pub(super) selected_region_id: Option<String>,
    pub(crate) url_input: Entity<InputState>,
    pub(super) key_input: Entity<InputState>,
    pub(crate) name_input: Entity<InputState>,
    pub(super) models_filter_input: Entity<InputState>,
    pub(super) manual_models_input: Entity<TextareaState>,
    pub(super) adv_models_url: Entity<InputState>,
    pub(super) adv_proxy_url: Entity<InputState>,
    pub(super) adv_open: bool,
    pub(super) extended_scan: bool,
    pub(super) allow_billable: bool,
    pub(super) normalized: Option<NormalizedUrlDto>,
    pub(super) scanning: bool,
    pub(super) cancel_token: Option<String>,
    pub(super) live_rows: Vec<LiveRow>,
    pub(super) report: Option<SniffReport>,
    pub(super) duplicate: Option<DuplicateInfo>,
    pub(super) checked_protocols: BTreeSet<Protocol>,
    pub(super) primary_protocol: Option<Protocol>,
    pub(super) checked_models: BTreeSet<String>,
    pub(super) saving: bool,
    pub(crate) flash: Option<(Tone, String)>,
    _subscriptions: Vec<Subscription>,
    pub(super) _stream_task: Option<Task<()>>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl SniffProviderSheet {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        parent: Entity<ProvidersView>,
        presets: Vec<ProviderPreset>,
        resniff_provider_id: Option<String>,
        initial_url: Option<String>,
        initial_name: Option<String>,
        initial_preset: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut init_url = initial_url.unwrap_or_default();
        let mut init_name = initial_name.unwrap_or_default();
        let mut init_region = None;
        if let Some(pid) = &initial_preset
            && let Some(pr) = presets.iter().find(|p| &p.id == pid)
        {
            if init_name.is_empty() {
                init_name.clone_from(&pr.name);
            }
            if let Some(reg) = pr.regions.first() {
                init_url = reg.base_url.clone();
                init_region = Some(reg.id.clone());
            }
        }
        let url_input = cx.new(|cx| {
            let mut st = InputState::new(window, cx)
                .placeholder("https://api.deepseek.com or relay Base URL…");
            if !init_url.is_empty() {
                st.set_value(init_url.clone(), window, cx);
            }
            st
        });
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr(cx).providers().prov_sniff_sheet_api_key_optional())
                .masked(true)
        });
        let name_input = cx.new(|cx| {
            let mut st = InputState::new(window, cx).placeholder(
                tr(cx)
                    .providers()
                    .prov_sniff_sheet_custom_name_optional_auto_inferred(),
            );
            if !init_name.is_empty() {
                st.set_value(init_name.clone(), window, cx);
            }
            st
        });
        let models_filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr(cx).providers().prov_providers_filter_models())
        });
        let manual_models_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(tr(cx).providers().prov_sniff_sheet_one_model_id_per_line())
        });
        let adv_models_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("/v1/models (optional)"));
        let adv_proxy_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("direct or http://127.0.0.1:7890"));

        let name_sub = cx.subscribe_in(
            &name_input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    this.flash = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } if this.step == 1 => {
                    this.start_sniff(window, cx);
                }
                _ => {}
            },
        );
        let url_sub = cx.subscribe_in(
            &url_input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    let raw = this.url_input.read(cx).value().trim().to_owned();
                    this.normalized = if raw.is_empty() {
                        None
                    } else {
                        llm_url_normalize_impl(&raw).ok()
                    };
                    this.flash = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } if this.step == 1 => {
                    this.start_sniff(window, cx);
                }
                _ => {}
            },
        );
        let key_sub = cx.subscribe_in(
            &key_input,
            window,
            |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) && this.step == 1 {
                    this.start_sniff(window, cx);
                }
            },
        );
        let filter_sub = cx.subscribe(&models_filter_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });

        if resniff_provider_id.is_none() {
            if init_name.is_empty() {
                name_input.update(cx, |s, cx| s.focus_handle(cx).focus(window, cx));
            } else if init_url.is_empty() {
                url_input.update(cx, |s, cx| s.focus_handle(cx).focus(window, cx));
            } else {
                key_input.update(cx, |s, cx| s.focus_handle(cx).focus(window, cx));
            }
        }

        let normalized = if init_url.is_empty() {
            None
        } else {
            llm_url_normalize_impl(&init_url).ok()
        };

        let mut sheet = Self {
            parent,
            presets,
            resniff_provider_id: resniff_provider_id.clone(),
            step: 1,
            selected_preset_id: initial_preset,
            selected_region_id: init_region,
            url_input,
            key_input,
            name_input,
            models_filter_input,
            manual_models_input,
            adv_models_url,
            adv_proxy_url,
            adv_open: false,
            extended_scan: false,
            allow_billable: false,
            normalized,
            scanning: false,
            cancel_token: None,
            live_rows: Vec::new(),
            report: None,
            duplicate: None,
            checked_protocols: BTreeSet::new(),
            primary_protocol: None,
            checked_models: BTreeSet::new(),
            saving: false,
            flash: None,
            _subscriptions: vec![name_sub, url_sub, key_sub, filter_sub],
            _stream_task: None,
            _tasks: Vec::new(),
        };
        if resniff_provider_id.is_some() {
            sheet.start_sniff(window, cx);
        }
        sheet
    }

    fn render_stepper(&self, cx: &gpui_kit::App) -> gpui_kit::Div {
        let p = palette(cx);
        let pr = tr(cx).providers();
        let step_item = |n: u8, label: &'static str| {
            let active = self.step == n;
            let done = self.step > n;
            let (circle_bg, circle_border, circle_fg, text_fg, weight) = if active {
                (
                    to_hsla(p.accent),
                    to_hsla(p.accent),
                    to_hsla(p.on_accent),
                    to_hsla(p.ink),
                    gpui_kit::FontWeight::SEMIBOLD,
                )
            } else if done {
                (
                    to_hsla(p.ok_soft),
                    to_hsla(p.ok_line.composite_over(p.ok_soft)),
                    to_hsla(p.ok),
                    to_hsla(p.ok),
                    gpui_kit::FontWeight::MEDIUM,
                )
            } else {
                (
                    to_hsla(p.sunken),
                    to_hsla(p.line_strong.composite_over(p.sunken)),
                    to_hsla(p.faint),
                    to_hsla(p.muted),
                    gpui_kit::FontWeight::MEDIUM,
                )
            };
            h_row()
                .gap(px(6.0))
                .child(
                    div()
                        .size(px(20.0))
                        .rounded(px(999.0))
                        .bg(circle_bg)
                        .border_1()
                        .border_color(circle_border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .flex_shrink_0()
                        .text_size(px(11.0))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(circle_fg)
                        .child(if done {
                            "✓".to_owned()
                        } else {
                            n.to_string()
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .font_weight(weight)
                        .text_color(text_fg)
                        .child(label),
                )
        };

        h_row()
            .w_full()
            .flex_shrink_0()
            .gap(px(8.0))
            .pb(px(12.0))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.overlay)))
            .child(step_item(1, pr.prov_sniff_sheet_address_key()))
            .child(div().text_xs().text_color(to_hsla(p.faint)).child("›"))
            .child(step_item(2, pr.prov_sniff_sheet_sniff_evidence()))
            .child(div().text_xs().text_color(to_hsla(p.faint)).child("›"))
            .child(step_item(3, pr.prov_sniff_sheet_review_save()))
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let pr = tr(cx).providers();
        let c = tr(cx).common();
        let name_empty = self.name_input.read(cx).value().trim().is_empty();
        let url_empty = self.url_input.read(cx).value().trim().is_empty();
        let missing_required = name_empty || url_empty;

        match self.step {
            1 => {
                let mut left = h_row().gap(px(6.0)).child(
                    btn_ghost("sn-unverified", pr.sniff_act_no_scan())
                        .disabled(missing_required || self.saving)
                        .on_click(cx.listener(|this, _, _, cx| this.create_unverified(cx))),
                );
                if self.selected_preset_id.is_some() {
                    left = left.child(
                        btn_secondary(
                            "sn-direct-preset",
                            pr.prov_sniff_sheet_quick_add_from_preset(),
                        )
                        .disabled(name_empty || self.saving)
                        .on_click(cx.listener(|this, _, _, cx| this.create_preset_direct(cx))),
                    );
                }
                let right = h_row()
                    .gap(px(8.0))
                    .child(
                        btn_secondary("sn-modal-cancel", c.cancel())
                            .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                    )
                    .child(
                        btn_primary("sn-go", pr.prov_sniff_sheet_start_sniff())
                            .disabled(self.resniff_provider_id.is_none() && missing_required)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.start_sniff(window, cx)),
                            ),
                    );
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .child(left)
                    .child(right)
                    .into_any_element()
            }
            2 if self.scanning => h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(palette(cx).muted))
                        .child(pr.prov_sniff_sheet_steps_scanning_protocols_models_balance()),
                )
                .child(
                    btn_secondary("sn-cancel", pr.prov_sniff_sheet_steps_cancel())
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_sniff(cx))),
                )
                .into_any_element(),
            2 => {
                let has_report = self.report.is_some();
                let left = btn_secondary(
                    if has_report {
                        "sn-back-1"
                    } else {
                        "sn-retry-1"
                    },
                    if has_report {
                        pr.prov_sniff_sheet_steps_edit_address()
                    } else {
                        pr.prov_sniff_sheet_steps_back()
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.step = 1;
                    cx.notify();
                }));
                let right: AnyElement = if has_report {
                    btn_primary("sn-next-3", pr.prov_sniff_sheet_steps_next_review_models())
                        .disabled(name_empty)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.step = 3;
                            cx.notify();
                        }))
                        .into_any_element()
                } else {
                    btn_primary("sn-rescan", pr.prov_sniff_sheet_start_sniff())
                        .on_click(cx.listener(|this, _, window, cx| this.start_sniff(window, cx)))
                        .into_any_element()
                };
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .child(left)
                    .child(right)
                    .into_any_element()
            }
            _ => {
                let save_label = if self.saving {
                    pr.prov_detail_config_extra_saving()
                } else if self.resniff_provider_id.is_some() {
                    pr.prov_sniff_sheet_steps_apply_changes()
                } else {
                    pr.prov_sniff_sheet_steps_save_provider()
                };
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        btn_secondary("sn-back-2", pr.prov_sniff_sheet_steps_back_2()).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.step = 2;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .child(
                                btn_secondary("sn-modal-cancel-3", c.cancel())
                                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                            )
                            .child(
                                btn_primary("sn-save-final", save_label)
                                    .disabled(name_empty || self.saving)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if !this.saving {
                                            this.save_from_sniff(true, cx);
                                        }
                                    })),
                            ),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Render for SniffProviderSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pr = tr(cx).providers();
        let title = if self.resniff_provider_id.is_some() {
            pr.prov_loader_re_scan_provider()
        } else {
            pr.prov_loader_add_ai_provider_smart_sniff()
        };

        let mut step_col = v_col().w_full().gap(px(12.0));
        if let Some((tone, msg)) = &self.flash {
            step_col = step_col.child(alert_banner(*tone, msg.clone(), None, cx));
        }
        step_col = match self.step {
            1 => step_col.child(render_step_one(self, cx)),
            2 => step_col.child(render_step_two(self, cx)),
            _ => step_col.child(render_step_three(self, cx)),
        };

        let vp = window.viewport_size();
        let (_, max_card_h) =
            responsive_modal_bounds(680.0, f32::from(vp.width), f32::from(vp.height));
        let body_max_h = (max_card_h - 190.0).max(220.0);

        let body = v_col()
            .w_full()
            .gap(px(14.0))
            .child(self.render_stepper(cx))
            .child(
                div()
                    .id("sniff-modal-scroll")
                    .w_full()
                    .max_h(px(body_max_h))
                    .overflow_y_scrollbar()
                    .child(step_col),
            )
            .into_any_element();

        let footer = self.render_footer(cx);
        let ent_close = cx.entity().clone();
        modal_dialog(
            "add-provider-modal",
            title,
            680.0,
            body,
            footer,
            window,
            cx,
            move |_, _, cx| {
                ent_close.update(cx, |this, cx| this.close_modal(cx));
            },
        )
    }
}
