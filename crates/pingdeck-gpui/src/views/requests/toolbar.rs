//! Multi-tab document bar and Request Identity / HTTP-vs-AI Mode Strip.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::model::ai_request::RequestMode;

use crate::ui::{
    Anchor, ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_secondary,
    doc_tab, h_row, icon_xs, is_compact, menu_divider, menu_item, method_badge, palette, popover,
    tab_bar_segmented, tab_item, text_input, to_hsla, toolbar_strip, v_col,
};

use super::RequestsView;
use super::model::{ActiveModal, NewItemKind, OpenTab, RequestsHandoff, method_str};
pub use super::toolbar_url::render_http_url_strip;
use super::toolbar_url::render_variables_popover;

pub fn render_doc_tabbar(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut tabs_row = h_row()
        .flex_1()
        .min_w_0()
        .items_end()
        .gap(px(4.0))
        .overflow_x_scrollbar();

    for tab in &view.tabs {
        let tid = tab.tab_id().to_owned();
        let active = view.active_tab_id.as_deref() == Some(tid.as_str());
        let dirty = tab.is_dirty();
        let title = tab.title(zh);
        let tid_sel = tid.clone();
        let tid_x = tid.clone();

        let prefix: gpui_kit::AnyElement = match tab {
            OpenTab::Request(r) if r.is_ai() => badge("AI", Tone::Accent, cx).into_any_element(),
            OpenTab::Request(r) => method_badge(method_str(r.draft.method), cx)
                .w(px(40.0))
                .into_any_element(),
            OpenTab::Runner(_) => div()
                .text_color(to_hsla(p.accent_ink))
                .child(icon_xs(UiIcon::Play))
                .into_any_element(),
            OpenTab::Container(_) => div()
                .text_color(to_hsla(p.muted))
                .child(icon_xs(UiIcon::Settings))
                .into_any_element(),
        };

        let close_btn = btn_icon(SharedString::from(format!("tab-x-{tid_x}")), UiIcon::Close)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.request_close_tab(&tid_x, window, cx);
            }))
            .into_any_element();

        tabs_row = tabs_row.child(doc_tab(
            SharedString::from(format!("doc-tab-{tid}")),
            active,
            dirty,
            prefix,
            title,
            close_btn,
            cx,
            cx.listener(move |this, _, window, cx| {
                this.switch_to_tab(&tid_sel, window, cx);
            }),
        ));
    }

    // Place the `+` button right beside the rightmost tab (not stranded at the far right edge).
    tabs_row = tabs_row.child(
        div().pb(px(3.0)).flex_shrink_0().child(
            btn_icon("doc-tab-plus", UiIcon::Plus)
                .tooltip(if zh {
                    "新建请求标签 (⌘T)"
                } else {
                    "New tab (⌘T)"
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_new_request_tab(NewItemKind::HttpGet, None, None, window, cx);
                })),
        ),
    );

    h_row()
        .w_full()
        .h(px(if is_compact(cx) { 40.0 } else { 44.0 }))
        .items_end()
        .flex_shrink_0()
        .px(px(8.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(tabs_row)
}

/// Compact Request Identity, Breadcrumb, HTTP ⇄ AI Mode switch, Variables, Code, Save & More strip.
pub fn render_identity_strip(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return div();
    };
    let is_ai = tab.is_ai();
    let is_dirty = tab.dirty;
    let is_saved = tab.saved_request_id.is_some();
    let agent_touched = tab.draft.agent_touched_ms.is_some();
    let col_id_for_bc = tab
        .draft
        .collection_id
        .as_ref()
        .map(|c| c.as_str().to_owned());

    let breadcrumb = {
        let mut parts = Vec::new();
        if let Some(cid) = &tab.draft.collection_id
            && let Some(c) = view.collections.iter().find(|c| c.id == cid.as_str())
        {
            parts.push(c.name.clone());
        }
        if let Some(fid) = &tab.draft.folder_id
            && let Some(f) = view.folders.iter().find(|f| f.id.as_str() == fid.as_str())
        {
            parts.push(f.name.clone());
        }
        if parts.is_empty() {
            if zh { "草稿 /" } else { "Draft /" }.to_owned()
        } else {
            format!("{} /", parts.join(" / "))
        }
    };

    let ent_mode = cx.entity().clone();
    let mode_seg = tab_bar_segmented("req-mode-seg")
        .selected_index(usize::from(is_ai))
        .child(tab_item("HTTP"))
        .child(tab_item("AI API"))
        .on_click(move |idx, window, cx| {
            let target = if *idx == 1 {
                RequestMode::AiApi
            } else {
                RequestMode::Http
            };
            ent_mode.update(cx, |this, cx| {
                this.switch_active_request_mode(target, window, cx);
            });
        });

    let ent_more = cx.entity().clone();
    let req_id_opt = tab.saved_request_id.clone();
    let req_name_del = tab.draft.name.clone();
    let more_menu = popover("req-more-pop")
        .anchor(Anchor::TopRight)
        .trigger(btn_icon("req-more-btn", UiIcon::Ellipsis))
        .content(move |_, _, pop_cx| {
            let ph = pop_cx.entity().clone();
            let mut m = v_col().w(px(184.0)).gap(px(2.0));

            let e1 = ent_more.clone();
            let ph1 = ph.clone();
            m = m.child(menu_item(
                "req-more-saveas",
                UiIcon::Folder,
                if zh { "另存为…" } else { "Save as…" },
                false,
                pop_cx,
                move |_, window, cx| {
                    e1.update(cx, |this, cx| {
                        this.snapshot_active_tab(cx);
                        let cur_name = this
                            .active_req()
                            .map(|t| t.draft.name.clone())
                            .unwrap_or_default();
                        this.focus_modal_name_input(cur_name, window, cx);
                        this.active_modal = Some(ActiveModal::SaveAs {
                            target_collection_id: this
                                .active_req()
                                .and_then(|t| {
                                    t.draft
                                        .collection_id
                                        .as_ref()
                                        .map(|c| c.as_str().to_owned())
                                })
                                .or_else(|| this.collections.first().map(|c| c.id.clone())),
                            target_folder_id: this.active_req().and_then(|t| {
                                t.draft.folder_id.as_ref().map(|f| f.as_str().to_owned())
                            }),
                        });
                        cx.notify();
                    });
                    ph1.update(cx, |s, cx| s.dismiss(window, cx));
                },
            ));

            let e2 = ent_more.clone();
            let ph2 = ph.clone();
            m = m.child(menu_item(
                "req-more-download",
                UiIcon::Download,
                if zh {
                    "发送并保存到文件…"
                } else {
                    "Send & save to file…"
                },
                false,
                pop_cx,
                move |_, window, cx| {
                    e2.update(cx, |this, cx| this.send_and_save_to_file(window, cx));
                    ph2.update(cx, |s, cx| s.dismiss(window, cx));
                },
            ));

            let e3 = ent_more.clone();
            let ph3 = ph.clone();
            m = m.child(menu_item(
                "req-more-hist",
                UiIcon::History,
                if zh {
                    "查看执行历史 ↗"
                } else {
                    "View history ↗"
                },
                false,
                pop_cx,
                move |_, window, cx| {
                    e3.update(cx, |this, cx| {
                        this.emit_handoff(RequestsHandoff::OpenHistory, cx);
                    });
                    ph3.update(cx, |s, cx| s.dismiss(window, cx));
                },
            ));

            if let Some(rid) = req_id_opt.clone() {
                let e4 = ent_more.clone();
                let ph4 = ph.clone();
                let rn = req_name_del.clone();
                m = m.child(menu_divider(pop_cx)).child(menu_item(
                    "req-more-del",
                    UiIcon::Trash,
                    if zh { "删除请求" } else { "Delete" },
                    true,
                    pop_cx,
                    move |_, window, cx| {
                        e4.update(cx, |this, cx| {
                            this.delete_tree_item(
                                super::model::TreeItemKind::Request,
                                rid.clone(),
                                rn.clone(),
                                cx,
                            );
                        });
                        ph4.update(cx, |s, cx| s.dismiss(window, cx));
                    },
                ));
            }
            m
        });

    let bc_el: gpui_kit::AnyElement = if let Some(cid) = col_id_for_bc {
        div()
            .id("req-bc-col")
            .text_size(px(11.0))
            .text_color(to_hsla(p.faint))
            .flex_shrink_0()
            .cursor_pointer()
            .hover(|s| s.text_color(to_hsla(p.accent_ink)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_collection_settings_tab(cid.clone(), window, cx);
            }))
            .child(SharedString::from(breadcrumb))
            .into_any_element()
    } else {
        div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.faint))
            .flex_shrink_0()
            .child(SharedString::from(breadcrumb))
            .into_any_element()
    };

    toolbar_strip(cx)
        .justify_between()
        .py(px(6.0))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(6.0))
                .child(bc_el)
                .child(div().w(px(220.0)).child(text_input(&view.name_input)))
                .children(
                    agent_touched.then(|| badge(if zh { "AI 改" } else { "AI" }, Tone::Accent, cx)),
                )
                .child(if is_dirty {
                    badge(if zh { "未保存" } else { "Unsaved" }, Tone::Warning, cx)
                } else {
                    badge(if zh { "已保存" } else { "Saved" }, Tone::Success, cx)
                }),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .flex_shrink_0()
                .child(mode_seg)
                .child(render_variables_popover(view, zh, cx))
                .child(
                    btn_ghost("req-code-btn", if zh { "代码" } else { "Code" })
                        .icon(icon_xs(UiIcon::FileCode))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_code_snippets_modal(window, cx);
                        })),
                )
                .child(
                    btn_secondary(
                        "req-save-btn",
                        if zh {
                            if is_saved { "保存" } else { "保存到…" }
                        } else if is_saved {
                            "Save"
                        } else {
                            "Save to…"
                        },
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if is_saved {
                            this.save_active_tab(false, window, cx);
                        } else {
                            this.snapshot_active_tab(cx);
                            let cur_name = this
                                .active_req()
                                .map(|t| t.draft.name.clone())
                                .unwrap_or_default();
                            this.focus_modal_name_input(cur_name, window, cx);
                            this.active_modal = Some(ActiveModal::SaveAs {
                                target_collection_id: this
                                    .collections
                                    .first()
                                    .map(|c| c.id.clone()),
                                target_folder_id: None,
                            });
                            cx.notify();
                        }
                    })),
                )
                .child(more_menu),
        )
}
