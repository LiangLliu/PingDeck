//! Request Editor (Params / Headers / Auth / Body / Scripts / Snippets) and Response Inspector
//! (Body / Headers / Cookies / Summary) panes for `RequestsView` using native `TabBar`s.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::snippet_service::SnippetLanguage;
use pingdeck_desktop::core::model::KeyValue;
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_icon, btn_primary, card, checkbox,
    code_editor, field_row, h_row, kv_chip, palette, section_heading, tab_bar_segmented,
    tab_bar_underline, tab_item, text_input, to_hsla, v_col,
};

use super::{AuthMode, ReqTab, RequestsView, RespTab};

pub fn render_request_editor(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let ent = cx.entity().clone();

    let params_lbl = if view.query_params.is_empty() {
        if zh { "参数".into() } else { "Params".into() }
    } else if zh {
        format!("参数 ({})", view.query_params.len())
    } else {
        format!("Params ({})", view.query_params.len())
    };
    let hdr_lbl = if view.headers.is_empty() {
        if zh {
            "请求头".into()
        } else {
            "Headers".into()
        }
    } else if zh {
        format!("请求头 ({})", view.headers.len())
    } else {
        format!("Headers ({})", view.headers.len())
    };

    let tabs = tab_bar_underline("req-editor-tabs")
        .selected_index(view.req_tab.index())
        .child(tab_item(params_lbl))
        .child(tab_item(hdr_lbl))
        .child(tab_item(if zh { "鉴权" } else { "Auth" }))
        .child(tab_item(if zh { "请求体" } else { "Body" }))
        .child(tab_item(if zh { "脚本" } else { "Scripts" }))
        .child(tab_item(if zh { "代码片段" } else { "Snippets" }))
        .on_click(move |idx, window, cx| {
            let next = ReqTab::from_index(*idx);
            ent.update(cx, |this, cx| {
                this.req_tab = next;
                if next == ReqTab::Snippets {
                    this.refresh_snippet(this.snippet_lang, window, cx);
                } else {
                    cx.notify();
                }
            });
        });

    let body = match view.req_tab {
        ReqTab::Params => render_kv_editor(
            "param",
            &view.query_params,
            &view.new_param_key,
            &view.new_param_val,
            zh,
            true,
            cx,
        )
        .into_any_element(),
        ReqTab::Headers => render_kv_editor(
            "hdr",
            &view.headers,
            &view.new_hdr_key,
            &view.new_hdr_val,
            zh,
            false,
            cx,
        )
        .into_any_element(),
        ReqTab::Auth => render_auth_tab(view, zh, cx).into_any_element(),
        ReqTab::Body => div()
            .size_full()
            .p(px(10.0))
            .child(code_editor(&view.body_editor))
            .into_any_element(),
        ReqTab::Scripts => div()
            .size_full()
            .p(px(10.0))
            .child(code_editor(&view.script_editor))
            .into_any_element(),
        ReqTab::Snippets => render_snippets_tab(view, zh, cx).into_any_element(),
    };

    v_col()
        .size_full()
        .bg(to_hsla(p.surface))
        .child(
            div()
                .px(px(12.0))
                .pt(px(4.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(tabs),
        )
        .child(div().flex_1().min_h_0().child(body))
}

fn render_kv_editor(
    prefix: &'static str,
    rows: &[KeyValue],
    key_in: &gpui_kit::Entity<crate::ui::InputState>,
    val_in: &gpui_kit::Entity<crate::ui::InputState>,
    zh: bool,
    is_param: bool,
    cx: &mut Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let mut list = v_col().gap(px(4.0));
    for (idx, item) in rows.iter().enumerate() {
        list = list.child(
            h_row()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    checkbox(
                        SharedString::from(format!("kv-chk-{prefix}-{idx}")),
                        item.enabled,
                    )
                    .on_click(cx.listener(
                        move |this, checked: &bool, _, cx| {
                            let target = if is_param {
                                &mut this.query_params
                            } else {
                                &mut this.headers
                            };
                            if let Some(row) = target.get_mut(idx) {
                                row.enabled = *checked;
                            }
                            cx.notify();
                        },
                    )),
                )
                .child(
                    div()
                        .w(px(190.0))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(item.key.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(SharedString::from(item.value.clone())),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("kv-del-{prefix}-{idx}")),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let target = if is_param {
                            &mut this.query_params
                        } else {
                            &mut this.headers
                        };
                        if idx < target.len() {
                            target.remove(idx);
                        }
                        cx.notify();
                    })),
                ),
        );
    }

    v_col()
        .size_full()
        .p(px(12.0))
        .gap(px(8.0))
        .overflow_y_scrollbar()
        .child(
            h_row()
                .gap(px(6.0))
                .child(div().w(px(190.0)).child(text_input(key_in)))
                .child(div().flex_1().child(text_input(val_in)))
                .child(
                    btn_primary(
                        SharedString::from(format!("kv-add-{prefix}")),
                        if zh { "添加" } else { "Add" },
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if is_param {
                            this.add_param(window, cx);
                        } else {
                            this.add_header(window, cx);
                        }
                    })),
                ),
        )
        .child(list)
}

