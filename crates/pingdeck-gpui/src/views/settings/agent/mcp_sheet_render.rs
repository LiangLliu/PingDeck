//! Render helpers and JSON/key-value parsers for `McpSheet`.

use std::collections::BTreeMap;

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::mcp::McpTransportConfig;
use pingdeck_desktop::core::agent::model::AgentMcpServer;

use crate::ui::{
    ScrollableElement as _, StatusShape, UiIcon, WindowExt as _, btn_ghost, btn_primary,
    btn_secondary, h_row, icon_xs, multiline_input, status_indicator, tab_bar_segmented, tab_item,
    text_input, to_hsla, toggle_switch, v_col,
};

use super::mcp_sheet::McpSheet;

pub(super) fn render_form_mode(
    sheet: &McpSheet,
    p: &pingdeck_desktop::theme::ResolvedPalette,
    cx: &mut Context<McpSheet>,
) -> gpui_kit::Div {
    let ent = cx.entity().clone();
    let transport_seg = tab_bar_segmented("mcp-sheet-transport-seg")
        .selected_index(usize::from(sheet.is_http))
        .child(tab_item("stdio"))
        .child(tab_item("Streamable HTTP"))
        .on_click(move |idx, _, cx| {
            ent.update(cx, |this, cx| {
                this.is_http = *idx == 1;
                this.needs_stdio_confirm = false;
                cx.notify();
            });
        });

    let mut fields = v_col()
        .flex_1()
        .min_h_0()
        .gap(px(12.0))
        .pr(px(4.0))
        .overflow_y_scrollbar()
        .child(field_block(
            crate::ui::tr(cx).settings().mcp_sheet_render_server_name(),
            text_input(&sheet.name_input),
            p,
        ))
        .child(field_block(
            crate::ui::tr(cx).settings().tools_form_transport(),
            transport_seg,
            p,
        ));

    if sheet.is_http {
        fields = fields
            .child(field_block(
                crate::ui::tr(cx).settings().mcp_sheet_render_endpoint_url(),
                text_input(&sheet.url_input),
                p,
            ))
            .child(field_block(
                crate::ui::tr(cx)
                    .settings()
                    .mcp_sheet_render_headers_name_value_per_line(),
                div()
                    .h(px(88.0))
                    .child(multiline_input(&sheet.headers_input)),
                p,
            ));
    } else {
        fields = fields
            .child(field_block(
                crate::ui::tr(cx).settings().mcp_sheet_render_command(),
                text_input(&sheet.command_input),
                p,
            ))
            .child(field_block(
                crate::ui::tr(cx)
                    .settings()
                    .mcp_sheet_render_arguments_one_per_line(),
                div().h(px(80.0)).child(multiline_input(&sheet.args_input)),
                p,
            ))
            .child(field_block(
                crate::ui::tr(cx)
                    .settings()
                    .mcp_sheet_render_working_directory_optional(),
                h_row()
                    .gap(px(6.0))
                    .child(div().flex_1().child(text_input(&sheet.cwd_input)))
                    .child(
                        btn_secondary(
                            "mcp-pick-cwd",
                            crate::ui::tr(cx).settings().mcp_sheet_render_browse(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.pick_cwd(window, cx))),
                    ),
                p,
            ))
            .child(field_block(
                crate::ui::tr(cx)
                    .settings()
                    .mcp_sheet_render_environment_key_value_per_line(),
                div().h(px(80.0)).child(multiline_input(&sheet.env_input)),
                p,
            ));
    }

    fields = fields
        .child(field_block(
            crate::ui::tr(cx)
                .settings()
                .mcp_sheet_render_call_timeout_seconds_default_120(),
            div().w(px(120.0)).child(text_input(&sheet.timeout_input)),
            p,
        ))
        .child(
            h_row()
                .justify_between()
                .py(px(6.0))
                .child(
                    v_col()
                        .flex_1()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(
                                    crate::ui::tr(cx)
                                        .settings()
                                        .mcp_sheet_render_trust_this_server(),
                                ),
                        )
                        .child(
                            div().text_xs().text_color(to_hsla(p.muted)).child(
                                crate::ui::tr(cx)
                                    .settings()
                                    .mcp_sheet_render_may_run_tools_it_marks(),
                            ),
                        ),
                )
                .child(
                    toggle_switch("mcp-sheet-trust", sheet.trusted).on_change(cx.listener(
                        |this, c: &bool, _, cx| {
                            this.trusted = *c;
                            cx.notify();
                        },
                    )),
                ),
        )
        .child(
            h_row()
                .justify_between()
                .py(px(4.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(crate::ui::tr(cx).settings().mcp_sheet_render_enabled()),
                )
                .child(
                    toggle_switch("mcp-sheet-en", sheet.enabled).on_change(cx.listener(
                        |this, c: &bool, _, cx| {
                            this.enabled = *c;
                            this.needs_stdio_confirm = false;
                            cx.notify();
                        },
                    )),
                ),
        );

    if let Some(tr) = &sheet.test_result {
        let (shape, bg, fg, msg) = if tr.ok {
            let srv_name = tr.server.as_deref().unwrap_or("MCP");
            let text = crate::ui::tr(cx)
                .settings()
                .mcp_sheet_render_connected_in_ms_tool_s(
                    srv_name,
                    tr.elapsed_ms,
                    tr.tool_names.len(),
                );
            (StatusShape::Ok, to_hsla(p.ok_soft), to_hsla(p.ok), text)
        } else {
            let err = tr.error.as_deref().unwrap_or("Failed");
            (
                StatusShape::Bad,
                to_hsla(p.bad_soft),
                to_hsla(p.bad),
                err.to_owned(),
            )
        };
        fields = fields.child(
            h_row()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(6.0))
                .bg(bg)
                .text_xs()
                .text_color(fg)
                .child(status_indicator(shape, cx))
                .child(SharedString::from(msg)),
        );
    }

    if sheet.needs_stdio_confirm {
        fields = fields.child(
            h_row()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.warn_soft))
                .text_xs()
                .text_color(to_hsla(p.warn))
                .child(status_indicator(StatusShape::Warn, cx))
                .child(
                    crate::ui::tr(cx)
                        .settings()
                        .mcp_sheet_render_this_stdio_command_runs_outside(),
                ),
        );
    }

    if let Some(err) = &sheet.error_msg {
        fields = fields.child(
            h_row()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.bad_soft))
                .text_xs()
                .text_color(to_hsla(p.bad))
                .child(status_indicator(StatusShape::Bad, cx))
                .child(SharedString::from(err.clone())),
        );
    }

    let save_label = if sheet.needs_stdio_confirm {
        crate::ui::tr(cx).settings().mcp_sheet_render_confirm_save()
    } else {
        crate::ui::tr(cx).common().save()
    };
    let confirm_flag = sheet.needs_stdio_confirm;

    v_col().size_full().gap(px(12.0)).child(fields).child(
        h_row()
            .justify_between()
            .pt(px(10.0))
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)))
            .child(
                btn_secondary(
                    "mcp-sheet-test",
                    if sheet.testing {
                        crate::ui::tr(cx).settings().mcp_sheet_render_testing()
                    } else {
                        crate::ui::tr(cx).settings().network_test()
                    },
                )
                .icon(icon_xs(UiIcon::Zap))
                .on_click(cx.listener(|this, _, _, cx| this.run_test(cx))),
            )
            .child(
                h_row()
                    .gap(px(8.0))
                    .child(
                        btn_ghost("mcp-sheet-cancel", crate::ui::tr(cx).common().cancel())
                            .on_click(|_, window, cx| window.close_sheet(cx)),
                    )
                    .child(btn_primary("mcp-sheet-save", save_label).on_click(
                        cx.listener(move |this, _, _, cx| this.save_form(confirm_flag, cx)),
                    )),
            ),
    )
}

