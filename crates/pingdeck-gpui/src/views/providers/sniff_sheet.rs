//! Native 3-step Smart Sniff & Add / Re-Sniff Provider Sheet (`SniffProviderSheet`):
//! Struct, initialization, Step 1 UI, and stepper host.
//! Async actions live in `sniff_sheet_actions.rs`; Steps 2 & 3 live in `sniff_sheet_steps.rs`.

use std::collections::BTreeSet;

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div, px,
};
use pingdeck_desktop::core::app::{DuplicateInfo, NormalizedUrlDto, llm_url_normalize_impl};
use pingdeck_desktop::core::llm::presets::ProviderPreset;
use pingdeck_desktop::core::llm::sniff::SniffReport;
use pingdeck_desktop::core::llm::wire::Protocol;

use crate::ui::{
    Anchor, InputEvent, InputState, ScrollableElement as _, TextareaState, Tone, badge, btn_ghost,
    btn_primary, btn_secondary, card, collapsible_box, field_row, filter_chip, h_row, list_row,
    palette, popover, section_heading, select_pill, text_input, to_hsla, toggle_switch, v_col,
};

use super::ProvidersView;
use super::sniff_sheet_steps::{render_step_three, render_step_two};

const QUICK_CHIPS: [&str; 8] = [
    "deepseek",
    "moonshot",
    "siliconflow",
    "openrouter",
    "openai",
    "anthropic",
    "google_gemini",
    "ollama",
];

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
    pub(super) zh: bool,
    pub(super) presets: Vec<ProviderPreset>,
    pub(super) resniff_provider_id: Option<String>,
    pub(super) step: u8,
    pub(super) selected_preset_id: Option<String>,
    pub(super) selected_region_id: Option<String>,
    pub(super) url_input: Entity<InputState>,
    pub(super) key_input: Entity<InputState>,
    pub(super) name_input: Entity<InputState>,
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
    pub(super) flash: Option<(Tone, String)>,
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
        initial_preset: Option<String>,
        zh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut init_url = initial_url.unwrap_or_default();
        let mut init_region = None;
        if let Some(pid) = &initial_preset
            && let Some(pr) = presets.iter().find(|p| &p.id == pid)
            && let Some(reg) = pr.regions.first()
        {
            init_url = reg.base_url.clone();
            init_region = Some(reg.id.clone());
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
                .placeholder(if zh {
                    "API 密钥（可选，仅存本机）"
                } else {
                    "API Key (optional)"
                })
                .masked(true)
        });
        let name_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if zh {
                "自定义名称（如 公司中转 / 个人主力；留空自动推断）"
            } else {
                "Custom name (optional; auto-inferred if empty)"
            })
        });
        let models_filter_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(if zh {
                "筛选模型…"
            } else {
                "Filter models…"
            })
        });
        let manual_models_input = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder(if zh {
                "每行输入一个模型 ID…"
            } else {
                "One model ID per line…"
            })
        });
        let adv_models_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("/v1/models (optional)"));
        let adv_proxy_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("direct or http://127.0.0.1:7890"));

        let url_sub = cx.subscribe(&url_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                let raw = this.url_input.read(cx).value().trim().to_owned();
                this.normalized = if raw.is_empty() {
                    None
                } else {
                    llm_url_normalize_impl(&raw).ok()
                };
                cx.notify();
            }
        });
        let filter_sub = cx.subscribe(&models_filter_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });

        let normalized = if init_url.is_empty() {
            None
        } else {
            llm_url_normalize_impl(&init_url).ok()
        };

        let mut sheet = Self {
            parent,
            zh,
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
            _subscriptions: vec![url_sub, filter_sub],
            _stream_task: None,
            _tasks: Vec::new(),
        };
        if resniff_provider_id.is_some() {
            sheet.start_sniff(window, cx);
        }
        sheet
    }
}

