//! HTTP/AI Pre & Post Scripts editor (with live Boa compat lint) and Docs/Response Examples tab.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::script_compat_list_impl;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_ghost, btn_icon, btn_secondary,
    card, code_editor, empty_inline, h_row, markdown_view, meta_pill, multiline_input, palette,
    section_header, tab_bar_segmented, tab_item, to_hsla, v_col,
};

use super::RequestsView;
use super::model::ScriptSubTab;

pub fn render_scripts_tab(
    view: &RequestsView,
    zh: bool,
    is_ai: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let Some(tab) = view.active_req() else {
        return v_col();
    };
    let sub = tab.script_sub;
    let ent = cx.entity().clone();
    let source = if sub == ScriptSubTab::Pre {
        view.pre_script_editor.read(cx).value().to_string()
    } else {
        view.post_script_editor.read(cx).value().to_string()
    };
    let compat = script_compat_list_impl(&source);

    let seg = tab_bar_segmented("req-script-phase-seg")
        .selected_index(usize::from(sub == ScriptSubTab::Post))
        .child(tab_item(if zh {
            "请求前脚本 (Pre)"
        } else {
            "Pre-request"
        }))
        .child(tab_item(if zh {
            "响应后脚本 (Post)"
        } else {
            "Post-response"
        }))
        .on_click(move |idx, _, cx| {
            ent.update(cx, |this, cx| {
                if let Some(t) = this.active_req_mut() {
                    t.script_sub = if *idx == 0 {
                        ScriptSubTab::Pre
                    } else {
                        ScriptSubTab::Post
                    };
                }
                cx.notify();
            });
        });

    let snip_btn = |id: &'static str,
                    lbl: &'static str,
                    code: &'static str,
                    cx: &mut Context<RequestsView>| {
        btn_ghost(id, lbl).on_click(cx.listener(move |this, _, window, cx| {
            let target = match this.active_req().map(|t| t.script_sub) {
                Some(ScriptSubTab::Pre) => this.pre_script_editor.clone(),
                _ => this.post_script_editor.clone(),
            };
            let cur = target.read(cx).value().to_string();
            let next = if cur.trim().is_empty() {
                code.to_owned()
            } else {
                format!("{cur}\n{code}")
            };
            target.update(cx, |e, cx| e.set_value(next, window, cx));
            this.mark_active_dirty(cx);
        }))
    };

    let mut top = h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .child(seg)
        .child(
            h_row()
                .gap(px(4.0))
                .child(snip_btn(
                    "snip-status",
                    "+ status == 200",
                    "pm.test(\"Status is 200\", () => pm.expect(pm.response.code).to.eql(200));",
                    cx,
                ))
                .child(snip_btn(
                    "snip-env",
                    "+ set env",
                    "pm.environment.set(\"token\", pm.response.json().token);",
                    cx,
                ))
                .children(is_ai.then(|| {
                    snip_btn(
                        "snip-ttft",
                        "+ AI TTFT",
                        "pm.test(\"Fast TTFT\", () => pm.expect(pm.response.ai.ttftMs).to.be.below(1500));",
                        cx,
                    )
                })),
        );

    if compat.is_empty() {
        top = top.child(meta_pill("Boa sandbox", cx));
    }

    let mut col = v_col().size_full().p(px(10.0)).gap(px(8.0)).child(top);
    if !compat.is_empty() {
        let apis = compat
            .iter()
            .map(|c| format!("{} (L{})", c.api, c.line))
            .collect::<Vec<_>>()
            .join(", ");
        col = col.child(alert_banner(
            Tone::Warning,
            if zh {
                format!("沙箱不支持以下接口：{apis}")
            } else {
                format!("Unsupported in sandbox: {apis}")
            },
            None,
            cx,
        ));
    }

    col.child(
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(code_editor(if sub == ScriptSubTab::Pre {
                &view.pre_script_editor
            } else {
                &view.post_script_editor
            })),
    )
}

