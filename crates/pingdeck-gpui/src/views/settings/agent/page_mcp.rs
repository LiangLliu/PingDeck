//! Settings › 智能体 › MCP 服务器 (`SettingsSection::Mcp`, prototype `ai/mcp`).

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use pingdeck_desktop::core::agent::mcp::{
    AgentMcpServerView, McpServerStatus, McpTransportConfig, McpTrustState,
};
use pingdeck_desktop::core::model::settings::Language;

use crate::ui::{
    Disableable as _, ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_danger,
    btn_ghost, btn_icon, btn_primary, btn_secondary, empty_state, h_row, icon_xs, markdown_view,
    palette, settings_sub_well, status_indicator, to_hsla, toggle_switch, v_col,
};

use super::AgentSettingsView;
use super::widgets::{format_mcp_status, section_card_flush, section_card_padded};

pub(super) fn render_mcp(
    view: &AgentSettingsView,
    lang: Language,
    _window: &mut Window,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let servers = view
        .config
        .as_ref()
        .map(|c| c.mcp_servers.as_slice())
        .unwrap_or(&[]);

    let toolbar = h_row()
        .gap(px(8.0))
        .flex_wrap()
        .child(
            btn_primary(
                "mcp-add-btn",
                crate::ui::tr(cx).settings().page_mcp_add_mcp_server(),
            )
            .on_click(cx.listener(|this, _, window, cx| this.open_mcp_add_sheet(window, cx))),
        )
        .child(
            btn_secondary(
                "mcp-json-btn",
                crate::ui::tr(cx).settings().page_mcp_edit_json(),
            )
            .icon(icon_xs(UiIcon::FileCode))
            .on_click(cx.listener(|this, _, window, cx| this.open_mcp_json_sheet(window, cx))),
        )
        .child(
            btn_ghost("mcp-refresh-all", crate::ui::tr(cx).common().refresh())
                .icon(icon_xs(UiIcon::Refresh))
                .on_click(cx.listener(|this, _, _, cx| this.reload_all(cx))),
        );

    let title = crate::ui::tr(cx)
        .settings()
        .page_mcp_external_tool_servers();
    let desc = div()
        .px(px(4.0))
        .text_xs()
        .text_color(to_hsla(p.muted))
        .child(crate::ui::Tr(lang).settings().sec_mcp_mcp_d().to_owned());

    if servers.is_empty() {
        return v_col()
            .w_full()
            .gap(px(8.0))
            .child(section_card_padded(
                title,
                Some(toolbar.into_any_element()),
                settings_sub_well(cx).h(px(186.0)).child(empty_state(
                    UiIcon::Server,
                    crate::ui::Tr(lang).settings().tools_mcp_empty().to_owned(),
                    crate::ui::Tr(lang)
                        .settings()
                        .tools_mcp_empty_d()
                        .to_owned(),
                    cx,
                )),
                cx,
            ))
            .child(desc);
    }

    let table_head = h_row()
        .px(px(12.0))
        .py(px(8.0))
        .gap(px(10.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(to_hsla(p.muted))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(crate::ui::tr(cx).settings().page_mcp_server()),
        )
        .child(
            div()
                .w(px(76.0))
                .flex_shrink_0()
                .child(crate::ui::tr(cx).settings().page_mcp_transport()),
        )
        .child(
            div()
                .w(px(180.0))
                .flex_shrink_0()
                .child(crate::ui::tr(cx).settings().page_mcp_status()),
        )
        .child(
            div()
                .w(px(172.0))
                .flex_shrink_0()
                .text_right()
                .child(crate::ui::tr(cx).settings().page_mcp_enable_actions()),
        );

    let mut rows = v_col().child(table_head);
    let total = servers.len();
    for (idx, srv_view) in servers.iter().enumerate() {
        rows = rows.child(render_mcp_row(
            view,
            srv_view,
            idx,
            idx + 1 < total,
            lang,
            cx,
        ));
    }

    v_col()
        .w_full()
        .gap(px(8.0))
        .child(section_card_flush(
            title,
            Some(toolbar.into_any_element()),
            rows,
            cx,
        ))
        .child(desc)
}

fn render_mcp_row(
    view: &AgentSettingsView,
    sv: &AgentMcpServerView,
    idx: usize,
    bottom_border: bool,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let srv = &sv.server;
    let sid = srv.id.clone();

    let (transport_tag, transport_target, is_stdio) = match &srv.transport {
        McpTransportConfig::Stdio { command, args, .. } => {
            let mut full = command.clone();
            if !args.is_empty() {
                full.push(' ');
                full.push_str(&args.join(" "));
            }
            ("stdio", full, true)
        }
        McpTransportConfig::Http { url, .. } => ("HTTP", url.clone(), false),
    };

    let (st_shape, st_text, st_fg) = format_mcp_status(sv, lang, &p);
    let restarting = view.mcp_restarting_id.as_deref() == Some(sid.as_str());
    let confirming_start = view.mcp_confirm_start_id.as_deref() == Some(sid.as_str());
    let confirming_delete = view.mcp_confirm_delete_id.as_deref() == Some(sid.as_str());
    let log_open = view.mcp_log_open_id.as_deref() == Some(sid.as_str());

    let trust_badge = match sv.trust_state {
        Some(McpTrustState::Trusted) => badge(
            crate::ui::Tr(lang)
                .settings()
                .tools_mcp_trusted()
                .to_owned(),
            Tone::Success,
            cx,
        ),
        Some(McpTrustState::NeedsReview) => badge(
            crate::ui::Tr(lang)
                .settings()
                .tools_mcp_drifted()
                .to_owned(),
            Tone::Warning,
            cx,
        ),
        _ => badge(
            crate::ui::Tr(lang)
                .settings()
                .tools_mcp_untrusted()
                .to_owned(),
            Tone::Neutral,
            cx,
        ),
    };

    let name_col = v_col()
        .flex_1()
        .min_w_0()
        .gap(px(2.0))
        .child(
            h_row()
                .gap(px(6.0))
                .flex_wrap()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(srv.name.clone())),
                )
                .child(trust_badge)
                .children(is_stdio.then(|| {
                    badge(
                        crate::ui::Tr(lang)
                            .settings()
                            .tools_mcp_not_sandboxed()
                            .to_owned(),
                        Tone::Neutral,
                        cx,
                    )
                })),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.faint))
                .truncate()
                .child(SharedString::from(transport_target.clone())),
        );

    let needs_auth = matches!(sv.status, McpServerStatus::NeedsAuth);
    let srv_auth = srv.clone();
    let status_inner = h_row()
        .w(px(180.0))
        .gap(px(5.0))
        .child(status_indicator(
            if restarting {
                StatusShape::Run
            } else {
                st_shape
            },
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(st_fg)
                .child(SharedString::from(st_text)),
        );
    let status_col: gpui_kit::AnyElement = if needs_auth {
        div()
            .id(SharedString::from(format!("mcp-auth-{idx}")))
            .cursor_pointer()
            .rounded(px(5.0))
            .px(px(4.0))
            .py(px(2.0))
            .bg(to_hsla(p.warn_soft))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_mcp_edit_sheet(srv_auth.clone(), window, cx);
            }))
            .child(status_inner)
            .into_any_element()
    } else {
        status_inner.into_any_element()
    };

    let srv_toggle = srv.clone();
    let srv_edit = srv.clone();
    let sid_restart = sid.clone();
    let sid_review = sid.clone();
    let sid_log = sid.clone();
    let sid_del = sid.clone();

    let needs_trust_btn = matches!(
        sv.trust_state,
        None | Some(McpTrustState::Untrusted | McpTrustState::NeedsReview)
    );
    let trust_btn_label = if sv.trust_state == Some(McpTrustState::NeedsReview) {
        crate::ui::Tr(lang).settings().tools_mcp_review().to_owned()
    } else {
        crate::ui::Tr(lang).settings().tools_mcp_trust().to_owned()
    };

    let actions_col = h_row()
        .w(px(172.0))
        .justify_end()
        .gap(px(4.0))
        .children(needs_trust_btn.then(|| {
            btn_ghost(
                SharedString::from(format!("mcp-trust-{idx}")),
                trust_btn_label,
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.review_mcp_server(sid_review.clone(), cx)),
            )
        }))
        .children(srv.enabled.then(|| {
            btn_icon(SharedString::from(format!("mcp-re-{idx}")), UiIcon::Refresh)
                .tooltip(crate::ui::tr(cx).settings().page_mcp_restart_server())
                .disabled(restarting)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.restart_mcp_server(sid_restart.clone(), cx);
                }))
        }))
        .children(is_stdio.then(|| {
            btn_icon(
                SharedString::from(format!("mcp-log-{idx}")),
                UiIcon::Terminal,
            )
            .tooltip(crate::ui::tr(cx).settings().page_mcp_view_stderr_log())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_mcp_log(sid_log.clone(), cx);
            }))
        }))
        .child(
            btn_icon(
                SharedString::from(format!("mcp-edit-{idx}")),
                UiIcon::Pencil,
            )
            .tooltip(crate::ui::tr(cx).settings().page_mcp_edit_server())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_mcp_edit_sheet(srv_edit.clone(), window, cx);
            })),
        )
        .child(
            btn_icon(SharedString::from(format!("mcp-del-{idx}")), UiIcon::Trash)
                .tooltip(crate::ui::tr(cx).settings().page_mcp_delete_server())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.mcp_confirm_delete_id = Some(sid_del.clone());
                    cx.notify();
                })),
        )
        .child(
            toggle_switch(SharedString::from(format!("mcp-en-{idx}")), srv.enabled).on_change(
                cx.listener(move |this, checked: &bool, _, cx| {
                    this.toggle_mcp_enabled(srv_toggle.clone(), *checked, false, cx);
                }),
            ),
        );

    let mut wrapper = v_col().child(
        h_row()
            .px(px(12.0))
            .py(px(10.0))
            .gap(px(10.0))
            .child(name_col)
            .child(
                div()
                    .w(px(76.0))
                    .flex_shrink_0()
                    .child(badge(transport_tag, Tone::Neutral, cx)),
            )
            .child(status_col)
            .child(actions_col),
    );

    if confirming_start {
        let srv_confirm = srv.clone();
        wrapper = wrapper.child(
            h_row()
                .justify_between()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .bg(to_hsla(p.warn_soft))
                .border_t_1()
                .border_color(to_hsla(p.warn_line.composite_over(p.warn_soft)))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.warn))
                                .child(SharedString::from(
                                    crate::ui::Tr(lang)
                                        .settings()
                                        .tools_mcp_start_title(&srv.name),
                                )),
                        )
                        .child(div().text_xs().text_color(to_hsla(p.ink_2)).child(
                            SharedString::from(format!(
                                "{} ({transport_target})",
                                crate::ui::Tr(lang).settings().tools_mcp_start_body()
                            )),
                        )),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            btn_ghost(
                                SharedString::from(format!("mcp-start-cancel-{idx}")),
                                crate::ui::tr(cx).common().cancel(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mcp_confirm_start_id = None;
                                cx.notify();
                            })),
                        )
                        .child(
                            btn_primary(
                                SharedString::from(format!("mcp-start-go-{idx}")),
                                crate::ui::Tr(lang)
                                    .settings()
                                    .tools_mcp_start_confirm()
                                    .to_owned(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.toggle_mcp_enabled(srv_confirm.clone(), true, true, cx);
                                },
                            )),
                        ),
                ),
        );
    }

    if confirming_delete {
        let sid_confirm_del = sid.clone();
        wrapper = wrapper.child(
            h_row()
                .justify_between()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .bg(to_hsla(p.bad_soft))
                .border_t_1()
                .border_color(to_hsla(p.bad_line.composite_over(p.bad_soft)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.bad))
                        .child(SharedString::from(format!(
                            "{} — {}",
                            crate::ui::Tr(lang)
                                .settings()
                                .tools_mcp_delete_title(&srv.name),
                            crate::ui::Tr(lang).settings().tools_mcp_delete_body()
                        ))),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            btn_ghost(
                                SharedString::from(format!("mcp-del-cancel-{idx}")),
                                crate::ui::tr(cx).common().cancel(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mcp_confirm_delete_id = None;
                                cx.notify();
                            })),
                        )
                        .child(
                            btn_danger(
                                SharedString::from(format!("mcp-del-go-{idx}")),
                                crate::ui::Tr(lang).settings().tools_mcp_delete().to_owned(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.delete_mcp_server(sid_confirm_del.clone(), cx);
                                },
                            )),
                        ),
                ),
        );
    }

    if log_open {
        let lines = view.mcp_logs.get(&sid).cloned().unwrap_or_default();
        let sid_ref = sid;
        let body = if lines.is_empty() {
            div()
                .p(px(10.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(
                    crate::ui::Tr(lang)
                        .settings()
                        .tools_mcp_log_empty()
                        .to_owned(),
                )
                .into_any_element()
        } else {
            let joined = format!("````text\n{}\n````", lines.join("\n"));
            div()
                .id(SharedString::from(format!("mcp-log-scroll-{idx}")))
                .w_full()
                .max_h(px(200.0))
                .overflow_y_scrollbar()
                .px(px(10.0))
                .py(px(6.0))
                .child(markdown_view(
                    SharedString::from(format!("mcp-log-md-{idx}")),
                    joined,
                ))
                .into_any_element()
        };
        wrapper = wrapper.child(
            v_col()
                .bg(to_hsla(p.sunken))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.sunken)))
                .child(
                    h_row()
                        .justify_between()
                        .px(px(10.0))
                        .py(px(5.0))
                        .border_b_1()
                        .border_color(to_hsla(p.line.composite_over(p.sunken)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(crate::ui::Tr(lang).settings().tools_mcp_log_d().to_owned()),
                        )
                        .child(
                            btn_ghost(
                                SharedString::from(format!("mcp-log-ref-{idx}")),
                                crate::ui::Tr(lang)
                                    .settings()
                                    .tools_mcp_log_refresh()
                                    .to_owned(),
                            )
                            .icon(icon_xs(UiIcon::Refresh))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.refresh_mcp_log(sid_ref.clone(), cx);
                                },
                            )),
                        ),
                )
                .child(body),
        );
    }

    if bottom_border {
        wrapper = wrapper
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)));
    }
    wrapper
}
