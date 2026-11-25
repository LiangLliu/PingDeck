//! Response branches editor for HTTP mock routes: status, fault, body/file/SSE, and callbacks.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::model::items::{
    MockBodySource, MockFault, MockRoute, MockServer, MockStream, MockStreamEnd, MockStreamEvent,
};
use pingdeck_desktop::services::{FilePickerSpec, HostDialog as _, RfdDialog};

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, btn_icon, btn_secondary, card, choice_chip, code_editor,
    data_row, h_row, meta_pill, number_field, palette, section_header, text_input, to_hsla, v_col,
};

use super::state::MockView;
use super::tab_route_rules::{render_matching_rules, render_resp_headers};

pub fn render_response_branches(
    view: &MockView,
    srv: &MockServer,
    route: &MockRoute,
    cx: &mut Context<MockView>,
) -> AnyElement {
    let p = palette(cx);
    let zh = view.is_zh();

    let mut branch_strip = h_row().gap(px(6.0)).flex_wrap();
    for resp in &route.responses {
        let rid = resp.id.clone();
        let active = view.selected_response_id.as_deref() == Some(resp.id.as_str());
        let label = if resp.is_default {
            format!("★ {} ({})", resp.status_code, resp.label)
        } else {
            format!("{} ({})", resp.status_code, resp.label)
        };
        branch_strip = branch_strip.child(
            choice_chip(
                SharedString::from(format!("mock-resp-tab-{}", resp.id)),
                label,
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.snapshot_inputs(cx);
                this.selected_response_id = Some(rid.clone());
                this.pending_sync = true;
                cx.notify();
            })),
        );
    }

    let branch_actions = h_row()
        .gap(px(4.0))
        .child(
            btn_secondary("mock-resp-add", view.t("+ 分支", "+ Branch"))
                .on_click(cx.listener(|this, _, _, cx| this.add_response(cx))),
        )
        .child(
            btn_ghost("mock-resp-dup", view.t("复制", "Duplicate"))
                .on_click(cx.listener(|this, _, _, cx| this.duplicate_selected_response(cx))),
        )
        .into_any_element();

    let mut box_el = card(cx)
        .p(px(14.0))
        .gap(px(12.0))
        .child(section_header(
            view.t("响应分支配置", "Response Branches"),
            Some(branch_actions),
            cx,
        ))
        .child(branch_strip);

    let Some(resp) = view.selected_response() else {
        return box_el.into_any_element();
    };
    let resp_id = resp.id.clone();
    let can_delete = route.responses.len() > 1;

    // Label, Status, Latency, Default & Delete
    let mut top_fields = h_row()
        .w_full()
        .gap(px(10.0))
        .items_end()
        .child(
            v_col()
                .flex_1()
                .min_w_0()
                .gap(px(4.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(view.t("分支名称", "Label")),
                )
                .child(text_input(&view.resp_label_input)),
        )
        .child(
            v_col()
                .w(px(90.0))
                .gap(px(4.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(view.t("状态码", "Status")),
                )
                .child(number_field(&view.resp_status_input, None, 86.0, cx)),
        )
        .child(
            v_col()
                .w(px(120.0))
                .gap(px(4.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(view.t("额外延迟", "Latency")),
                )
                .child(number_field(&view.resp_latency_input, Some("ms"), 86.0, cx)),
        );
    if !resp.is_default {
        let rid_def = resp_id.clone();
        top_fields = top_fields.child(
            btn_ghost("mock-resp-set-def", view.t("设为默认", "Set Default")).on_click(
                cx.listener(move |this, _, _, cx| {
                    if let Some(r) = this.selected_route_mut() {
                        for x in &mut r.responses {
                            x.is_default = x.id == rid_def;
                        }
                        this.dirty = true;
                        cx.notify();
                    }
                }),
            ),
        );
    }
    if can_delete {
        let rid_del = resp_id.clone();
        top_fields = top_fields.child(
            btn_icon("mock-resp-del", UiIcon::Trash)
                .tooltip(view.t("删除分支", "Delete branch"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.snapshot_inputs(cx);
                    if let Some(r) = this.selected_route_mut() {
                        r.responses.retain(|x| x.id != rid_del);
                        if !r.responses.iter().any(|x| x.is_default)
                            && let Some(first) = r.responses.first_mut()
                        {
                            first.is_default = true;
                        }
                        this.dirty = true;
                        this.pending_sync = true;
                        cx.notify();
                    }
                })),
        );
    }
    box_el = box_el.child(top_fields);

    // Fault simulation + Body mode selectors
    let fault_row = h_row()
        .gap(px(6.0))
        .flex_wrap()
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(view.t("故障注入:", "Fault:")),
        )
        .child(
            choice_chip(
                "mock-fault-none",
                view.t("正常响应", "None"),
                resp.fault.is_none(),
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(rp) = this.selected_response_mut() {
                    rp.fault = None;
                    this.dirty = true;
                    cx.notify();
                }
            })),
        )
        .child(
            choice_chip(
                "mock-fault-hang",
                view.t("挂起超时 Hang", "Hang"),
                resp.fault == Some(MockFault::Hang),
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(rp) = this.selected_response_mut() {
                    rp.fault = Some(MockFault::Hang);
                    this.dirty = true;
                    cx.notify();
                }
            })),
        )
        .child(
            choice_chip(
                "mock-fault-drop",
                view.t("直接断开 Drop", "Drop"),
                resp.fault == Some(MockFault::Drop),
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(rp) = this.selected_response_mut() {
                    rp.fault = Some(MockFault::Drop);
                    this.dirty = true;
                    cx.notify();
                }
            })),
        );

    let mode_idx = if resp.stream.is_some() {
        3
    } else if resp.body_source == Some(MockBodySource::File) {
        1
    } else if resp.body_source == Some(MockBodySource::Base64) {
        2
    } else {
        0
    };
    let body_mode_row = h_row()
        .gap(px(6.0))
        .flex_wrap()
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(view.t("正文模式:", "Body:")),
        )
        .child(
            choice_chip(
                "mock-bm-tpl",
                view.t("模板文本", "Template"),
                mode_idx == 0,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.snapshot_inputs(cx);
                if let Some(rp) = this.selected_response_mut() {
                    rp.body_source = None;
                    rp.stream = None;
                    this.dirty = true;
                    this.pending_sync = true;
                    cx.notify();
                }
            })),
        )
        .child(
            choice_chip(
                "mock-bm-file",
                view.t("本地文件", "File"),
                mode_idx == 1,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.snapshot_inputs(cx);
                if let Some(rp) = this.selected_response_mut() {
                    rp.body_source = Some(MockBodySource::File);
                    rp.stream = None;
                    this.dirty = true;
                    this.pending_sync = true;
                    cx.notify();
                }
            })),
        )
        .child(
            choice_chip("mock-bm-b64", "Base64", mode_idx == 2, cx).on_click(cx.listener(
                |this, _, _, cx| {
                    this.snapshot_inputs(cx);
                    if let Some(rp) = this.selected_response_mut() {
                        rp.body_source = Some(MockBodySource::Base64);
                        rp.stream = None;
                        this.dirty = true;
                        this.pending_sync = true;
                        cx.notify();
                    }
                },
            )),
        )
        .child(
            choice_chip(
                "mock-bm-sse",
                view.t("SSE 流式", "SSE Stream"),
                mode_idx == 3,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.snapshot_inputs(cx);
                if let Some(rp) = this.selected_response_mut() {
                    rp.body_source = None;
                    if rp.stream.is_none() {
                        rp.stream = Some(MockStream {
                            events: vec![MockStreamEvent {
                                event: "message".into(),
                                data: "{\"chunk\":1}".into(),
                            }],
                            interval_ms: 200,
                            end: MockStreamEnd::Close,
                        });
                    }
                    this.dirty = true;
                    this.pending_sync = true;
                    cx.notify();
                }
            })),
        );

    box_el = box_el.child(fault_row).child(body_mode_row);

    // Body content section
    if mode_idx == 1 {
        box_el = box_el.child(
            h_row()
                .w_full()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(text_input(&view.resp_file_input)),
                )
                .child(
                    btn_secondary("mock-pick-body-file", view.t("选择文件…", "Browse…")).on_click(
                        cx.listener(|this, _, window, cx| {
                            if let Some(path) = RfdDialog.pick_file(&FilePickerSpec::default()) {
                                let s = path.display().to_string();
                                this.resp_file_input
                                    .update(cx, |inp, cx| inp.set_value(s.clone(), window, cx));
                                if let Some(rp) = this.selected_response_mut() {
                                    rp.body = s;
                                    this.dirty = true;
                                }
                                cx.notify();
                            }
                        }),
                    ),
                ),
        );
    } else if let Some(stream) = &resp.stream {
        box_el = box_el.child(render_sse_editor(view, stream, cx));
    } else {
        let body_val = view.resp_body_editor.read(cx).value().to_string();
        let json_valid =
            mode_idx == 0 && serde_json::from_str::<serde_json::Value>(&body_val).is_ok();
        box_el = box_el.child(
            v_col()
                .w_full()
                .gap(px(4.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(if mode_idx == 2 {
                                    view.t("Base64 编码字节内容", "Base64-encoded binary payload")
                                } else {
                                    view.t(
                                        "模板：{{urlParam 'id'}} · {{queryParam 'q'}} · {{body 'key'}} · {{data 'bucket'}} · {{faker.uuid}}",
                                        "Templates: {{urlParam 'id'}} · {{queryParam 'q'}} · {{body 'key'}} · {{data 'bucket'}} · {{faker.uuid}}",
                                    )
                                }),
                        )
                        .children((mode_idx == 0 && !body_val.trim().is_empty()).then(|| {
                            badge(
                                if json_valid { "JSON ✓" } else { view.t("模板/文本", "Template") },
                                if json_valid { Tone::Success } else { Tone::Neutral },
                                cx,
                            )
                        })),
                )
                .child(div().w_full().h(px(180.0)).child(code_editor(&view.resp_body_editor))),
        );
    }

    // Response Headers
    box_el = box_el.child(render_resp_headers(view, &resp.headers, cx));
    // Matching Rules
    box_el = box_el.child(render_matching_rules(view, resp, zh, cx));
    // Attached Callbacks
    if !srv.callbacks.is_empty() {
        let mut cb_row = h_row().gap(px(6.0)).flex_wrap().child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(view.t("触发回调:", "Callbacks:")),
        );
        for cb in &srv.callbacks {
            let cid = cb.id.clone();
            let attached = resp.callback_ids.contains(&cb.id);
            cb_row = cb_row.child(
                choice_chip(
                    SharedString::from(format!("mock-rcb-{}", cb.id)),
                    cb.name.clone(),
                    attached,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(rp) = this.selected_response_mut() {
                        if let Some(pos) = rp.callback_ids.iter().position(|x| x == &cid) {
                            rp.callback_ids.remove(pos);
                        } else {
                            rp.callback_ids.push(cid.clone());
                        }
                        this.dirty = true;
                        cx.notify();
                    }
                })),
            );
        }
        box_el = box_el.child(cb_row);
    }

    box_el.into_any_element()
}

