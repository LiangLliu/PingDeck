//! Provider Models Tab table rendering: sticky header + independently scrollable body
//! with stable model-keyed IDs and zero-shift selection feedback.

use std::collections::HashMap;

use gpui_kit::{
    Context, Div, InteractiveElement as _, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::app::{InputModality, ProviderModelView};
use pingdeck_desktop::core::llm::probe::ProbeStatus;
use pingdeck_desktop::core::llm::probe::listing::Presence;
use pingdeck_desktop::core::llm::store::ProbeResult;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_icon, btn_secondary, card, checkbox, h_row,
    palette, to_hsla, toggle_switch, v_col,
};

use super::ProvidersView;
use super::format::{format_price_state, format_probe_status, format_tokens};

#[allow(clippy::too_many_arguments)]
pub(super) fn render_models_table(
    view: &ProvidersView,
    prov_id: &str,
    filtered: &[&ProviderModelView],
    latest_probe: &HashMap<&str, &ProbeResult>,
    total_cnt: usize,
    shown_limit: usize,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> Div {
    let p = palette(cx);
    let visible_cnt = filtered.len().min(shown_limit);
    let all_visible_selected = visible_cnt > 0
        && filtered
            .iter()
            .take(shown_limit)
            .all(|m| view.selected_models.contains(&m.model.model_id));
    let visible_ids: Vec<String> = filtered
        .iter()
        .take(shown_limit)
        .map(|m| m.model.model_id.clone())
        .collect();

    let thead = h_row()
        .flex_shrink_0()
        .px(px(12.0))
        .py(px(8.0))
        .gap(px(10.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(to_hsla(p.muted))
        .child(
            div()
                .w(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    checkbox("pm-sel-all", all_visible_selected).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if all_visible_selected {
                                this.selected_models.clear();
                            } else {
                                for id in &visible_ids {
                                    this.selected_models.insert(id.clone());
                                }
                            }
                            cx.notify();
                        },
                    )),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(200.0))
                .child(if zh { "模型 ID" } else { "Model ID" }),
        )
        .child(div().w(px(105.0)).child(if zh {
            "上下文 / 输出"
        } else {
            "Context / Out"
        }))
        .child(div().w(px(110.0)).flex().justify_end().child(if zh {
            "生效价 / 1M"
        } else {
            "Price / 1M"
        }))
        .child(div().w(px(110.0)).flex().justify_end().child(if zh {
            "上次测试"
        } else {
            "Last test"
        }))
        .child(div().w(px(110.0)).flex().justify_end().child(if zh {
            "启用 / 操作"
        } else {
            "Enabled"
        }));

    let mut tbody = v_col();
    if filtered.is_empty() {
        tbody = tbody.child(
            v_col()
                .items_center()
                .gap(px(6.0))
                .py(px(28.0))
                .child(div().text_xs().text_color(to_hsla(p.muted)).child(if total_cnt == 0 {
                    if zh {
                        "暂无模型。点击「刷新模型」拉取服务商列表，或点击「+ 添加模型」手动录入。"
                    } else {
                        "No models yet. Click Refresh models or + Add model."
                    }
                } else if zh {
                    "没有符合当前筛选条件的模型。"
                } else {
                    "No models match the current filter."
                })),
        );
    } else {
        for (idx, mv) in filtered.iter().take(shown_limit).enumerate() {
            let m = &mv.model;
            let mid = m.model_id.clone();
            let mid_chk = mid.clone();
            let mid_tog = mid.clone();
            let mid_copy = mid.clone();
            let mid_del = mid.clone();
            let pid_tog = prov_id.to_owned();
            let pid_del = prov_id.to_owned();
            let checked = view.selected_models.contains(&m.model_id);
            let is_new = m.first_seen_ms.is_some() && m.new_cleared_at_ms.is_none();
            let is_removed = m.presence == Presence::NotInProvider;

            let ctx_val = m.context.or(mv.catalog_context);
            let out_val = m.max_output.or(mv.catalog_output);
            let ctx_str = match (ctx_val, out_val) {
                (Some(c), Some(o)) => format!("{} / {}", format_tokens(c), format_tokens(o)),
                (Some(c), None) => format_tokens(c),
                _ => "—".into(),
            };
            let price_str = format_price_state(mv.effective_price.as_ref(), zh);

            let probe_badge = latest_probe.get(m.model_id.as_str()).map(|pr| {
                let st = if pr.ok {
                    ProbeStatus::Ready
                } else {
                    match pr.status_word.as_str() {
                        "rejected" => ProbeStatus::Rejected,
                        "rate_limited" => ProbeStatus::RateLimited,
                        "timed_out" => ProbeStatus::TimedOut,
                        "unreachable" => ProbeStatus::Unreachable,
                        _ => ProbeStatus::InvalidResponse,
                    }
                };
                let (lbl, tone) = format_probe_status(st, zh);
                let text = if pr.ok {
                    pr.ttft_ms
                        .or(pr.latency_ms)
                        .map_or_else(|| lbl.to_owned(), |ms| format!("{ms} ms"))
                } else {
                    lbl.to_owned()
                };
                badge(text, tone, cx)
            });

            let mut tags = h_row().gap(px(4.0)).flex_wrap();
            if is_new {
                tags = tags.child(badge(if zh { "新增" } else { "New" }, Tone::Accent, cx));
            }
            if is_removed {
                tags = tags.child(badge(
                    if zh { "已移除" } else { "Removed" },
                    Tone::Warning,
                    cx,
                ));
            }
            if m.tools {
                tags = tags.child(cap_tag(if zh { "工具" } else { "Tools" }, cx));
            }
            if m.vision || mv.inputs.iter().any(|i| i.modality == InputModality::Image) {
                tags = tags.child(cap_tag(if zh { "视觉" } else { "Vision" }, cx));
            }
            if mv.inputs.iter().any(|i| i.modality == InputModality::Pdf) {
                tags = tags.child(cap_tag("PDF", cx));
            }

            let row_bg = to_hsla(if checked { p.accent_soft } else { p.surface });
            let hover_bg = to_hsla(if checked { p.accent_soft } else { p.pane });
            let mut row_el = h_row()
                .px(px(12.0))
                .py(px(8.0))
                .gap(px(10.0))
                .bg(row_bg)
                .hover(move |s| s.bg(hover_bg))
                .child(
                    div()
                        .w(px(22.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            checkbox(SharedString::from(format!("pm-chk-{mid}")), checked)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.selected_models.contains(&mid_chk) {
                                        this.selected_models.remove(&mid_chk);
                                    } else {
                                        this.selected_models.insert(mid_chk.clone());
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    v_col()
                        .flex_1()
                        .min_w(px(200.0))
                        .gap(px(3.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(if m.enabled { p.ink } else { p.muted }))
                                .truncate()
                                .child(SharedString::from(mid.clone())),
                        )
                        .child(tags),
                )
                .child(
                    div()
                        .w(px(105.0))
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(ctx_str)),
                )
                .child(
                    div()
                        .w(px(110.0))
                        .flex()
                        .justify_end()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(price_str)),
                )
                .child(
                    div()
                        .w(px(110.0))
                        .flex()
                        .justify_end()
                        .child(match probe_badge {
                            Some(b) => b,
                            None => div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .child(if zh { "未测试" } else { "Untested" }),
                        }),
                )
                .child(
                    h_row()
                        .w(px(110.0))
                        .justify_end()
                        .gap(px(4.0))
                        .child(
                            toggle_switch(SharedString::from(format!("pm-en-{mid}")), m.enabled)
                                .on_click(cx.listener({
                                    let next_en = !m.enabled;
                                    move |this, _, _, cx| {
                                        this.toggle_single_model(
                                            pid_tog.clone(),
                                            mid_tog.clone(),
                                            next_en,
                                            zh,
                                            cx,
                                        );
                                    }
                                })),
                        )
                        .child(
                            btn_icon(SharedString::from(format!("pm-copy-{mid}")), UiIcon::Copy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let _ =
                                        pingdeck_desktop::services::clipboard::set_text(&mid_copy);
                                    this.status_msg = Some((
                                        Tone::Success,
                                        if zh {
                                            "已复制模型 ID".into()
                                        } else {
                                            "Model ID copied".into()
                                        },
                                    ));
                                    cx.notify();
                                })),
                        )
                        .child(
                            btn_icon(SharedString::from(format!("pm-del-{mid}")), UiIcon::Trash)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_single_model(
                                        pid_del.clone(),
                                        mid_del.clone(),
                                        zh,
                                        cx,
                                    );
                                })),
                        ),
                );

            if idx + 1 < visible_cnt {
                row_el = row_el
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)));
            }
            tbody = tbody.child(row_el);
        }

        if filtered.len() > visible_cnt {
            let rem = filtered.len() - visible_cnt;
            tbody = tbody.child(
                h_row()
                    .justify_center()
                    .py(px(10.0))
                    .border_t_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)))
                    .child(
                        btn_secondary(
                            "pm-load-more",
                            if zh {
                                format!("再显示 80 个（剩余 {rem} 个）")
                            } else {
                                format!("Show 80 more ({rem} left)")
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.models_page_limit += 80;
                            cx.notify();
                        })),
                    ),
            );
        }
    }

    card(cx)
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(thead)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .id(SharedString::from(format!("pm-scroll-{prov_id}")))
                .child(tbody),
        )
}

fn cap_tag(text: &'static str, cx: &gpui_kit::App) -> Div {
    let p = palette(cx);
    div()
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .bg(to_hsla(p.sunken))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .text_size(px(10.5))
        .text_color(to_hsla(p.muted))
        .child(text)
}
