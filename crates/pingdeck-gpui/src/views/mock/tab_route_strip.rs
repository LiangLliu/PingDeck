//! Inline Try-It result card and docked Live Traffic Strip for the Routes sub-tab.

use gpui_kit::{
    AnyElement, Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::model::items::MockServer;

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, btn_icon, card, h_row, meta_pill, method_badge, palette,
    to_hsla, v_col,
};

use super::state::{MockHandoff, MockSubTab, MockView};

pub fn render_try_it_card(
    view: &MockView,
    code: u16,
    ms: u64,
    url: &str,
    body: &str,
    cx: &mut Context<MockView>,
) -> AnyElement {
    let p = palette(cx);
    let tone = if code >= 500 {
        Tone::Danger
    } else if code >= 400 {
        Tone::Warning
    } else {
        Tone::Success
    };
    card(cx)
        .p(px(10.0))
        .gap(px(6.0))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(badge(code.to_string(), tone, cx))
                        .child(meta_pill(format!("{ms} ms"), cx))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(url.to_owned()),
                        ),
                )
                .child(
                    btn_icon("mock-try-close", UiIcon::Close).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.try_it_result = None;
                            cx.notify();
                        },
                    )),
                ),
        )
        .child(
            div()
                .w_full()
                .max_h(px(120.0))
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(if body.is_empty() {
                    crate::ui::Tr(view.lang)
                        .mock()
                        .tab_route_strip_empty_body()
                        .to_owned()
                } else {
                    body.to_owned()
                }),
        )
        .into_any_element()
}

pub fn render_live_traffic_strip(
    view: &MockView,
    srv: &MockServer,
    cx: &mut Context<MockView>,
) -> AnyElement {
    let p = palette(cx);
    let base_url = view
        .status_of(&srv.id)
        .map(|s| s.bound_url.clone())
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", srv.port));
    let mut strip = v_col()
        .w_full()
        .flex_shrink_0()
        .bg(to_hsla(p.pane))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .px(px(12.0))
        .py(px(6.0))
        .gap(px(4.0))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.muted))
                        .child(
                            crate::ui::tr(cx)
                                .mock()
                                .tab_route_strip_recent_traffic(view.logs.len()),
                        ),
                )
                .child(
                    btn_ghost(
                        "mock-strip-all-traffic",
                        crate::ui::Tr(view.lang)
                            .mock()
                            .tab_route_strip_all_traffic(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_tab = MockSubTab::Traffic;
                        cx.notify();
                    })),
                ),
        );
    for log in view.logs.iter().take(3) {
        let m = log.method.clone();
        let full_url = format!("{base_url}{}", log.path);
        let tone = if log.status_code >= 500 {
            Tone::Danger
        } else if log.status_code >= 400 {
            Tone::Warning
        } else {
            Tone::Success
        };
        strip = strip.child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .child(method_badge(&log.method, cx))
                .child(badge(log.status_code.to_string(), tone, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(log.path.clone()),
                )
                .child(meta_pill(format!("{}ms", log.elapsed_ms), cx))
                .child(
                    btn_ghost(
                        SharedString::from(format!("mock-strip-draft-{}", log.id)),
                        crate::ui::Tr(view.lang).mock().tab_route_strip_save_draft(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pending_handoff.set(Some(MockHandoff::OpenTestRequest {
                            name: format!("Mock {m}"),
                            method: m.clone(),
                            url: full_url.clone(),
                        }));
                        cx.notify();
                    })),
                ),
        );
    }
    strip.into_any_element()
}
