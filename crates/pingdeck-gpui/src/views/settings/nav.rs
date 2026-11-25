//! Left sidebar navigation rail for [`SettingsView`] with grouped typography hierarchy,
//! live bilingual search, and MCP attention indicator.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::state::{SettingsGroup, SettingsSection};

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_secondary, empty_inline, h_row,
    icon_xs, list_row, palette, search_input, status_indicator, to_hsla, v_col,
};

use super::state_inputs::section_matches_query;
use super::{SettingsView, agent};

pub(super) fn render_settings_sidebar(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let query = view.search_input.read(cx).value().trim().to_lowercase();

    let mcp_attn = view
        .agent_view
        .as_ref()
        .map(|v| v.read(cx).mcp_attention_count())
        .unwrap_or_else(|| agent::count_mcp_attention(&view.mcp_servers));

    let mut nav = v_col()
        .flex_1()
        .gap(px(2.0))
        .p(px(10.0))
        .overflow_y_scrollbar();

    let mut nav_hits = 0usize;
    let mut group_idx = 0usize;
    for group in SettingsGroup::ALL {
        let matching: Vec<SettingsSection> = SettingsSection::ALL
            .into_iter()
            .filter(|s| s.group() == group && section_matches_query(*s, &query))
            .collect();
        if matching.is_empty() {
            continue;
        }
        nav_hits += matching.len();
        let (gz, ge) = group.label_pair();
        let mut grp_hdr = h_row()
            .justify_between()
            .px(px(8.0))
            .pt(px(if group_idx == 0 { 4.0 } else { 10.0 }))
            .pb(px(4.0))
            .text_size(px(11.0))
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .text_color(to_hsla(p.faint))
            .child(if zh { gz } else { ge })
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(to_hsla(p.faint))
                    .child(SharedString::from(matching.len().to_string())),
            );
        if group_idx > 0 {
            grp_hdr = grp_hdr
                .mt(px(8.0))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)));
        }
        group_idx += 1;
        nav = nav.child(grp_hdr);
        for sec in matching {
            let (zl, el) = sec.label_pair();
            let is_active = view.active == sec;
            let show_mcp_warn = sec == SettingsSection::Mcp && mcp_attn > 0;
            nav = nav.child(
                list_row(
                    SharedString::from(format!("set-nav-{el}")),
                    is_active,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.set_section(sec, cx);
                    }),
                )
                .pl(px(14.0))
                .justify_between()
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(if is_active {
                            gpui_kit::FontWeight::SEMIBOLD
                        } else {
                            gpui_kit::FontWeight::MEDIUM
                        })
                        .child(if zh { zl } else { el }),
                )
                .children(show_mcp_warn.then(|| {
                    h_row()
                        .gap(px(4.0))
                        .text_size(px(10.5))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.warn))
                        .child(status_indicator(StatusShape::Warn, cx))
                        .child(if zh { "需要处理" } else { "Attention" })
                })),
            );
        }
    }

    if nav_hits == 0 {
        nav = nav.child(empty_inline(
            if zh {
                "没有匹配的设置项"
            } else {
                "No matching settings"
            },
            Some(SharedString::from(if zh {
                "尝试搜索：代理、超时、备份、快捷键、MCP"
            } else {
                "Try: proxy, timeout, backup, shortcuts, MCP"
            })),
            Some(
                btn_secondary("set-nav-clear", if zh { "清除搜索" } else { "Clear" })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.search_input
                            .update(cx, |i, cx| i.set_value("", window, cx));
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
            cx,
        ));
    }

    v_col()
        .w(px(230.0))
        .h_full()
        .bg(to_hsla(p.pane))
        .border_r_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .w_full()
                .h(px(44.0))
                .flex_shrink_0()
                .px(px(10.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    list_row(
                        "settings-back",
                        false,
                        cx,
                        cx.listener(|this, _, _, cx| {
                            this.pending_back.set(true);
                            cx.notify();
                        }),
                    )
                    .w_full()
                    .justify_between()
                    .gap(px(6.0))
                    .text_color(to_hsla(p.ink_2))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(icon_xs(UiIcon::ArrowLeft))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child(if zh { "返回工作区" } else { "Back" }),
                            ),
                    )
                    .child(badge(
                        if zh {
                            format!("{} 页", SettingsSection::ALL.len())
                        } else {
                            format!("{} pages", SettingsSection::ALL.len())
                        },
                        Tone::Neutral,
                        cx,
                    )),
                ),
        )
        .child(
            div()
                .w_full()
                .px(px(10.0))
                .pt(px(8.0))
                .pb(px(6.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(search_input(&view.search_input, cx)),
        )
        .child(nav)
}
