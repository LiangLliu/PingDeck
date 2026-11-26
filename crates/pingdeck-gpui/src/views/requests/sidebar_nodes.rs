//! Recursive folder/request tree rows and per-node context menus for the Requests sidebar.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::model::ai_request::RequestMode;
use pingdeck_desktop::core::model::{Folder, Request};

use crate::ui::{
    Anchor, Popover, Tone, UiIcon, badge, btn_icon, h_row, icon_xs, list_row, menu_divider,
    menu_item, method_badge, palette, popover, to_hsla, v_col,
};

use super::RequestsView;
use super::model::{ActiveModal, NewItemKind, OpenTab, RequestsHandoff, TreeItemKind, method_str};

#[allow(clippy::too_many_arguments)]
pub(super) fn render_folder_branch(
    list: &mut gpui_kit::Div,
    view: &RequestsView,
    folder: &Folder,
    depth: usize,
    query: &str,
    active_req_id: Option<&str>,
    matches_req: &dyn Fn(&Request) -> bool,
    cx: &mut Context<RequestsView>,
) {
    let p = palette(cx);
    let fid = folder.id.as_str().to_owned();
    let fkey = format!("fld:{fid}");
    let collapsed = view.collapsed_nodes.contains(&fkey) && query.is_empty();
    let indent = px((depth as f32) * 14.0 + 6.0);

    let child_folders: Vec<&Folder> = view
        .folders
        .iter()
        .filter(|f| f.parent_folder_id.as_ref().map(|p| p.as_str()) == Some(fid.as_str()))
        .collect();
    let direct_reqs: Vec<&Request> = view
        .requests
        .iter()
        .filter(|r| {
            r.folder_id.as_ref().map(|f| f.as_str()) == Some(fid.as_str()) && matches_req(r)
        })
        .collect();

    let fkey_tog = fkey.clone();
    let fid_menu = fid.clone();
    let col_id_menu = folder.collection_id.as_str().to_owned();
    let fname_menu = folder.name.clone();

    *list = std::mem::replace(list, v_col()).child(
        h_row()
            .w_full()
            .justify_between()
            .pl(indent)
            .pr(px(4.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
            .child(
                div()
                    .id(SharedString::from(format!("fld-tog-{fid}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.0))
                    .flex_1()
                    .min_w_0()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed_nodes.remove(&fkey_tog) {
                            this.collapsed_nodes.insert(fkey_tog.clone());
                        }
                        cx.notify();
                    }))
                    .child(icon_xs(if collapsed {
                        UiIcon::ChevronRight
                    } else {
                        UiIcon::ChevronDown
                    }))
                    .child(icon_xs(UiIcon::Folder))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(SharedString::from(folder.name.clone())),
                    ),
            )
            .child(render_folder_menu(fid_menu, col_id_menu, fname_menu, cx)),
    );

    if !collapsed && depth < 6 {
        for sub in child_folders {
            render_folder_branch(
                list,
                view,
                sub,
                depth + 1,
                query,
                active_req_id,
                matches_req,
                cx,
            );
        }
        for req in direct_reqs {
            *list = std::mem::replace(list, v_col()).child(render_request_row(
                view,
                req,
                depth + 1,
                active_req_id,
                cx,
            ));
        }
    }
}

pub(super) fn render_request_row(
    view: &RequestsView,
    req: &Request,
    depth: usize,
    active_req_id: Option<&str>,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let rid = req.id.as_str().to_owned();
    let sel = active_req_id == Some(rid.as_str());
    let is_dirty = view.tabs.iter().any(|t| match t {
        OpenTab::Request(r) => r.saved_request_id.as_deref() == Some(rid.as_str()) && r.dirty,
        _ => false,
    });
    let req_open = req.clone();
    let req_menu = req.clone();
    let indent = px((depth as f32) * 14.0 + 10.0);

    let method_el: gpui_kit::AnyElement = if req.mode == RequestMode::AiApi {
        div()
            .w(px(36.0))
            .flex_shrink_0()
            .text_size(px(10.5))
            .font_weight(gpui_kit::FontWeight::BOLD)
            .text_color(to_hsla(p.accent_ink))
            .child("AI")
            .into_any_element()
    } else {
        method_badge(method_str(req.method), cx)
            .w(px(42.0))
            .flex_shrink_0()
            .into_any_element()
    };

    h_row()
        .w_full()
        .gap(px(2.0))
        .child(
            list_row(
                SharedString::from(format!("req-item-{rid}")),
                sel,
                cx,
                cx.listener(move |this, _, window, cx| {
                    this.open_request_tab(&req_open, window, cx);
                }),
            )
            .flex_1()
            .min_w_0()
            .pl(indent)
            .child(method_el)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .truncate()
                    .child(SharedString::from(req.name.clone())),
            )
            .children(req.agent_touched_ms.is_some().then(|| {
                badge(
                    crate::ui::tr(cx).requests().sidebar_nodes_ai(),
                    Tone::Accent,
                    cx,
                )
            }))
            .children(is_dirty.then(|| {
                div()
                    .size(px(6.0))
                    .rounded(px(999.0))
                    .bg(to_hsla(p.accent))
                    .flex_shrink_0()
            })),
        )
        .child(render_request_menu(req_menu, cx))
}