fn render_sse_editor(
    view: &MockView,
    stream: &MockStream,
    cx: &mut Context<MockView>,
) -> AnyElement {
    let p = palette(cx);
    let mut box_el = v_col()
        .w_full()
        .gap(px(8.0))
        .p(px(10.0))
        .rounded(px(8.0))
        .bg(to_hsla(p.sunken));
    let top = h_row()
        .w_full()
        .justify_between()
        .gap(px(10.0))
        .child(
            h_row()
                .gap(px(8.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(view.t("事件间隔", "Interval")),
                )
                .child(number_field(
                    &view.stream_interval_input,
                    Some("ms"),
                    76.0,
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .child(
                    choice_chip(
                        "mock-sse-close",
                        "Close",
                        stream.end == MockStreamEnd::Close,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(st) =
                            this.selected_response_mut().and_then(|r| r.stream.as_mut())
                        {
                            st.end = MockStreamEnd::Close;
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                )
                .child(
                    choice_chip(
                        "mock-sse-hang",
                        "Hang",
                        stream.end == MockStreamEnd::Hang,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(st) =
                            this.selected_response_mut().and_then(|r| r.stream.as_mut())
                        {
                            st.end = MockStreamEnd::Hang;
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                )
                .child(
                    choice_chip(
                        "mock-sse-drop",
                        "Drop",
                        stream.end == MockStreamEnd::Drop,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(st) =
                            this.selected_response_mut().and_then(|r| r.stream.as_mut())
                        {
                            st.end = MockStreamEnd::Drop;
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                ),
        );
    box_el = box_el.child(top);

    for (idx, ev) in stream.events.iter().enumerate() {
        box_el = box_el.child(
            data_row(cx)
                .child(meta_pill(
                    if ev.event.is_empty() {
                        "message"
                    } else {
                        &ev.event
                    },
                    cx,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(ev.data.clone()),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("mock-sse-rm-{idx}")),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(st) =
                            this.selected_response_mut().and_then(|r| r.stream.as_mut())
                            && idx < st.events.len()
                        {
                            st.events.remove(idx);
                            this.dirty = true;
                            cx.notify();
                        }
                    })),
                ),
        );
    }

    box_el = box_el.child(
        h_row()
            .w_full()
            .gap(px(6.0))
            .child(
                div()
                    .w(px(120.0))
                    .child(text_input(&view.stream_ev_name_input)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(text_input(&view.stream_ev_data_input)),
            )
            .child(
                btn_secondary("mock-sse-add", view.t("+ 事件", "+ Event")).on_click(cx.listener(
                    |this, _, window, cx| {
                        let ev = this.stream_ev_name_input.read(cx).value().to_string();
                        let data = this.stream_ev_data_input.read(cx).value().to_string();
                        if data.trim().is_empty() {
                            return;
                        }
                        if let Some(st) =
                            this.selected_response_mut().and_then(|r| r.stream.as_mut())
                        {
                            st.events.push(MockStreamEvent { event: ev, data });
                            this.dirty = true;
                        }
                        this.stream_ev_name_input
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        this.stream_ev_data_input
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        cx.notify();
                    },
                )),
            ),
    );
    box_el.into_any_element()
}