pub(super) fn render_docs_tab(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().into_any_element();
    };
    let preview = tab.docs_preview;
    let has_resp = tab.last_http.is_some();

    let mut root = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(10.0))
        .overflow_y_scrollbar()
        .child(section_header(
            if zh {
                "请求说明（Markdown）"
            } else {
                "Description (Markdown)"
            },
            Some(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_ghost(
                            "docs-toggle-prev",
                            if preview {
                                if zh { "编辑" } else { "Edit" }
                            } else if zh {
                                "预览"
                            } else {
                                "Preview"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.snapshot_active_tab(cx);
                            if let Some(t) = this.active_req_mut() {
                                t.docs_preview = !t.docs_preview;
                            }
                            cx.notify();
                        })),
                    )
                    .children(has_resp.then(|| {
                        btn_secondary(
                            "docs-save-ex",
                            if zh {
                                "存当前响应为示例"
                            } else {
                                "Save response as example"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.save_current_response_as_example(cx);
                        }))
                    }))
                    .into_any_element(),
            ),
            cx,
        ));

    if preview {
        let md = view.docs_input.read(cx).value().to_string();
        root = root.child(card(cx).p(px(12.0)).child(markdown_view(
            "req-docs-md",
            if md.is_empty() { "_无说明_" } else { &md },
        )));
    } else {
        root = root.child(div().h(px(130.0)).child(multiline_input(&view.docs_input)));
    }

    root = root.child(section_header(
        if zh {
            format!("已保存响应示例 ({}/20)", tab.draft.examples.len())
        } else {
            format!("Saved Response Examples ({}/20)", tab.draft.examples.len())
        },
        None,
        cx,
    ));

    if tab.draft.examples.is_empty() {
        root = root.child(empty_inline(
            if zh {
                "暂无响应示例"
            } else {
                "No saved examples yet"
            },
            Some(SharedString::from(if zh {
                "从 Postman / OpenAPI 导入的示例，或发送后手动保存的响应会列在这里。"
            } else {
                "Examples imported from Postman/OpenAPI or saved from a live response appear here."
            })),
            None,
            cx,
        ));
    } else {
        for (i, ex) in tab.draft.examples.iter().enumerate() {
            let expanded = tab.expanded_example_idx == Some(i);
            let preview_body: String = if expanded {
                ex.body.clone()
            } else {
                ex.body.chars().take(180).collect()
            };
            let tone = if ex.status < 300 {
                Tone::Success
            } else if ex.status < 500 {
                Tone::Warning
            } else {
                Tone::Danger
            };
            let mut ex_col = v_col().gap(px(6.0)).child(
                h_row()
                    .justify_between()
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(badge(ex.status.to_string(), tone, cx))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .child(SharedString::from(ex.name.clone())),
                            ),
                    )
                    .child(
                        h_row()
                            .gap(px(4.0))
                            .child(
                                btn_ghost(
                                    SharedString::from(format!("ex-exp-{i}")),
                                    if expanded {
                                        if zh { "收起" } else { "Collapse" }
                                    } else if zh {
                                        "展开详情"
                                    } else {
                                        "Expand"
                                    },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some(t) = this.active_req_mut() {
                                            t.expanded_example_idx =
                                                if t.expanded_example_idx == Some(i) {
                                                    None
                                                } else {
                                                    Some(i)
                                                };
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                btn_icon(SharedString::from(format!("ex-del-{i}")), UiIcon::Trash)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(t) = this.active_req_mut()
                                            && i < t.draft.examples.len()
                                        {
                                            t.draft.examples.remove(i);
                                            t.expanded_example_idx = None;
                                            t.recompute_dirty();
                                        }
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
            if expanded {
                if !ex.headers.is_empty() {
                    let hdrs: String = ex
                        .headers
                        .iter()
                        .map(|h| format!("{}: {}", h.key, h.value))
                        .collect::<Vec<_>>()
                        .join("\n");
                    ex_col = ex_col.child(
                        div()
                            .p(px(8.0))
                            .rounded(px(6.0))
                            .bg(to_hsla(p.pane))
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(hdrs)),
                    );
                }
                ex_col = ex_col.child(
                    div()
                        .p(px(8.0))
                        .rounded(px(6.0))
                        .bg(to_hsla(p.sunken))
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(preview_body)),
                );
            } else {
                ex_col = ex_col.child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(SharedString::from(preview_body)),
                );
            }
            root = root.child(card(cx).p(px(10.0)).child(ex_col));
        }
    }

    root.into_any_element()
}
