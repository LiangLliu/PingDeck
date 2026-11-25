//! Resizable Requests sidebar: unified `master_pane` header, search, agent-touched filter,
//! undo banner, collections/unfiled/AI tree, polished empty states, and footer.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::model::ai_request::RequestMode;
use pingdeck_desktop::core::model::{Folder, Request};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, btn_ghost, btn_icon, btn_primary,
    btn_secondary, empty_inline, filter_chip, h_row, icon_xs, master_pane, palette, pane_footer,
    pane_header, search_input, section_heading, to_hsla, v_col,
};

use super::RequestsView;
use super::model::{ImportExportTab, NewItemKind, method_str};
use super::sidebar_nodes::{render_collection_menu, render_folder_branch, render_request_row};

pub fn render_sidebar(view: &RequestsView, cx: &mut Context<RequestsView>) -> impl IntoElement {
    let p = palette(cx);
    let query = view.search_input.read(cx).value().trim().to_lowercase();
    let active_req_id = view.active_req().and_then(|t| t.saved_request_id.clone());

    let agent_touched_count = view
        .requests
        .iter()
        .filter(|r| r.agent_touched_ms.is_some())
        .count();

    let matches_req = |r: &Request| -> bool {
        if view.agent_touched_only && r.agent_touched_ms.is_none() {
            return false;
        }
        if query.is_empty() {
            return true;
        }
        r.name.to_lowercase().contains(&query)
            || r.url.to_lowercase().contains(&query)
            || method_str(r.method).to_lowercase().contains(&query)
    };

    let mut list = v_col()
        .size_full()
        .gap(px(2.0))
        .p(px(8.0))
        .overflow_y_scrollbar();

    // Undo deletion banner when a recent delete can be restored.
    if let Some((_, label)) = &view.last_undo {
        let lbl = label.clone();
        list = list.child(
            div().mb(px(6.0)).child(alert_banner(
                Tone::Warning,
                crate::ui::tr(cx).requests().sidebar_deleted(lbl),
                Some(
                    btn_ghost("req-undo-del", crate::ui::tr(cx).common().undo())
                        .on_click(cx.listener(|this, _, _, cx| this.undo_last_delete(cx)))
                        .into_any_element(),
                ),
                cx,
            )),
        );
    }

    let mut visible_total = 0usize;

    // 1. Collections → recursive nested Folders → Requests.
    for col in &view.collections {
        let col_reqs: Vec<&Request> = view
            .requests
            .iter()
            .filter(|r| {
                r.collection_id.as_ref().map(|c| c.as_str()) == Some(col.id.as_str())
                    && matches_req(r)
            })
            .collect();
        if col_reqs.is_empty() && (!query.is_empty() || view.agent_touched_only) {
            continue;
        }
        visible_total += col_reqs.len();
        let col_key = format!("col:{}", col.id);
        let collapsed = view.collapsed_nodes.contains(&col_key) && query.is_empty();
        let col_id_toggle = col_key.clone();
        let col_id_run = col.id.clone();
        let col_id_settings = col.id.clone();
        let col_id_menu = col.id.clone();
        let col_name_menu = col.name.clone();

        list = list.child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(4.0))
                .px(px(6.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
                .child(
                    div()
                        .id(SharedString::from(format!("col-tog-{}", col.id)))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(5.0))
                        .flex_1()
                        .min_w_0()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.collapsed_nodes.remove(&col_id_toggle) {
                                this.collapsed_nodes.insert(col_id_toggle.clone());
                            }
                            cx.notify();
                        }))
                        .child(icon_xs(if collapsed {
                            UiIcon::ChevronRight
                        } else {
                            UiIcon::ChevronDown
                        }))
                        .child(icon_xs(if collapsed {
                            UiIcon::Folder
                        } else {
                            UiIcon::FolderOpen
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(col.name.clone())),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .pr(px(2.0))
                                .child(SharedString::from(col_reqs.len().to_string())),
                        )
                        .child(
                            btn_icon(
                                SharedString::from(format!("col-run-{}", col.id)),
                                UiIcon::Play,
                            )
                            .tooltip(crate::ui::tr(cx).requests().sidebar_run_collection())
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_collection_runner_tab(
                                        col_id_run.clone(),
                                        None,
                                        window,
                                        cx,
                                    );
                                },
                            )),
                        )
                        .child(
                            btn_icon(
                                SharedString::from(format!("col-cfg-{}", col.id)),
                                UiIcon::Settings,
                            )
                            .tooltip(crate::ui::tr(cx).requests().coll_title_collection())
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_collection_settings_tab(
                                        col_id_settings.clone(),
                                        window,
                                        cx,
                                    );
                                },
                            )),
                        )
                        .child(render_collection_menu(col_id_menu, col_name_menu, cx)),
                ),
        );

        if !collapsed {
            let root_folders: Vec<&Folder> = view
                .folders
                .iter()
                .filter(|f| {
                    f.collection_id.as_str() == col.id.as_str() && f.parent_folder_id.is_none()
                })
                .collect();
            for folder in root_folders {
                let mut branch = v_col().w_full().gap(px(2.0));
                render_folder_branch(
                    &mut branch,
                    view,
                    folder,
                    1,
                    &query,
                    active_req_id.as_deref(),
                    &matches_req,
                    cx,
                );
                list = list.child(branch);
            }
            for req in col_reqs.iter().filter(|r| r.folder_id.is_none()) {
                list = list.child(render_request_row(
                    view,
                    req,
                    1,
                    active_req_id.as_deref(),
                    cx,
                ));
            }
        }
    }

    // 2. Unfiled requests (collection_id == None).
    let unfiled_http: Vec<&Request> = view
        .requests
        .iter()
        .filter(|r| r.collection_id.is_none() && r.mode == RequestMode::Http && matches_req(r))
        .collect();
    let unfiled_ai: Vec<&Request> = view
        .requests
        .iter()
        .filter(|r| r.collection_id.is_none() && r.mode == RequestMode::AiApi && matches_req(r))
        .collect();
    visible_total += unfiled_http.len() + unfiled_ai.len();

    if !unfiled_http.is_empty() {
        list = list.child(
            h_row()
                .justify_between()
                .px(px(8.0))
                .pt(px(10.0))
                .pb(px(3.0))
                .child(section_heading(
                    crate::ui::tr(cx).requests().sidebar_unfiled(),
                    cx,
                ))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(unfiled_http.len().to_string())),
                ),
        );
        for req in unfiled_http {
            list = list.child(render_request_row(
                view,
                req,
                0,
                active_req_id.as_deref(),
                cx,
            ));
        }
    }

    if !unfiled_ai.is_empty() {
        list = list.child(
            h_row()
                .justify_between()
                .px(px(8.0))
                .pt(px(10.0))
                .pb(px(3.0))
                .child(section_heading(
                    crate::ui::tr(cx).requests().sidebar_ai_requests(),
                    cx,
                ))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(unfiled_ai.len().to_string())),
                ),
        );
        for req in unfiled_ai {
            list = list.child(render_request_row(
                view,
                req,
                0,
                active_req_id.as_deref(),
                cx,
            ));
        }
    }

    // Empty / no-search-results states (centered with outlined action buttons).
    if view.collections.is_empty() && view.requests.is_empty() {
        list = list.child(empty_inline(
            crate::ui::tr(cx).requests().sidebar_no_collections_yet(),
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .requests()
                    .sidebar_create_a_collection_or_import(),
            )),
            Some(
                h_row()
                    .gap(px(6.0))
                    .flex_wrap()
                    .justify_center()
                    .child(
                        btn_secondary(
                            "req-empty-new-col",
                            crate::ui::tr(cx).requests().sidebar_new_col(),
                        )
                        .icon(icon_xs(UiIcon::Folder))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_new_item_modal(
                                NewItemKind::Collection,
                                None,
                                None,
                                window,
                                cx,
                            );
                        })),
                    )
                    .child(
                        btn_secondary(
                            "req-empty-import",
                            crate::ui::tr(cx).requests().state_empty_tree_import(),
                        )
                        .icon(icon_xs(UiIcon::Upload))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_import_export_modal(
                                ImportExportTab::Import,
                                None,
                                window,
                                cx,
                            );
                        })),
                    )
                    .into_any_element(),
            ),
            cx,
        ));
    } else if visible_total == 0 && (!query.is_empty() || view.agent_touched_only) {
        list = list.child(empty_inline(
            crate::ui::tr(cx).requests().sidebar_no_matching_requests(),
            None,
            Some(
                btn_secondary(
                    "req-clear-filter",
                    crate::ui::tr(cx).common().list_clear_filter(),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.agent_touched_only = false;
                    this.search_input
                        .update(cx, |i, cx| i.set_value("", window, cx));
                    cx.notify();
                }))
                .into_any_element(),
            ),
            cx,
        ));
    }

    let col_cnt = view.collections.len();
    let req_cnt = view.requests.len();
    let footer_stats = crate::ui::tr(cx)
        .requests()
        .sidebar_col_req(col_cnt, req_cnt);

    let mut filter_slot = v_col()
        .w_full()
        .gap(px(6.0))
        .child(search_input(&view.search_input, cx));
    if agent_touched_count > 0 {
        filter_slot = filter_slot.child(
            filter_chip(
                "req-agent-filter",
                crate::ui::tr(cx)
                    .requests()
                    .sidebar_agent_modified(agent_touched_count),
                Some(UiIcon::Sparkles),
                view.agent_touched_only,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.agent_touched_only = !this.agent_touched_only;
                cx.notify();
            })),
        );
    }

    let header = pane_header(
        crate::ui::tr(cx).requests().title(),
        Some(req_cnt),
        Some(
            btn_primary(
                "req-sidebar-new",
                crate::ui::tr(cx).requests().sidebar_new(),
            )
            .icon(icon_xs(UiIcon::Plus))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_new_item_modal(NewItemKind::HttpGet, None, None, window, cx);
            }))
            .into_any_element(),
        ),
        Some(filter_slot.into_any_element()),
        cx,
    );

    let footer = pane_footer(
        div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.muted))
            .truncate()
            .child(SharedString::from(footer_stats)),
        Some(
            div()
                .id("req-sidebar-io")
                .cursor_pointer()
                .text_size(px(11.5))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.accent_ink))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_import_export_modal(ImportExportTab::Import, None, window, cx);
                }))
                .child(crate::ui::tr(cx).requests().sidebar_import_export())
                .into_any_element(),
        ),
        cx,
    );

    master_pane(header, list, Some(footer.into_any_element()), cx)
}