impl Render for SniffProviderSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let zh = self.zh;

        let step_pill = |n: u8, label: &'static str, cx: &gpui_kit::App| {
            let active = self.step == n;
            let done = self.step > n;
            badge(
                format!(
                    "{} {label}",
                    if done {
                        "✓".to_owned()
                    } else {
                        n.to_string()
                    }
                ),
                if active {
                    Tone::Accent
                } else if done {
                    Tone::Success
                } else {
                    Tone::Neutral
                },
                cx,
            )
        };
        let stepper = h_row()
            .gap(px(6.0))
            .child(step_pill(
                1,
                if zh {
                    "地址与密钥"
                } else {
                    "Address & Key"
                },
                cx,
            ))
            .child(div().text_xs().text_color(to_hsla(p.faint)).child("›"))
            .child(step_pill(
                2,
                if zh {
                    "协议嗅探证据"
                } else {
                    "Sniff Evidence"
                },
                cx,
            ))
            .child(div().text_xs().text_color(to_hsla(p.faint)).child("›"))
            .child(step_pill(
                3,
                if zh {
                    "审核模型并保存"
                } else {
                    "Review & Save"
                },
                cx,
            ));

        let mut body = v_col()
            .size_full()
            .gap(px(14.0))
            .overflow_y_scrollbar()
            .child(stepper);
        if let Some((tone, msg)) = &self.flash {
            body = body.child(badge(msg.clone(), *tone, cx));
        }

        match self.step {
            1 => {
                let mut quick_row = h_row().flex_wrap().gap(px(5.0));
                for pid in QUICK_CHIPS {
                    if let Some(pr) = self.presets.iter().find(|x| x.id == pid) {
                        let id = pr.id.clone();
                        let active = self.selected_preset_id.as_deref() == Some(pid);
                        quick_row = quick_row.child(
                            filter_chip(
                                SharedString::from(format!("sn-chip-{pid}")),
                                pr.name.clone(),
                                None,
                                active,
                                cx,
                            )
                            .h(px(26.0))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.pick_preset(id.clone(), window, cx);
                                },
                            )),
                        );
                    }
                }
                let ent_all = cx.entity().clone();
                let all_presets = self.presets.clone();
                let more_pop = popover("sn-more-presets")
                    .anchor(Anchor::TopLeft)
                    .trigger(select_pill(
                        "sn-more-trig",
                        if zh { "全部预设" } else { "All presets" },
                        false,
                        cx,
                    ))
                    .content(move |_, _, cx| {
                        let list_h = ((all_presets.len() as f32) * 30.0 + 12.0).clamp(80.0, 320.0);
                        let mut col = v_col().w(px(250.0)).gap(px(2.0)).p(px(6.0));
                        for (i, pr) in all_presets.iter().enumerate() {
                            let id = pr.id.clone();
                            let ent = ent_all.clone();
                            col = col.child(
                                list_row(
                                    SharedString::from(format!("sn-all-{i}")),
                                    false,
                                    cx,
                                    move |_, window, cx| {
                                        ent.update(cx, |this, cx| {
                                            this.pick_preset(id.clone(), window, cx);
                                        });
                                    },
                                )
                                .child(div().text_xs().child(SharedString::from(pr.name.clone()))),
                            );
                        }
                        div()
                            .w(px(250.0))
                            .h(px(list_h))
                            .overflow_y_scrollbar()
                            .id("sn-all-presets-scroll")
                            .child(col)
                    });
                quick_row = quick_row.child(more_pop);

                let adv_body = v_col()
                    .gap(px(8.0))
                    .pt(px(8.0))
                    .child(field_row(
                        if zh {
                            "扩展扫描（更多路径变体，上限 20 请求）"
                        } else {
                            "Extended scan (20 req budget)"
                        },
                        None,
                        toggle_switch("sn-adv-ext", self.extended_scan).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.extended_scan = !this.extended_scan;
                                cx.notify();
                            },
                        )),
                        cx,
                    ))
                    .child(field_row(
                        if zh {
                            "用真实请求验证路由（可能产生极少量费用）"
                        } else {
                            "Verify with real requests"
                        },
                        None,
                        toggle_switch("sn-adv-bill", self.allow_billable).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.allow_billable = !this.allow_billable;
                                cx.notify();
                            },
                        )),
                        cx,
                    ))
                    .child(field_row(
                        if zh {
                            "自定义模型列表路径"
                        } else {
                            "Models list path"
                        },
                        None,
                        div().w(px(220.0)).child(text_input(&self.adv_models_url)),
                        cx,
                    ))
                    .child(field_row(
                        if zh { "独立代理" } else { "Proxy" },
                        None,
                        div().w(px(220.0)).child(text_input(&self.adv_proxy_url)),
                        cx,
                    ));

                let mut actions = h_row().gap(px(8.0)).flex_wrap().child(
                    btn_primary(
                        "sn-go",
                        if zh {
                            "开始嗅探探测 →"
                        } else {
                            "Start Sniff →"
                        },
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.start_sniff(window, cx))),
                );
                if self.selected_preset_id.is_some() {
                    actions = actions.child(
                        btn_secondary(
                            "sn-direct-preset",
                            if zh {
                                "按预设直接添加"
                            } else {
                                "Quick add from preset"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.create_preset_direct(cx))),
                    );
                }
                actions = actions.child(
                    btn_ghost(
                        "sn-unverified",
                        if zh {
                            "不扫描直接添加"
                        } else {
                            "Add without scanning"
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.create_unverified(cx))),
                );

                body = body
                    .child(
                        v_col()
                            .gap(px(6.0))
                            .child(section_heading(
                                if zh { "快速预设" } else { "Quick Presets" },
                                cx,
                            ))
                            .child(quick_row),
                    )
                    .child(
                        card(cx).p(px(12.0)).child(
                            v_col()
                                .gap(px(10.0))
                                .child(
                                    v_col()
                                        .gap(px(4.0))
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .child(if zh {
                                                    "服务商名称（自定义，可选）"
                                                } else {
                                                    "Provider Name (custom, optional)"
                                                }),
                                        )
                                        .child(text_input(&self.name_input))
                                        .child(
                                            div()
                                                .text_size(px(11.0))
                                                .text_color(to_hsla(p.faint))
                                                .child(if zh {
                                                    "将作为 AI 对话与网关中的「服务商 / 模型」显示前缀"
                                                } else {
                                                    "Used as the \"Provider / Model\" prefix in Chat & Gateway"
                                                }),
                                        ),
                                )
                                .child(
                                    v_col()
                                        .gap(px(4.0))
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .child(if zh {
                                                    "服务商地址（Base URL）"
                                                } else {
                                                    "Base URL"
                                                }),
                                        )
                                        .child(text_input(&self.url_input))
                                        .children(self.normalized.as_ref().map(|n| {
                                            div()
                                                .text_size(px(11.0))
                                                .text_color(to_hsla(p.faint))
                                                .child(SharedString::from(format!(
                                                    "Origin: {}",
                                                    n.origin
                                                )))
                                        })),
                                )
                                .child(
                                    v_col()
                                        .gap(px(4.0))
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .child(if zh {
                                                    "API 密钥（明文 0600 保险库，仅存本机）"
                                                } else {
                                                    "API Key (stored locally)"
                                                }),
                                        )
                                        .child(text_input(&self.key_input)),
                                )
                                .child(
                                    btn_ghost(
                                        "sn-adv-tog",
                                        if zh {
                                            format!(
                                                "{} 高级选项",
                                                if self.adv_open { "▾" } else { "▸" }
                                            )
                                        } else {
                                            format!(
                                                "{} Advanced",
                                                if self.adv_open { "▾" } else { "▸" }
                                            )
                                        },
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.adv_open = !this.adv_open;
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(
                                    collapsible_box(self.adv_open, "sn-adv-col").content(adv_body),
                                ),
                        ),
                    )
                    .child(actions);
            }
            2 => body = body.child(render_step_two(self, cx)),
            _ => body = body.child(render_step_three(self, cx)),
        }

        body
    }
}
