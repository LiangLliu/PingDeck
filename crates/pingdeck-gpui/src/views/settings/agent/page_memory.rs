//! Settings › 智能体 › 项目指令与信任 (`SettingsSection::Memory`, prototype `ai/memory`).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::current_time_ms;
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::t;

use crate::ui::{
    Tone, UiIcon, badge, btn_danger, btn_ghost, btn_primary, card, empty_state, h_row, icon_xs,
    palette, to_hsla, v_col,
};

use super::AgentSettingsView;
use super::widgets::{format_ago_short, section_head};

pub(super) fn render_memory(
    view: &AgentSettingsView,
    lang: Language,
    zh: bool,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let now_ms = current_time_ms();

    let add_btn = btn_primary(
        "agent-trust-add",
        if zh {
            "+ 信任文件夹…"
        } else {
            "+ Trust folder…"
        },
    )
    .on_click(cx.listener(|this, _, _, cx| this.pick_and_trust_folder(cx)))
    .into_any_element();

    let header = section_head(
        if zh {
            "已信任的项目目录"
        } else {
            "Trusted project folders"
        },
        Some(add_btn),
        cx,
    );

    let intro = div()
        .text_xs()
        .text_color(to_hsla(p.muted))
        .child(t(lang, "set.sec.memory.trusted.d").to_owned());

    if view.trusted_folders.is_empty() {
        return v_col().gap(px(12.0)).child(header).child(intro).child(
            card(cx).h(px(180.0)).child(empty_state(
                UiIcon::ShieldCheck,
                if zh {
                    "还没有信任的项目目录"
                } else {
                    "No trusted folders yet"
                },
                t(lang, "set.agent.trusted.empty").to_owned(),
                cx,
            )),
        );
    }

    let table_head = h_row()
        .px(px(12.0))
        .py(px(8.0))
        .gap(px(10.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(to_hsla(p.muted))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(if zh { "目录路径" } else { "Folder path" }),
        )
        .child(div().w(px(180.0)).flex_shrink_0().child(if zh {
            "读取内容"
        } else {
            "Loaded content"
        }))
        .child(
            div()
                .w(px(96.0))
                .flex_shrink_0()
                .child(if zh { "授权时间" } else { "Decided" }),
        )
        .child(
            div()
                .w(px(110.0))
                .flex_shrink_0()
                .text_right()
                .child(if zh { "操作" } else { "Action" }),
        );

    let mut rows = v_col().child(table_head);
    let total = view.trusted_folders.len();
    for (idx, item) in view.trusted_folders.iter().enumerate() {
        let trust = &item.trust;
        let path = trust.path.clone();
        let exists = item.info.as_ref().is_none_or(|i| i.exists);
        let instr_changed = item.info.as_ref().is_some_and(|i| i.instructions_changed);
        let instr_file = item.info.as_ref().and_then(|i| i.instructions_file.clone());
        let skill_count: u32 = item
            .info
            .as_ref()
            .map(|i| i.skill_dirs.iter().map(|s| s.skills).sum())
            .unwrap_or(0);

        let mut content_tags = h_row().w(px(180.0)).gap(px(5.0)).flex_wrap();
        if !trust.trusted {
            content_tags = content_tags.child(badge(
                if zh { "已拒绝" } else { "Declined" },
                Tone::Neutral,
                cx,
            ));
        } else {
            if let Some(f) = instr_file {
                content_tags = content_tags.child(badge(f, Tone::Neutral, cx));
            }
            if skill_count > 0 {
                content_tags = content_tags.child(badge(
                    if zh {
                        format!("{skill_count} 项技能")
                    } else {
                        format!("{skill_count} skill(s)")
                    },
                    Tone::Accent,
                    cx,
                ));
            }
            if item
                .info
                .as_ref()
                .is_some_and(|i| i.instructions_file.is_none() && skill_count == 0 && i.exists)
            {
                content_tags =
                    content_tags.child(div().text_xs().text_color(to_hsla(p.faint)).child(if zh {
                        "无项目指令"
                    } else {
                        "No instructions"
                    }));
            }
        }

        let path_col = v_col()
            .flex_1()
            .min_w_0()
            .gap(px(3.0))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(to_hsla(p.ink))
                    .truncate()
                    .child(SharedString::from(path.clone())),
            )
            .children((!exists || instr_changed).then(|| {
                let mut warns = h_row().gap(px(6.0));
                if !exists {
                    warns = warns.child(badge(
                        t(lang, "set.agent.trusted.gone").to_owned(),
                        Tone::Warning,
                        cx,
                    ));
                }
                if instr_changed {
                    warns = warns.child(badge(
                        if zh {
                            "指令已变化，下次需重新确认"
                        } else {
                            "Changed — asks again"
                        },
                        Tone::Warning,
                        cx,
                    ));
                }
                warns
            }));

        let when = format_ago_short(trust.decided_at_ms, now_ms, zh);
        let path_for_act = path.clone();
        let path_for_retrust = path;
        let action_el = h_row()
            .w(px(110.0))
            .justify_end()
            .gap(px(4.0))
            .children((instr_changed && exists).then(|| {
                btn_ghost(
                    SharedString::from(format!("trust-re-{idx}")),
                    if zh { "重信" } else { "Re-trust" },
                )
                .icon(icon_xs(UiIcon::Refresh))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.trust_folder(path_for_retrust.clone(), cx);
                }))
            }))
            .child(if trust.trusted {
                btn_danger(
                    SharedString::from(format!("trust-revoke-{idx}")),
                    if zh { "撤销信任" } else { "Revoke" },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.revoke_folder_trust(path_for_act.clone(), cx);
                }))
                .into_any_element()
            } else {
                btn_ghost(
                    SharedString::from(format!("trust-rm-{idx}")),
                    if zh { "移除" } else { "Remove" },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.revoke_folder_trust(path_for_act.clone(), cx);
                }))
                .into_any_element()
            });

        let mut row = h_row()
            .px(px(12.0))
            .py(px(10.0))
            .gap(px(10.0))
            .child(path_col)
            .child(content_tags)
            .child(
                div()
                    .w(px(96.0))
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(when)),
            )
            .child(action_el);
        if idx + 1 < total {
            row = row
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)));
        }
        rows = rows.child(row);
    }

    v_col()
        .gap(px(12.0))
        .child(header)
        .child(intro)
        .child(card(cx).overflow_hidden().child(rows))
}