fn render_auth_tab(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> impl IntoElement {
    let ent = cx.entity().clone();
    let mut col = v_col()
        .size_full()
        .p(px(14.0))
        .gap(px(10.0))
        .overflow_y_scrollbar()
        .child(field_row(
            if zh { "鉴权类型" } else { "Auth type" },
            None,
            tab_bar_segmented("req-auth-seg")
                .selected_index(view.auth_mode.index())
                .child(tab_item(if zh { "无" } else { "None" }))
                .child(tab_item("Bearer"))
                .child(tab_item("Basic"))
                .child(tab_item("API Key"))
                .on_click(move |idx, _, cx| {
                    let mode = AuthMode::from_index(*idx);
                    ent.update(cx, |this, cx| {
                        this.auth_mode = mode;
                        cx.notify();
                    });
                }),
            cx,
        ));

    match view.auth_mode {
        AuthMode::None => {}
        AuthMode::Bearer => {
            col = col.child(field_row(
                "Bearer Token",
                Some(SharedString::from(if zh {
                    "支持 {{变量}} 占位符"
                } else {
                    "Supports {{variable}} interpolation"
                })),
                div().w(px(340.0)).child(text_input(&view.auth_primary)),
                cx,
            ));
        }
        AuthMode::Basic => {
            col = col
                .child(field_row(
                    if zh { "用户名" } else { "Username" },
                    None,
                    div().w(px(300.0)).child(text_input(&view.auth_primary)),
                    cx,
                ))
                .child(field_row(
                    if zh { "密码" } else { "Password" },
                    None,
                    div().w(px(300.0)).child(text_input(&view.auth_secondary)),
                    cx,
                ));
        }
        AuthMode::ApiKey => {
            col = col
                .child(field_row(
                    if zh { "Header 名称" } else { "Header name" },
                    None,
                    div().w(px(300.0)).child(text_input(&view.auth_primary)),
                    cx,
                ))
                .child(field_row(
                    "API Key",
                    None,
                    div().w(px(300.0)).child(text_input(&view.auth_secondary)),
                    cx,
                ));
        }
    }
    col
}

fn render_snippets_tab(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let ent = cx.entity().clone();
    let lang_idx = match view.snippet_lang {
        SnippetLanguage::Curl => 0,
        SnippetLanguage::Fetch => 1,
        SnippetLanguage::Python => 2,
        SnippetLanguage::Go => 3,
        _ => 0,
    };

    v_col()
        .size_full()
        .p(px(10.0))
        .gap(px(8.0))
        .child(
            h_row()
                .justify_between()
                .child(
                    tab_bar_segmented("req-snip-lang-seg")
                        .selected_index(lang_idx)
                        .child(tab_item("cURL"))
                        .child(tab_item("fetch"))
                        .child(tab_item("Python"))
                        .child(tab_item("Go"))
                        .on_click(move |idx, window, cx| {
                            let lang = match *idx {
                                1 => SnippetLanguage::Fetch,
                                2 => SnippetLanguage::Python,
                                3 => SnippetLanguage::Go,
                                _ => SnippetLanguage::Curl,
                            };
                            ent.update(cx, |this, cx| this.refresh_snippet(lang, window, cx));
                        }),
                )
                .child(
                    btn_primary("snip-copy", if zh { "复制代码" } else { "Copy" })
                        .icon(crate::ui::icon_xs(UiIcon::Copy))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let txt = this.snippet_editor.read(cx).value().to_string();
                            let _ = clipboard::set_text(&txt);
                            this.status_msg = Some((Tone::Success, "Snippet copied".into()));
                            cx.notify();
                        })),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .child(code_editor(&view.snippet_editor)),
        )
}

