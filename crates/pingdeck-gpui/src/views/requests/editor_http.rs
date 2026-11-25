//! HTTP Request Editor tabs: tab bar, Params (Path + Query), and Headers (inherited + explicit).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::request_model_service::{AuthPreviewLocation, AuthSource};
use pingdeck_desktop::core::model::{KeyValue, RawLanguage, RequestBody};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_secondary, checkbox,
    data_row, h_row, meta_pill, palette, section_heading, subtab_strip, tab_bar_underline,
    tab_item, table_header, text_input, to_hsla, v_col,
};

use super::RequestsView;
use super::editor_auth_net::{render_assertions_tab, render_auth_tab, render_network_tab};
use super::editor_body::render_body_tab;
use super::editor_scripts_docs::render_docs_tab;
pub use super::editor_scripts_docs::render_scripts_tab;
use super::model::HttpEdTab;

pub fn render_http_editor(view: &RequestsView, cx: &mut Context<RequestsView>) -> impl IntoElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().size_full();
    };
    let ent = cx.entity().clone();

    let params_cnt = tab.draft.query_params.len() + tab.draft.path_variables.len();
    let hdr_cnt = tab.draft.headers.len();
    let assert_cnt = tab.draft.assertions.len();

    let r = crate::ui::tr(cx).requests();
    let lbl = |base: &str, cnt: usize| -> String {
        if cnt > 0 {
            format!("{base} ({cnt})")
        } else {
            base.to_owned()
        }
    };

    let tabs = tab_bar_underline("req-http-ed-tabs")
        .selected_index(tab.http_ed_tab.index())
        .child(tab_item(lbl(r.editor_http_tab_params(), params_cnt)))
        .child(tab_item(lbl(r.editor_http_tab_headers(), hdr_cnt)))
        .child(tab_item(r.coll_tab_auth()))
        .child(tab_item(r.editor_tabs_body()))
        .child(tab_item(lbl(r.editor_http_tab_assertions(), assert_cnt)))
        .child(tab_item(r.coll_tab_scripts()))
        .child(tab_item(r.editor_tabs_network()))
        .child(tab_item(r.editor_tabs_docs()))
        .on_click(move |idx, _, cx| {
            let next = HttpEdTab::from_index(*idx);
            ent.update(cx, |this, cx| {
                this.snapshot_active_tab(cx);
                if let Some(t) = this.active_req_mut() {
                    t.http_ed_tab = next;
                }
                if next == HttpEdTab::Auth {
                    this.refresh_effective_auth(cx);
                    this.oauth_refresh_status(cx);
                }
                cx.notify();
            });
        });

    let auth_hint = tab
        .effective_auth
        .as_ref()
        .map(|ea| match &ea.source {
            AuthSource::Collection { name, .. } => crate::ui::tr(cx)
                .requests()
                .editor_http_auth_inherited(name),
            AuthSource::Folder { name, .. } => crate::ui::tr(cx)
                .requests()
                .editor_http_auth_inherited_2(name),
            AuthSource::Request => crate::ui::tr(cx)
                .requests()
                .editor_http_auth_request()
                .into(),
            AuthSource::None => crate::ui::tr(cx).requests().editor_http_auth_none().into(),
        })
        .unwrap_or_default();

    let content: gpui_kit::AnyElement = match tab.http_ed_tab {
        HttpEdTab::Params => render_params_tab(view, cx).into_any_element(),
        HttpEdTab::Headers => render_headers_tab(view, cx).into_any_element(),
        HttpEdTab::Auth => render_auth_tab(view, cx).into_any_element(),
        HttpEdTab::Body => render_body_tab(view, cx).into_any_element(),
        HttpEdTab::Assertions => render_assertions_tab(view, false, cx).into_any_element(),
        HttpEdTab::Scripts => render_scripts_tab(view, false, cx).into_any_element(),
        HttpEdTab::Network => render_network_tab(view, cx).into_any_element(),
        HttpEdTab::Docs => render_docs_tab(view, cx).into_any_element(),
    };

    let auth_pill = (!auth_hint.is_empty()).then(|| meta_pill(auth_hint, cx).into_any_element());

    v_col()
        .size_full()
        .bg(to_hsla(p.surface))
        .child(subtab_strip(tabs, auth_pill, cx))
        .child(div().flex_1().min_h_0().w_full().child(content))
}