pub(super) fn render_collection_menu(
    col_id: String,
    col_name: String,
    cx: &mut Context<RequestsView>,
) -> Popover {
    let ent = cx.entity().clone();
    popover(SharedString::from(format!("col-menu-pop-{col_id}")))
        .anchor(Anchor::TopRight)
        .trigger(btn_icon(
            SharedString::from(format!("col-menu-btn-{col_id}")),
            UiIcon::Ellipsis,
        ))
        .content(move |_, _, pop_cx| {
            let pop_h = pop_cx.entity().clone();
            let mut m = v_col().w(px(174.0)).gap(px(2.0));
            let r = crate::ui::tr(pop_cx).requests();
            let items: Vec<(&str, UiIcon, usize, bool)> = vec![
                (r.menu_new_request(), UiIcon::Plus, 0, false),
                (r.menu_new_folder(), UiIcon::Folder, 1, false),
                (r.menu_run_collection(), UiIcon::Play, 2, false),
                (r.menu_collection_settings(), UiIcon::Settings, 3, false),
                (r.menu_export_postman(), UiIcon::Download, 4, false),
                (r.menu_duplicate_collection(), UiIcon::Copy, 5, false),
                (r.menu_rename(), UiIcon::Pencil, 6, false),
                (r.menu_delete_collection(), UiIcon::Trash, 7, true),
            ];
            for (lbl, ic, act, danger) in items {
                if danger {
                    m = m.child(menu_divider(pop_cx));
                }
                let ent_c = ent.clone();
                let cid = col_id.clone();
                let cname = col_name.clone();
                let ph = pop_h.clone();
                m = m.child(menu_item(
                    SharedString::from(format!("col-act-{col_id}-{act}")),
                    ic,
                    lbl,
                    danger,
                    pop_cx,
                    move |_, window, cx| {
                        ent_c.update(cx, |this, cx| match act {
                            0 => this.open_new_request_tab(
                                NewItemKind::HttpGet,
                                Some(cid.clone()),
                                None,
                                window,
                                cx,
                            ),
                            1 => this.open_new_item_modal(
                                NewItemKind::Folder,
                                Some(cid.clone()),
                                None,
                                window,
                                cx,
                            ),
                            2 => this.open_collection_runner_tab(cid.clone(), None, window, cx),
                            3 => this.open_collection_settings_tab(cid.clone(), window, cx),
                            4 => {
                                this.pending_handoff
                                    .set(Some(RequestsHandoff::OpenImportExport {
                                        export_collection_id: Some(cid.clone()),
                                    }));
                                cx.notify();
                            }
                            5 => {
                                this.duplicate_tree_item(TreeItemKind::Collection, cid.clone(), cx)
                            }
                            6 => {
                                this.focus_modal_name_input(cname.clone(), window, cx);
                                this.active_modal = Some(ActiveModal::Rename {
                                    kind: TreeItemKind::Collection,
                                    id: cid.clone(),
                                });
                                cx.notify();
                            }
                            _ => this.delete_tree_item(
                                TreeItemKind::Collection,
                                cid.clone(),
                                cname.clone(),
                                cx,
                            ),
                        });
                        ph.update(cx, |s, cx| s.dismiss(window, cx));
                    },
                ));
            }
            m
        })
}

