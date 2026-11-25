//! Master sidebar pane listing workspace Flows with step counts, last-run status badges, and search.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_icon, btn_primary, empty_inline,
    h_row, icon_xs, list_row, master_pane, meta_pill, palette, pane_footer, pane_header,
    search_input, status_indicator, to_hsla, v_col,
};

use super::state::FlowsView;

pub fn render_sidebar(view: &FlowsView, cx: &mut Context<FlowsView>) -> AnyElement {
    let p = palette(cx);
    let zh = view.is_zh();
    let q = view.search_input.read(cx).value().trim().to_lowercase();

    let filtered: Vec<_> = view
        .entries
        .iter()
        .filter(|e| {
            q.is_empty()
                || e.flow.name.to_lowercase().contains(&q)
                || e.flow.description.to_lowercase().contains(&q)
        })
        .collect();

    let header = pane_header(
        view.t("流程", "Flows"),
        Some(view.entries.len()),
        Some(
            btn_primary("flow-new-btn", view.t("新建", "New"))
                .icon(icon_xs(UiIcon::Plus))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_create_flow_modal(window, cx);
                }))
                .into_any_element(),
        ),
        Some(search_input(&view.search_input, cx).into_any_element()),
        cx,
    );

    let mut list = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if filtered.is_empty() {
        list = list.child(empty_inline(
            if view.entries.is_empty() {
                view.t("暂无流程", "No flows yet")
            } else {
                view.t("没有匹配的流程", "No matching flows")
            },
            Some(SharedString::from(view.t(
                "将多个 API 请求按步骤串联，自动提取变量并验证断言。",
                "Chain API requests step-by-step with variable extractions and assertions.",
            ))),
            None,
            cx,
        ));
    } else {
        for entry in filtered {
            let fid = entry.flow.id.clone();
            let active = view.selected_flow_id.as_deref() == Some(entry.flow.id.as_str());
            let is_running_row = active && view.running;
            let shape = if is_running_row {
                StatusShape::Run
            } else {
                match &entry.last_run {
                    Some(r) if r.cancelled => StatusShape::Warn,
                    Some(r) if r.passed => StatusShape::Ok,
                    Some(_) => StatusShape::Bad,
                    None => StatusShape::Idle,
                }
            };
            let steps_text = if zh {
                format!("{} 步", entry.flow.steps.len())
            } else {
                format!("{} steps", entry.flow.steps.len())
            };

            let mut row = list_row(
                SharedString::from(format!("flow-item-{}", entry.flow.id)),
                active,
                cx,
                cx.listener(move |this, _, _, cx| {
                    this.select_flow(fid.clone(), cx);
                }),
            )
            .items_start()
            .py(px(7.0))
            .child(div().pt(px(3.0)).child(status_indicator(shape, cx)))
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(entry.flow.name.clone()),
                    )
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(meta_pill(steps_text, cx))
                            .children(
                                is_running_row.then(|| {
                                    badge(view.t("↻ 运行中", "↻ Running"), Tone::Accent, cx)
                                }),
                            )
                            .children(
                                (!is_running_row)
                                    .then_some(entry.last_run.as_ref())
                                    .flatten()
                                    .map(|r| {
                                        if r.cancelled {
                                            badge(view.t("已取消", "Cancelled"), Tone::Warning, cx)
                                        } else if r.passed {
                                            badge(format!("{} ms", r.elapsed_ms), Tone::Success, cx)
                                        } else {
                                            badge(
                                                format!("{}/{}", r.steps_passed, r.steps_total),
                                                Tone::Danger,
                                                cx,
                                            )
                                        }
                                    }),
                            ),
                    ),
            );
            if active && view.dirty {
                row = row.child(
                    div()
                        .size(px(6.0))
                        .mt(px(5.0))
                        .rounded(px(999.0))
                        .bg(to_hsla(p.accent))
                        .flex_shrink_0(),
                );
            }
            list = list.child(row);
        }
    }

    let footer = pane_footer(
        div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.muted))
            .child(if zh {
                format!("共 {} 条流程", view.entries.len())
            } else {
                format!("{} flows", view.entries.len())
            }),
        Some(
            btn_icon("flow-refresh-btn", UiIcon::Refresh)
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(ws) = this.workspace_id.clone() {
                        this.reload(ws, cx);
                    }
                }))
                .into_any_element(),
        ),
        cx,
    );

    master_pane(
        header,
        div()
            .id("flow-sidebar-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(list),
        Some(footer.into_any_element()),
        cx,
    )
    .into_any_element()
}