fn render_params_tab(view: &RequestsView, cx: &mut Context<RequestsView>) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().into_any_element();
    };
    let mut root = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(10.0))
        .overflow_y_scrollbar();

    // 1. Path variables extracted automatically from URL (`:id` or `{id}`).
    if !tab.draft.path_variables.is_empty() {
        let mut pv_box = v_col().gap(px(4.0)).child(section_heading(
            crate::ui::tr(cx).requests().editor_http_path_vars_heading(),
            cx,
        ));
        for (idx, pv) in tab.draft.path_variables.iter().enumerate() {
            pv_box = pv_box.child(
                data_row(cx)
                    .child(badge(format!(":{}", pv.key), Tone::Accent, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .child(SharedString::from(if pv.value.is_empty() {
                                crate::ui::tr(cx).requests().editor_http_empty().to_owned()
                            } else {
                                pv.value.clone()
                            })),
                    )
                    .child(
                        btn_ghost(
                            SharedString::from(format!("pv-set-{idx}")),
                            crate::ui::tr(cx).requests().editor_http_set_from_value(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let v = this.kv_val_input.read(cx).value().to_string();
                            if let Some(t) = this.active_req_mut()
                                && let Some(row) = t.draft.path_variables.get_mut(idx)
                            {
                                row.value = v;
                                t.recompute_dirty();
                            }
                            cx.notify();
                        })),
                    ),
            );
        }
        root = root.child(pv_box);
    }

    // 2. Query Parameters (synced bidirectionally with URL bar).
    root = root.child(section_heading(
        crate::ui::tr(cx)
            .requests()
            .editor_http_query_parameters_synced_with_url(),
        cx,
    ));
    root = root.child(render_kv_adder(view, "param", true, cx));
    root = root.child(render_kv_rows(&tab.draft.query_params, "param", true, cx));
    root.into_any_element()
}

fn render_headers_tab(view: &RequestsView, cx: &mut Context<RequestsView>) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().into_any_element();
    };
    let mut root = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(8.0))
        .overflow_y_scrollbar();

    // Inherited / automatic headers preview (read-only transparency rows matching prototype).
    let mut auto_rows = Vec::new();
    if let Some(ea) = &tab.effective_auth
        && let Some(prev) = &ea.preview
        && prev.location == AuthPreviewLocation::Header
    {
        auto_rows.push((
            prev.name.clone(),
            prev.value.clone(),
            crate::ui::tr(cx).requests().editor_http_auth(),
            Tone::Accent,
        ));
    }
    if let Some(ct) = implicit_content_type(&tab.draft.body) {
        auto_rows.push((
            "Content-Type".into(),
            ct.into(),
            crate::ui::tr(cx).requests().editor_network_auto(),
            Tone::Neutral,
        ));
    }

    if !auto_rows.is_empty() {
        let mut auto_box = v_col().gap(px(3.0)).child(section_heading(
            crate::ui::tr(cx)
                .requests()
                .editor_http_implicit_inherited_headers(),
            cx,
        ));
        for (k, v, tag, tone) in auto_rows {
            auto_box = auto_box.child(
                h_row()
                    .w_full()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(5.0))
                    .bg(to_hsla(p.sunken))
                    .child(
                        div()
                            .w(px(190.0))
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(SharedString::from(k)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(v)),
                    )
                    .child(badge(tag, tone, cx)),
            );
        }
        root = root.child(auto_box);
    }

    root = root.child(section_heading(
        crate::ui::tr(cx).requests().editor_http_explicit_headers(),
        cx,
    ));
    root = root.child(render_kv_adder(view, "hdr", false, cx));
    root = root.child(render_kv_rows(&tab.draft.headers, "hdr", false, cx));
    root.into_any_element()
}

