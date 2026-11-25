//! Left provider-scope sidebar for `UsageView` matching `docs/design/prototype/index.html:1381-1413`.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::llm::status::ProviderStatus;

use crate::ui::{
    ScrollableElement as _, StatusShape, UiIcon, empty_inline, h_row, list_row, master_pane,
    palette, pane_footer, pane_header, status_indicator, to_hsla, v_col,
};

use super::state::{UsageView, fmt_compact};

pub fn render_sidebar(view: &UsageView, cx: &mut Context<UsageView>) -> AnyElement {
    let p = palette(cx);
    let zh = view.is_zh();

    let header = pane_header(view.t("用量与日志", "Usage & Logs"), None, None, None, cx);

    let all_row = view.total_scope_row();
    let all_sub = if zh {
        format!(
            "{} 次调用 · {} token",
            fmt_compact(all_row.requests),
            fmt_compact(all_row.total_tokens())
        )
    } else {
        format!(
            "{} calls · {} tok",
            fmt_compact(all_row.requests),
            fmt_compact(all_row.total_tokens())
        )
    };

    let mut body = v_col().w_full().p(px(8.0)).gap(px(4.0)).child(
        list_row(
            "usage-scope-all",
            view.scope_provider_id.is_none(),
            cx,
            cx.listener(|this, _, _, cx| {
                this.scope_provider_id = None;
                this.reload_all(cx);
            }),
        )
        .child(status_indicator(StatusShape::Live, cx))
        .child(
            v_col()
                .flex_1()
                .min_w_0()
                .gap(px(1.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(view.t("全部服务商", "All Providers")),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(all_sub),
                ),
        ),
    );

    if view.providers.is_empty() {
        body = body.child(empty_inline(
            view.t("尚未配置服务商", "No providers configured"),
            None,
            None,
            cx,
        ));
    } else {
        body = body.child(
            div()
                .px(px(6.0))
                .pt(px(8.0))
                .pb(px(2.0))
                .text_size(px(10.5))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.muted))
                .child(view.t("按服务商筛选", "By Provider")),
        );

        for prov in &view.providers {
            let pid = prov.id.clone();
            let active = view.scope_provider_id.as_deref() == Some(prov.id.as_str());
            let shape = match view.provider_statuses.get(&prov.id).map(|s| s.status) {
                Some(ProviderStatus::Healthy) => StatusShape::Ok,
                Some(ProviderStatus::NeedsAttention) => StatusShape::Warn,
                Some(ProviderStatus::Unreachable) => StatusShape::Bad,
                Some(ProviderStatus::Disabled | ProviderStatus::Unverified) | None => {
                    StatusShape::Idle
                }
            };
            let totals = view.provider_totals.get(&prov.id);
            let reqs = totals.map_or(0, |r| r.requests);
            let toks = totals.map_or(0, |r| r.total_tokens());
            let sub = if zh {
                format!("{} 次调用 · {} token", fmt_compact(reqs), fmt_compact(toks))
            } else {
                format!("{} calls · {} tok", fmt_compact(reqs), fmt_compact(toks))
            };
            body = body.child(
                list_row(
                    SharedString::from(format!("usage-scope-{}", prov.id)),
                    active,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.scope_provider_id = Some(pid.clone());
                        this.reload_all(cx);
                    }),
                )
                .child(status_indicator(shape, cx))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(prov.name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(sub),
                        ),
                ),
            );
        }
    }

    let footer = pane_footer(
        h_row()
            .gap(px(6.0))
            .child(status_indicator(
                if view.llm_stream_task.is_some() {
                    StatusShape::Live
                } else {
                    StatusShape::Idle
                },
                cx,
            ))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.muted))
                    .child(if zh {
                        format!("{} 个服务商 · 实时同步", view.providers.len())
                    } else {
                        format!("{} provider(s) · Live", view.providers.len())
                    }),
            ),
        None,
        cx,
    );
    let _ = UiIcon::Usage;

    let scroll = div()
        .id("usage-scope-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(body);
    master_pane(header, scroll, Some(footer.into_any_element()), cx).into_any_element()
}