pub fn render_response_inspector(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let ent = cx.entity().clone();

    let hdr_cnt = view.last_response.as_ref().map_or(0, |r| r.headers.len());
    let ck_cnt = view.last_response.as_ref().map_or(0, |r| r.cookies.len());

    let tabs = tab_bar_underline("resp-tabs")
        .selected_index(view.resp_tab.index())
        .child(tab_item(if zh { "响应体" } else { "Body" }))
        .child(tab_item(if zh {
            format!("响应头 ({hdr_cnt})")
        } else {
            format!("Headers ({hdr_cnt})")
        }))
        .child(tab_item(format!("Cookies ({ck_cnt})")))
        .child(tab_item(if zh { "摘要" } else { "Summary" }))
        .on_click(move |idx, _, cx| {
            let next = RespTab::from_index(*idx);
            ent.update(cx, |this, cx| {
                this.resp_tab = next;
                cx.notify();
            });
        });

    let metrics = if let Some(resp) = &view.last_response {
        let tone = match resp.status_code {
            200..=299 => Tone::Success,
            400..=499 => Tone::Warning,
            _ => Tone::Danger,
        };
        h_row()
            .gap(px(6.0))
            .child(badge(
                format!("{} {}", resp.status_code, resp.status_text),
                tone,
                cx,
            ))
            .child(kv_chip("Time", format!("{} ms", resp.elapsed_ms), cx))
            .child(kv_chip("Size", format!("{} B", resp.body_size_bytes), cx))
            .child(
                btn_icon("resp-copy-body", UiIcon::Copy)
                    .tooltip(if zh {
                        "复制响应体"
                    } else {
                        "Copy response"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        let txt = this.response_editor.read(cx).value().to_string();
                        let _ = clipboard::set_text(&txt);
                        this.status_msg = Some((Tone::Success, "Response copied".into()));
                        cx.notify();
                    })),
            )
    } else {
        h_row().child(badge(
            if zh { "等待发送" } else { "Idle" },
            Tone::Neutral,
            cx,
        ))
    };

    let content: gpui_kit::AnyElement = match view.resp_tab {
        RespTab::Body => div()
            .size_full()
            .p(px(10.0))
            .child(code_editor(&view.response_editor))
            .into_any_element(),
        RespTab::Headers => {
            let mut list = v_col()
                .size_full()
                .p(px(12.0))
                .gap(px(4.0))
                .overflow_y_scrollbar();
            if let Some(resp) = &view.last_response {
                for h in &resp.headers {
                    list = list.child(
                        h_row()
                            .gap(px(10.0))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(5.0))
                            .bg(to_hsla(p.pane))
                            .child(
                                div()
                                    .w(px(210.0))
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(h.key.clone())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(to_hsla(p.ink_2))
                                    .truncate()
                                    .child(SharedString::from(h.value.clone())),
                            ),
                    );
                }
            }
            list.into_any_element()
        }
        RespTab::Cookies => {
            let mut list = v_col()
                .size_full()
                .p(px(12.0))
                .gap(px(6.0))
                .overflow_y_scrollbar();
            if let Some(resp) = &view.last_response {
                for ck in &resp.cookies {
                    list = list.child(
                        card(cx).p(px(8.0)).child(
                            h_row()
                                .justify_between()
                                .gap(px(8.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .child(SharedString::from(ck.name.clone())),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .text_xs()
                                        .text_color(to_hsla(p.muted))
                                        .truncate()
                                        .child(SharedString::from(ck.value.clone())),
                                )
                                .children(ck.domain.as_ref().map(|d| {
                                    badge(SharedString::from(d.clone()), Tone::Neutral, cx)
                                })),
                        ),
                    );
                }
            }
            list.into_any_element()
        }
        RespTab::Summary => {
            let mut col = v_col()
                .size_full()
                .p(px(14.0))
                .gap(px(8.0))
                .overflow_y_scrollbar();
            if let Some(resp) = &view.last_response {
                col = col
                    .child(section_heading(
                        if zh {
                            "执行摘要"
                        } else {
                            "Execution Summary"
                        },
                        cx,
                    ))
                    .child(field_row(
                        "Final URL",
                        None,
                        div().text_xs().child(SharedString::from(resp.url.clone())),
                        cx,
                    ))
                    .child(field_row(
                        "Attempts",
                        None,
                        badge(resp.attempt_count.to_string(), Tone::Neutral, cx),
                        cx,
                    ))
                    .child(field_row(
                        "Elapsed",
                        None,
                        badge(format!("{} ms", resp.elapsed_ms), Tone::Accent, cx),
                        cx,
                    ))
                    .child(field_row(
                        "Body Size",
                        None,
                        badge(format!("{} bytes", resp.body_size_bytes), Tone::Neutral, cx),
                        cx,
                    ));
            }
            col.into_any_element()
        }
    };

    v_col()
        .size_full()
        .bg(to_hsla(p.surface))
        .child(
            h_row()
                .justify_between()
                .px(px(12.0))
                .pt(px(4.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(tabs)
                .child(metrics),
        )
        .child(div().flex_1().min_h_0().child(content))
}
