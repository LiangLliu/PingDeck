//! Settings › 智能体 › 项目指令与信任 (`SettingsSection::Memory`, prototype `ai/memory`).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::app::current_time_ms;
use spanbay_desktop::core::model::settings::Language;

use crate::ui::{
    Tone, UiIcon, badge, btn_danger, btn_ghost, btn_primary, empty_state, h_row, icon_xs, palette,
    settings_sub_well, to_hsla, v_col,
};

use super::AgentSettingsView;
use super::widgets::{format_ago_short, section_card_flush, section_card_padded};

pub(super) fn render_memory(
    view: &AgentSettingsView,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let now_ms = current_time_ms();

    let add_btn = btn_primary(
        "agent-trust-add",
        crate::ui::tr(cx).settings().page_memory_trust_folder(),
    )
    .on_click(cx.listener(|this, _, _, cx| this.pick_and_trust_folder(cx)))
    .into_any_element();

    let title = crate::ui::tr(cx)
        .settings()
        .page_memory_trusted_project_folders();

    let intro = div().text_xs().text_color(to_hsla(p.muted)).child(
        crate::ui::Tr(lang)
            .settings()
            .sec_memory_trusted_d()
            .to_owned(),
    );

    if view.trusted_folders.is_empty() {
        return section_card_padded(
            title,
            Some(add_btn),
            v_col().w_full().gap(px(12.0)).child(intro).child(
                settings_sub_well(cx).h(px(176.0)).child(empty_state(
                    UiIcon::ShieldCheck,
                    crate::ui::tr(cx)
                        .settings()
                        .page_memory_no_trusted_folders_yet(),
                    crate::ui::Tr(lang)
                        .settings()
                        .agent_trusted_empty()
                        .to_owned(),
                    cx,
                )),
            ),
            cx,
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
                .child(crate::ui::tr(cx).settings().page_memory_folder_path()),
        )
        .child(
            div()
                .w(px(180.0))
                .flex_shrink_0()
                .child(crate::ui::tr(cx).settings().page_memory_loaded_content()),
        )
        .child(
            div()
                .w(px(96.0))
                .flex_shrink_0()
                .child(crate::ui::tr(cx).settings().page_memory_decided()),
        )
        .child(
            div()
                .w(px(110.0))
                .flex_shrink_0()
                .text_right()
                .child(crate::ui::tr(cx).settings().catalog_unlisted_action()),
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
                crate::ui::tr(cx).settings().page_memory_declined(),
                Tone::Neutral,
                cx,
            ));
        } else {
            if let Some(f) = instr_file {
                content_tags = content_tags.child(badge(f, Tone::Neutral, cx));
            }
            if skill_count > 0 {
                content_tags = content_tags.child(badge(
                    crate::ui::tr(cx)
                        .settings()
                        .page_memory_skill_s(skill_count),
                    Tone::Accent,
                    cx,
                ));
            }
            if item
                .info
                .as_ref()
                .is_some_and(|i| i.instructions_file.is_none() && skill_count == 0 && i.exists)
            {
                content_tags = content_tags.child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.faint))
                        .child(crate::ui::tr(cx).settings().page_memory_no_instructions()),
                );
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
                        crate::ui::Tr(lang)
                            .settings()
                            .agent_trusted_gone()
                            .to_owned(),
                        Tone::Warning,
                        cx,
                    ));
                }
                if instr_changed {
                    warns = warns.child(badge(
                        crate::ui::tr(cx)
                            .settings()
                            .page_memory_changed_asks_again(),
                        Tone::Warning,
                        cx,
                    ));
                }
                warns
            }));

        let when = format_ago_short(trust.decided_at_ms, now_ms, lang);
        let path_for_act = path.clone();
        let path_for_retrust = path;
        let action_el = h_row()
            .w(px(110.0))
            .justify_end()
            .gap(px(4.0))
            .children((instr_changed && exists).then(|| {
                btn_ghost(
                    SharedString::from(format!("trust-re-{idx}")),
                    crate::ui::tr(cx).settings().page_memory_re_trust(),
                )
                .icon(icon_xs(UiIcon::Refresh))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.trust_folder(path_for_retrust.clone(), cx);
                }))
            }))
            .child(if trust.trusted {
                btn_danger(
                    SharedString::from(format!("trust-revoke-{idx}")),
                    crate::ui::tr(cx).settings().page_memory_revoke(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.revoke_folder_trust(path_for_act.clone(), cx);
                }))
                .into_any_element()
            } else {
                btn_ghost(
                    SharedString::from(format!("trust-rm-{idx}")),
                    crate::ui::tr(cx).settings().secret_remove(),
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
        .w_full()
        .gap(px(8.0))
        .child(section_card_flush(title, Some(add_btn), rows, cx))
        .child(div().px(px(4.0)).child(intro))
}
