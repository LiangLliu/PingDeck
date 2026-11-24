//! Model-group Settings pages not yet split into dedicated controllers (`Routing`, `Usage`).

use gpui_kit::{Context, ParentElement as _, Styled as _, px};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::state::SettingsSection;

use crate::ui::{Tone, badge, field_row, h_row, v_col};

use super::SettingsView;

pub fn render_ai_section(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let zh = view.settings.general.language == Language::ZhCn;
    let ai = &view.settings.ai;
    match view.active {
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