pub(super) fn render_json_mode(
    sheet: &McpSheet,
    p: &pingdeck_desktop::theme::ResolvedPalette,
    cx: &mut Context<McpSheet>,
) -> gpui_kit::Div {
    let mut col = v_col()
        .size_full()
        .gap(px(12.0))
        .child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .settings()
                    .mcp_sheet_render_paste_claude_desktop_cursor_mcpservers(),
            ),
        )
        .child(
            div()
                .flex_1()
                .min_h(px(260.0))
                .child(multiline_input(&sheet.json_input)),
        );

    if let Some(err) = &sheet.error_msg {
        col = col.child(
            h_row()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.bad_soft))
                .text_xs()
                .text_color(to_hsla(p.bad))
                .child(status_indicator(StatusShape::Bad, cx))
                .child(SharedString::from(err.clone())),
        );
    }

    col.child(
        h_row()
            .justify_end()
            .gap(px(8.0))
            .pt(px(10.0))
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)))
            .child(
                btn_ghost("mcp-json-cancel", crate::ui::tr(cx).common().cancel())
                    .on_click(|_, window, cx| window.close_sheet(cx)),
            )
            .child(
                btn_primary(
                    "mcp-json-import",
                    crate::ui::tr(cx)
                        .settings()
                        .mcp_sheet_render_validate_import(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.import_json(cx))),
            ),
    )
}

