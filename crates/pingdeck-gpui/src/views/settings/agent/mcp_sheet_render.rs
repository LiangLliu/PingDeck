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
    zh: bool,
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
            if zh { "服务器名称" } else { "Server name" },
            text_input(&sheet.name_input),
            p,
        ))
        .child(field_block(
            if zh { "传输方式" } else { "Transport" },
            transport_seg,
            p,
        ));

    if sheet.is_http {
        fields = fields
            .child(field_block(
                if zh { "端点 URL" } else { "Endpoint URL" },
                text_input(&sheet.url_input),
                p,
            ))
            .child(field_block(
                if zh {
                    "请求头（每行 Name: Value，存入本机保险库）"
                } else {
                    "Headers (Name: Value per line, stored locally)"
                },
                div()
                    .h(px(88.0))
                    .child(multiline_input(&sheet.headers_input)),
                p,
            ));
    } else {
        fields = fields
            .child(field_block(
                if zh { "启动命令" } else { "Command" },
                text_input(&sheet.command_input),
                p,
            ))
            .child(field_block(
                if zh {
                    "命令参数（每行一个）"
                } else {
                    "Arguments (one per line)"
                },
                div().h(px(80.0)).child(multiline_input(&sheet.args_input)),
                p,
            ))
            .child(field_block(
                if zh {
                    "工作目录（可选，绝对路径）"
                } else {
                    "Working directory (optional)"
                },
                h_row()
                    .gap(px(6.0))
                    .child(div().flex_1().child(text_input(&sheet.cwd_input)))
                    .child(
                        btn_secondary("mcp-pick-cwd", if zh { "选择…" } else { "Browse…" })
                            .on_click(cx.listener(|this, _, window, cx| this.pick_cwd(window, cx))),
                    ),
                p,
            ))
            .child(field_block(
                if zh {
                    "环境变量（每行 KEY=VALUE，存入本机保险库）"
                } else {
                    "Environment (KEY=VALUE per line, stored locally)"
                },
                div().h(px(80.0)).child(multiline_input(&sheet.env_input)),
                p,
            ));
    }

    fields = fields
        .child(field_block(
            if zh {
                "调用超时（秒，留空默认 120）"
            } else {
                "Call timeout (seconds, default 120)"
            },
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
                                .child(if zh {
                                    "信任此服务器"
                                } else {
                                    "Trust this server"
                                }),
                        )
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                            "允许不经询问运行其标记为只读的工具；未信任服务器的工具每次都会询问。"
                        } else {
                            "May run tools it marks read-only without asking."
                        })),
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
                        .child(if zh {
                            "保存后立即启用"
                        } else {
                            "Enabled"
                        }),
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
            let text = if zh {
                format!(
                    "已连接 {srv_name}，用时 {} ms · {} 个工具",
                    tr.elapsed_ms,
                    tr.tool_names.len()
                )
            } else {
                format!(
                    "Connected {srv_name} in {} ms · {} tool(s)",
                    tr.elapsed_ms,
                    tr.tool_names.len()
                )
            };
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
                .child(if zh {
                    "此 stdio 命令将在沙箱外运行。确认命令安全后再次点击“确认启动并保存”。"
                } else {
                    "This stdio command runs outside the sandbox. Click Confirm & Save to start it."
                }),
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
        if zh {
            "确认启动并保存"
        } else {
            "Confirm & Save"
        }
    } else if zh {
        "保存"
    } else {
        "Save"
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
                        if zh { "测试中…" } else { "Testing…" }
                    } else if zh {
                        "测试连接"
                    } else {
                        "Test connection"
                    },
                )
                .icon(icon_xs(UiIcon::Zap))
                .on_click(cx.listener(|this, _, _, cx| this.run_test(cx))),
            )
            .child(
                h_row()
                    .gap(px(8.0))
                    .child(
                        btn_ghost("mcp-sheet-cancel", if zh { "取消" } else { "Cancel" })
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
    zh: bool,
    p: &pingdeck_desktop::theme::ResolvedPalette,
    cx: &mut Context<McpSheet>,
) -> gpui_kit::Div {
    let mut col = v_col()
        .size_full()
        .gap(px(12.0))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(if zh {
                    "粘贴 Claude Desktop 或 Cursor 的 mcpServers JSON 格式。导入的服务器默认停用且未受信任，秘密值以占位符或本机保险库存管。"
                } else {
                    "Paste Claude Desktop / Cursor mcpServers JSON. Imported servers start disabled and untrusted."
                }),
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
                btn_ghost("mcp-json-cancel", if zh { "取消" } else { "Cancel" })
                    .on_click(|_, window, cx| window.close_sheet(cx)),
            )
            .child(
                btn_primary(
                    "mcp-json-import",
                    if zh {
                        "校验并导入"
                    } else {
                        "Validate & Import"
                    },
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
) -> Result<Vec<AgentMcpServer>, String> {
    let root: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("JSON 解析失败：{e}"))?;
    let Some(servers_obj) = root.get("mcpServers").and_then(|v| v.as_object()) else {
        return Err("需要名为 mcpServers 的顶层对象 / Expected top-level mcpServers object".into());
    };
    let mut out = Vec::new();
    for (name, val) in servers_obj {
        let Some(obj) = val.as_object() else {
            return Err(format!("{name} 必须是 JSON 对象"));
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
            return Err(format!("{name} 需要 command 或 url 字段"));
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