fn render_kv_adder(
    view: &RequestsView,
    prefix: &'static str,
    is_param: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    h_row()
        .w_full()
        .gap(px(6.0))
        .child(div().w(px(180.0)).child(text_input(&view.kv_key_input)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(text_input(&view.kv_val_input)),
        )
        .child(div().w(px(150.0)).child(text_input(&view.kv_desc_input)))
        .child(
            btn_secondary(
                SharedString::from(format!("kv-add-{prefix}")),
                crate::ui::tr(cx).requests().container_settings_add(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                let key = this.kv_key_input.read(cx).value().trim().to_owned();
                let value = this.kv_val_input.read(cx).value().to_string();
                let desc = this.kv_desc_input.read(cx).value().trim().to_owned();
                if key.is_empty() {
                    return;
                }
                let row = KeyValue {
                    key,
                    value,
                    enabled: true,
                    description: (!desc.is_empty()).then_some(desc),
                };
                if let Some(tab) = this.active_req_mut() {
                    if is_param {
                        tab.draft.query_params.push(row);
                    } else {
                        tab.draft.headers.push(row);
                    }
                    tab.recompute_dirty();
                }
                this.kv_key_input
                    .update(cx, |i, cx| i.set_value("", window, cx));
                this.kv_val_input
                    .update(cx, |i, cx| i.set_value("", window, cx));
                this.kv_desc_input
                    .update(cx, |i, cx| i.set_value("", window, cx));
                if is_param {
                    this.sync_url_from_params(window, cx);
                } else {
                    cx.notify();
                }
            })),
        )
}

fn render_kv_rows(
    rows: &[KeyValue],
    prefix: &'static str,
    is_param: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut list = v_col().w_full().gap(px(4.0));
    if !rows.is_empty() {
        list = list.child(table_header(
            &[
                ("", Some(22.0)),
                (crate::ui::tr(cx).requests().editor_kv_key(), Some(180.0)),
                (crate::ui::tr(cx).common().secret_value_placeholder(), None),
                (
                    crate::ui::tr(cx).requests().editor_kv_description(),
                    Some(140.0),
                ),
                ("", Some(28.0)),
            ],
            cx,
        ));
    }
    for (idx, item) in rows.iter().enumerate() {
        let fg = if item.enabled {
            to_hsla(p.ink)
        } else {
            to_hsla(p.faint)
        };
        list = list.child(
            data_row(cx)
                .child(
                    checkbox(
                        SharedString::from(format!("kv-chk-{prefix}-{idx}")),
                        item.enabled,
                    )
                    .on_click(cx.listener(
                        move |this, checked: &bool, window, cx| {
                            if let Some(tab) = this.active_req_mut() {
                                let list = if is_param {
                                    &mut tab.draft.query_params
                                } else {
                                    &mut tab.draft.headers
                                };
                                if let Some(r) = list.get_mut(idx) {
                                    r.enabled = *checked;
                                }
                                tab.recompute_dirty();
                            }
                            if is_param {
                                this.sync_url_from_params(window, cx);
                            } else {
                                cx.notify();
                            }
                        },
                    )),
                )
                .child(
                    div()
                        .w(px(180.0))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(fg)
                        .truncate()
                        .child(SharedString::from(item.key.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(if item.enabled {
                            to_hsla(p.ink_2)
                        } else {
                            to_hsla(p.faint)
                        })
                        .truncate()
                        .child(SharedString::from(item.value.clone())),
                )
                .children(item.description.as_ref().map(|d| {
                    div()
                        .w(px(140.0))
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(SharedString::from(d.clone()))
                }))
                .child(
                    btn_icon(
                        SharedString::from(format!("kv-del-{prefix}-{idx}")),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(tab) = this.active_req_mut() {
                            let list = if is_param {
                                &mut tab.draft.query_params
                            } else {
                                &mut tab.draft.headers
                            };
                            if idx < list.len() {
                                list.remove(idx);
                            }
                            tab.recompute_dirty();
                        }
                        if is_param {
                            this.sync_url_from_params(window, cx);
                        } else {
                            cx.notify();
                        }
                    })),
                ),
        );
    }
    list
}

fn implicit_content_type(body: &RequestBody) -> Option<&'static str> {
    match body {
        RequestBody::Json { .. } | RequestBody::Graphql { .. } => Some("application/json"),
        RequestBody::RawText { language, .. } => {
            Some(language.unwrap_or(RawLanguage::Text).content_type())
        }
        RequestBody::FormUrlEncoded { .. } => Some("application/x-www-form-urlencoded"),
        RequestBody::Multipart { .. } => Some("multipart/form-data; boundary=…"),
        RequestBody::Binary { .. } => Some("application/octet-stream"),
        RequestBody::None => None,
    }
}