fn field_block(
    label: &str,
    control: impl IntoElement,
    p: &pingdeck_desktop::theme::ResolvedPalette,
) -> gpui_kit::Div {
    v_col()
        .gap(px(4.0))
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(label.to_owned())),
        )
        .child(control)
}

pub(super) fn parse_kv_lines(raw: &str, sep: char) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once(sep) {
            let key = k.trim();
            if !key.is_empty() {
                map.insert(key.to_owned(), v.trim().to_owned());
            }
        }
    }
    map
}

pub(super) fn serialize_mcp_json(servers: &[AgentMcpServer]) -> String {
    let mut map = serde_json::Map::new();
    for s in servers {
        let entry = match &s.transport {
            McpTransportConfig::Stdio {
                command,
                args,
                env,
                cwd,
                ..
            } => {
                let mut o = serde_json::Map::new();
                o.insert("command".into(), serde_json::Value::String(command.clone()));
                if !args.is_empty() {
                    o.insert("args".into(), serde_json::json!(args));
                }
                if !env.is_empty() {
                    o.insert("env".into(), serde_json::json!(env));
                }
                if let Some(c) = cwd {
                    o.insert("cwd".into(), serde_json::Value::String(c.clone()));
                }
                serde_json::Value::Object(o)
            }
            McpTransportConfig::Http { url, headers } => {
                let mut o = serde_json::Map::new();
                o.insert("url".into(), serde_json::Value::String(url.clone()));
                if !headers.is_empty() {
                    o.insert("headers".into(), serde_json::json!(headers));
                }
                serde_json::Value::Object(o)
            }
        };
        map.insert(s.name.clone(), entry);
    }
    serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": map }))
        .unwrap_or_else(|_| "{\n  \"mcpServers\": {}\n}".to_owned())
}

pub(super) fn parse_mcp_servers_json(
    raw: &str,
    existing: &[AgentMcpServer],
    lang: pingdeck_desktop::core::model::settings::Language,
) -> Result<Vec<AgentMcpServer>, String> {
    let s = crate::ui::Tr(lang).settings();
    let root: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| s.agent_mcp_err_json_parse(e))?;
    let Some(servers_obj) = root.get("mcpServers").and_then(|v| v.as_object()) else {
        return Err(s.agent_mcp_err_top_object().to_owned());
    };
    let mut out = Vec::new();
    for (name, val) in servers_obj {
        let Some(obj) = val.as_object() else {
            return Err(s.agent_mcp_err_entry_object(name));
        };
        let existing_id = existing
            .iter()
            .find(|s| s.name == *name)
            .map(|s| s.id.clone())
            .unwrap_or_default();
        let transport = if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
            let headers = obj
                .get("headers")
                .and_then(|v| v.as_object())
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            McpTransportConfig::Http {
                url: url.to_owned(),
                headers,
            }
        } else if let Some(cmd) = obj.get("command").and_then(|v| v.as_str()) {
            let args = obj
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let env = obj
                .get("env")
                .and_then(|v| v.as_object())
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            let cwd = obj.get("cwd").and_then(|v| v.as_str()).map(str::to_owned);
            McpTransportConfig::Stdio {
                command: cmd.to_owned(),
                args,
                env,
                env_allow: Vec::new(),
                cwd,
            }
        } else {
            return Err(s.agent_mcp_err_entry_target(name));
        };
        out.push(AgentMcpServer {
            id: existing_id,
            name: name.clone(),
            transport,
            enabled: false,
            trusted: false,
            updated_at_ms: 0,
        });
    }
    Ok(out)
}