fn render_folder_menu(
    folder_id: String,
    col_id: String,
    folder_name: String,
    cx: &mut Context<RequestsView>,
) -> Popover {
    let ent = cx.entity().clone();
    popover(SharedString::from(format!("fld-menu-pop-{folder_id}")))
        .anchor(Anchor::TopRight)
        .trigger(btn_icon(
            SharedString::from(format!("fld-menu-btn-{folder_id}")),
            UiIcon::Ellipsis,
        ))
        .content(move |_, _, pop_cx| {
            let pop_h = pop_cx.entity().clone();
            let mut m = v_col().w(px(164.0)).gap(px(2.0));
            let r = crate::ui::tr(pop_cx).requests();
            let items: Vec<(&str, UiIcon, usize, bool)> = vec![
                (r.menu_new_request(), UiIcon::Plus, 0, false),
                (r.menu_new_subfolder(), UiIcon::Folder, 1, false),
                (r.menu_run_folder(), UiIcon::Play, 2, false),
                (r.menu_folder_settings(), UiIcon::Settings, 3, false),
                (r.menu_duplicate_folder(), UiIcon::Copy, 4, false),
                (r.menu_rename(), UiIcon::Pencil, 5, false),
                (r.menu_delete_folder(), UiIcon::Trash, 6, true),
            ];
            for (lbl, ic, act, danger) in items {
                if danger {
                    m = m.child(menu_divider(pop_cx));
                }
                let ent_c = ent.clone();
                let fid = folder_id.clone();
                let cid = col_id.clone();
                let fname = folder_name.clone();
                let ph = pop_h.clone();
                m = m.child(menu_item(
                    SharedString::from(format!("fld-act-{folder_id}-{act}")),
                    ic,
                    lbl,
                    danger,
                    pop_cx,
                    move |_, window, cx| {
                        ent_c.update(cx, |this, cx| match act {
                            0 => this.open_new_request_tab(
                                NewItemKind::HttpGet,
                                Some(cid.clone()),
                                Some(fid.clone()),
                                window,
                                cx,
                            ),
                            1 => this.open_new_item_modal(
                                NewItemKind::Folder,
                                Some(cid.clone()),
                                Some(fid.clone()),
                                window,
                                cx,
                            ),
                            2 => this.open_collection_runner_tab(
                                cid.clone(),
                                Some(fid.clone()),
                                window,
                                cx,
                            ),
                            3 => this.open_folder_settings_tab(fid.clone(), window, cx),
                            4 => this.duplicate_tree_item(TreeItemKind::Folder, fid.clone(), cx),
                            5 => {
                                this.focus_modal_name_input(fname.clone(), window, cx);
                                this.active_modal = Some(ActiveModal::Rename {
                                    kind: TreeItemKind::Folder,
                                    id: fid.clone(),
                                });
                                cx.notify();
                            }
                            _ => this.delete_tree_item(
                                TreeItemKind::Folder,
                                fid.clone(),
                                fname.clone(),
                                cx,
                            ),
                        });
                        ph.update(cx, |s, cx| s.dismiss(window, cx));
                    },
                ));
            }
            m
        })
}

fn render_request_menu(req: Request, cx: &mut Context<RequestsView>) -> Popover {
    let rid = req.id.as_str().to_owned();
    let ent = cx.entity().clone();
    popover(SharedString::from(format!("req-menu-pop-{rid}")))
        .anchor(Anchor::TopRight)
        .trigger(btn_icon(
            SharedString::from(format!("req-menu-btn-{rid}")),
            UiIcon::Ellipsis,
        ))
        .content(move |_, _, pop_cx| {
            let pop_h = pop_cx.entity().clone();
            let mut m = v_col().w(px(156.0)).gap(px(2.0));
            let r = crate::ui::tr(pop_cx).requests();
            let items: Vec<(&str, UiIcon, usize, bool)> = vec![
                (r.menu_copy_curl(), UiIcon::FileCode, 0, false),
                (r.menu_duplicate_request(), UiIcon::Copy, 1, false),
                (r.menu_move_to(), UiIcon::Folder, 2, false),
                (r.menu_rename(), UiIcon::Pencil, 3, false),
                (r.menu_delete_request(), UiIcon::Trash, 4, true),
            ];
            for (lbl, ic, act, danger) in items {
                if danger {
                    m = m.child(menu_divider(pop_cx));
                }
                let ent_c = ent.clone();
                let req_c = req.clone();
                let ph = pop_h.clone();
                m = m.child(menu_item(
                    SharedString::from(format!("req-act-{}-{act}", req_c.id.as_str())),
                    ic,
                    lbl,
                    danger,
                    pop_cx,
                    move |_, window, cx| {
                        let id = req_c.id.as_str().to_owned();
                        ent_c.update(cx, |this, cx| match act {
                            0 => this.copy_request_as_curl(&req_c, cx),
                            1 => this.duplicate_tree_item(TreeItemKind::Request, id, cx),
                            2 => {
                                this.active_modal = Some(ActiveModal::MoveRequest {
                                    request_id: id,
                                    target_collection_id: req_c
                                        .collection_id
                                        .as_ref()
                                        .map(|c| c.as_str().to_owned()),
                                    target_folder_id: req_c
                                        .folder_id
                                        .as_ref()
                                        .map(|f| f.as_str().to_owned()),
                                });
                                cx.notify();
                            }
                            3 => {
                                this.focus_modal_name_input(req_c.name.clone(), window, cx);
                                this.active_modal = Some(ActiveModal::Rename {
                                    kind: TreeItemKind::Request,
                                    id,
                                });
                                cx.notify();
                            }
                            _ => this.delete_tree_item(
                                TreeItemKind::Request,
                                id,
                                req_c.name.clone(),
                                cx,
                            ),
                        });
                        ph.update(cx, |s, cx| s.dismiss(window, cx));
                    },
                ));
            }
            m
        })
}
