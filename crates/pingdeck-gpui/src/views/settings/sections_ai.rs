//! AI, Agent, Permissions, MCP, Skills, Routing, Model Catalog, and Usage Settings pages.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::{Language, SettingsSection as CoreSection};
use pingdeck_desktop::state::SettingsSection;

use crate::ui::{
    Tone, badge, btn_secondary, card, field_row, h_row, palette, section_heading, to_hsla, v_col,
};

use super::SettingsView;

pub fn render_ai_section(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let zh = view.settings.general.language == Language::ZhCn;
    let ai = &view.settings.ai;
    match view.active {
        SettingsSection::Agent => v_col()
            .gap(px(4.0))
            .child(field_row(
                if zh {
                    "最大工具迭代次数"
                } else {
                    "Max tool iterations"
                },
                None,
                badge(ai.agent.max_tool_iterations.to_string(), Tone::Neutral, cx),
                cx,
            ))
            .child(field_row(
                if zh {
                    "审批超时"
                } else {
                    "Approval timeout"
                },
                None,
                badge(
                    format!("{} s", ai.agent.approval_timeout_s),
                    Tone::Neutral,
                    cx,
                ),
                cx,
            ))
            .child(field_row(
                if zh {
                    "默认权限模式"
                } else {
                    "Default permission mode"
                },
                None,
                badge(
                    format!("{:?}", ai.agent.default_permission_mode),
                    Tone::Accent,
                    cx,
                ),
                cx,
            ))
            .child(field_row(
                if zh {
                    "重置 AI 设置"
                } else {
                    "Reset AI defaults"
                },
                None,
                btn_secondary("ai-reset", if zh { "恢复默认" } else { "Reset" }).on_click(
                    cx.listener(|this, _, _, cx| this.reset_section(CoreSection::Ai, cx)),
                ),
                cx,
            )),
        SettingsSection::Personalization => v_col().gap(px(8.0)).child(
            card(cx).p(px(14.0)).child(
                v_col()
                    .gap(px(6.0))
                    .child(section_heading(
                        if zh {
                            "全局指令 (Global Instructions)"
                        } else {
                            "Global Instructions"
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(palette(cx).ink_2))
                            .child(SharedString::from(
                                if ai.agent.global_instructions.is_empty() {
                                    if zh {
                                        "当前未设置全局指令（最多 4000 字符）。"
                                    } else {
                                        "No global instructions set (up to 4,000 chars)."
                                    }
                                    .to_owned()
                                } else {
                                    ai.agent.global_instructions.clone()
                                },
                            )),
                    ),
            ),
        ),
        SettingsSection::Memory => {
            let mut list = v_col().gap(px(6.0)).child(section_heading(
                format!(
                    "{} ({})",
                    if zh {
                        "受信任的项目文件夹"
                    } else {
                        "Trusted project folders"
                    },
                    view.trusted_folders.len()
                ),
                cx,
            ));
            for folder in &view.trusted_folders {
                list = list.child(field_row(
                    SharedString::from(folder.path.clone()),
                    None,
                    badge(if zh { "已信任" } else { "Trusted" }, Tone::Success, cx),
                    cx,
                ));
            }
            list
        }
        SettingsSection::Subagents => {
            let roles = &ai.agent.role_models;
            v_col()
                .gap(px(4.0))
                .child(field_row(
                    "Fast",
                    None,
                    badge(
                        roles.fast.clone().unwrap_or_else(|| "default".into()),
                        Tone::Neutral,
                        cx,
                    ),
                    cx,
                ))
                .child(field_row(
                    "Deep",
                    None,
                    badge(
                        roles.deep.clone().unwrap_or_else(|| "default".into()),
                        Tone::Neutral,
                        cx,
                    ),
                    cx,
                ))
                .child(field_row(
                    "Vision",
                    None,
                    badge(
                        roles.vision.clone().unwrap_or_else(|| "default".into()),
                        Tone::Neutral,
                        cx,
                    ),
                    cx,
                ))
        }
        SettingsSection::Tools => v_col().gap(px(4.0)).child(field_row(
            if zh {
                "已屏蔽命令模式"
            } else {
                "Blocked command patterns"
            },
            None,
            badge(
                format!("{} rules", ai.agent.blocked_commands.len()),
                Tone::Warning,
                cx,
            ),
            cx,
        )),
        SettingsSection::Permissions => {
            let steps = [
                "1. Hard block (never lowered)",
                "2. Explicit Block rules",
                "3. Mandatory Ask gates",
                "4. Scoped Allow rules",
                "5. Single-tool Allow",
                "6. Permission mode default (OS sandbox always active)",
            ];
            let mut col = v_col().gap(px(8.0)).child(
                card(cx).p(px(12.0)).child(
                    v_col()
                        .gap(px(4.0))
                        .child(section_heading(
                            if zh {
                                "固定权限评估顺序（Full Access 不降低前三层，不关闭 OS 沙箱）"
                            } else {
                                "Fixed Permission Precedence (Full Access never lowers layers 1-3 or disables OS sandbox)"
                            },
                            cx,
                        ))
                        .children(steps.into_iter().map(|s| {
                            div()
                                .text_xs()
                                .text_color(to_hsla(palette(cx).ink_2))
                                .child(s)
                        })),
                ),
            );
            col = col.child(section_heading(
                format!(
                    "{} ({})",
                    if zh {
                        "当前规则"
                    } else {
                        "Configured Rules"
                    },
                    view.permission_rules.len()
                ),
                cx,
            ));
            for rv in view.permission_rules.iter().take(40) {
                col = col.child(field_row(
                    SharedString::from(rv.rule.tool_pattern.clone()),
                    rv.rule.arg_pattern.clone().map(SharedString::from),
                    badge(format!("{:?}", rv.rule.action), Tone::Neutral, cx),
                    cx,
                ));
            }
            col
        }
        SettingsSection::Mcp => {
            let mut col = v_col().gap(px(6.0)).child(section_heading(
                format!(
                    "{} ({})",
                    if zh { "MCP 服务器" } else { "MCP Servers" },
                    view.mcp_servers.len()
                ),
                cx,
            ));
            for srv in &view.mcp_servers {
                col = col.child(field_row(
                    SharedString::from(srv.server.name.clone()),
                    Some(SharedString::from(format!("{:?}", srv.server.transport))),
                    badge(
                        if srv.server.enabled {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                        if srv.server.enabled {
                            Tone::Success
                        } else {
                            Tone::Neutral
                        },
                        cx,
                    ),
                    cx,
                ));
            }
            col
        }
        SettingsSection::Skills => {
            let mut col = v_col().gap(px(6.0)).child(section_heading(
                format!(
                    "{} ({})",
                    if zh {
                        "已发现技能"
                    } else {
                        "Discovered Skills"
                    },
                    view.skills.len()
                ),
                cx,
            ));
            for skill in &view.skills {
                col = col.child(field_row(
                    SharedString::from(skill.name.clone()),
                    Some(SharedString::from(skill.description.clone())),
                    badge(
                        if skill.enabled { "On" } else { "Off" },
                        if skill.enabled {
                            Tone::Success
                        } else {
                            Tone::Neutral
                        },
                        cx,
                    ),
                    cx,
                ));
            }
            col
        }
        SettingsSection::Routing => v_col()
            .gap(px(4.0))
            .child(field_row(
                if zh {
                    "启动时自动开启本地网关"
                } else {
                    "Autostart local gateway"
                },
                None,
                badge(
                    if ai.gateway.enabled_on_start {
                        "On"
                    } else {
                        "Off"
                    },
                    if ai.gateway.enabled_on_start {
                        Tone::Success
                    } else {
                        Tone::Neutral
                    },
                    cx,
                ),
                cx,
            ))
            .child(field_row(
                if zh {
                    "网关监听端口"
                } else {
                    "Gateway port"
                },
                None,
                badge(ai.gateway.port.to_string(), Tone::Accent, cx),
                cx,
            )),
        SettingsSection::Usage => v_col().gap(px(4.0)).child(field_row(
            if zh {
                "用量汇总保留天数"
            } else {
                "Usage rollup retention"
            },
            None,
            h_row().gap(px(6.0)).child(badge(
                format!("{} days", ai.providers.usage_retention_days),
                Tone::Neutral,
                cx,
            )),
            cx,
        )),
        _ => v_col(),
    }
}
