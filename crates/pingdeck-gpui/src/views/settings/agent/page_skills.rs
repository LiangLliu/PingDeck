//! Settings › 智能体 › 技能 (`SettingsSection::Skills`, prototype `ai/skills`).

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::Language;

use crate::ui::{
    Tone, UiIcon, badge, btn_danger, btn_ghost, btn_primary, btn_secondary, empty_state, h_row,
    icon_xs, palette, settings_sub_well, to_hsla, toggle_switch, v_col,
};

use super::AgentSettingsView;
use super::widgets::{section_card_flush, section_card_padded};

pub(super) fn render_skills(
    view: &AgentSettingsView,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);

    let toolbar = h_row()
        .gap(px(8.0))
        .flex_wrap()
        .child(
            btn_secondary(
                "skills-rescan",
                crate::ui::Tr(lang).settings().skills_rescan().to_owned(),
            )
            .icon(icon_xs(UiIcon::Refresh))
            .on_click(cx.listener(|this, _, _, cx| this.rescan_skills(cx))),
        )
        .child(
            btn_primary(
                "skills-add-dir",
                crate::ui::tr(cx).settings().page_skills_add_folder(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.pick_and_add_skill_dir(cx))),
        )
        .child(
            btn_secondary(
                "skills-import",
                crate::ui::tr(cx).settings().page_skills_import_skill(),
            )
            .icon(icon_xs(UiIcon::Upload))
            .on_click(cx.listener(|this, _, _, cx| this.pick_and_import_skill(cx))),
        );

    // ── Section 1: Skill folders ─────────────────────────────────────────────
    let mut folder_rows = v_col();
    let folders = &view.skill_folders;
    for (idx, f) in folders.iter().enumerate() {
        let path_open = f.path.clone();
        let path_rm = f.path.clone();
        let status_badge = if !f.scanned {
            badge(
                crate::ui::Tr(lang).settings().skills_not_found().to_owned(),
                Tone::Warning,
                cx,
            )
        } else if f.skill_count == 0 {
            badge(
                crate::ui::Tr(lang)
                    .settings()
                    .skills_folder_empty()
                    .to_owned(),
                Tone::Neutral,
                cx,
            )
        } else {
            badge(
                crate::ui::tr(cx)
                    .settings()
                    .page_skills_skill_s(f.skill_count),
                Tone::Accent,
                cx,
            )
        };

        let mut row = h_row()
            .justify_between()
            .gap(px(10.0))
            .px(px(12.0))
            .py(px(9.0))
            .child(
                h_row()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_color(to_hsla(p.muted))
                            .child(icon_xs(UiIcon::Folder)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .child(SharedString::from(f.path.clone())),
                    )
                    .child(status_badge),
            )
            .child(
                h_row()
                    .gap(px(4.0))
                    .flex_shrink_0()
                    .children(f.scanned.then(|| {
                        btn_ghost(
                            SharedString::from(format!("skill-dir-open-{idx}")),
                            crate::ui::Tr(lang).settings().skills_open().to_owned(),
                        )
                        .icon(icon_xs(UiIcon::FolderOpen))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reveal_folder(path_open.clone(), cx);
                        }))
                    }))
                    .child(
                        btn_ghost(
                            SharedString::from(format!("skill-dir-rm-{idx}")),
                            crate::ui::tr(cx).settings().secret_remove(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_skill_dir(&path_rm, cx);
                        })),
                    ),
            );
        if idx + 1 < folders.len() {
            row = row
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)));
        }
        folder_rows = folder_rows.child(row);
    }

    let sec_folders = v_col()
        .w_full()
        .gap(px(8.0))
        .child(if folders.is_empty() {
            section_card_padded(
                crate::ui::tr(cx).settings().page_skills_skill_folders(),
                Some(toolbar.into_any_element()),
                div().text_xs().text_color(to_hsla(p.muted)).child(
                    crate::ui::Tr(lang)
                        .settings()
                        .sec_skills_folders_d()
                        .to_owned(),
                ),
                cx,
            )
        } else {
            section_card_flush(
                crate::ui::tr(cx).settings().page_skills_skill_folders(),
                Some(toolbar.into_any_element()),
                folder_rows,
                cx,
            )
        })
        .children((!folders.is_empty()).then(|| {
            div()
                .px(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(
                    crate::ui::Tr(lang)
                        .settings()
                        .sec_skills_folders_d()
                        .to_owned(),
                )
        }));

    // ── Section 2: Discovered & imported skills table ────────────────────────
    let table_section = if view.skill_entries.is_empty() {
        section_card_padded(
            crate::ui::tr(cx).settings().page_skills_discovered_skills(),
            None,
            settings_sub_well(cx).h(px(168.0)).child(empty_state(
                UiIcon::Sparkles,
                crate::ui::Tr(lang)
                    .settings()
                    .skills_empty_title()
                    .to_owned(),
                crate::ui::tr(cx)
                    .settings()
                    .page_skills_put_a_folder_holding_skill(),
                cx,
            )),
            cx,
        )
    } else {
        let head = h_row()
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
                    .child(crate::ui::tr(cx).settings().page_skills_skill()),
            )
            .child(
                div()
                    .w(px(170.0))
                    .flex_shrink_0()
                    .child(crate::ui::tr(cx).settings().page_skills_source()),
            )
            .child(
                div()
                    .w(px(140.0))
                    .flex_shrink_0()
                    .child(crate::ui::tr(cx).settings().page_skills_trigger()),
            )
            .child(
                div()
                    .w(px(110.0))
                    .flex_shrink_0()
                    .text_right()
                    .child(crate::ui::tr(cx).common().kv_enabled()),
            );

        let mut rows = v_col().child(head);
        let total = view.skill_entries.len();
        for (idx, entry) in view.skill_entries.iter().enumerate() {
            let sk = &entry.skill;
            let is_managed = sk.origin == "managed";
            let name_tog = sk.name.clone();
            let name_rm = sk.name.clone();

            let origin_cell = if is_managed {
                badge(
                    crate::ui::tr(cx).settings().page_skills_imported(),
                    Tone::Accent,
                    cx,
                )
            } else {
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .truncate()
                    .child(SharedString::from(sk.origin.clone()))
            };

            let mut row = h_row()
                .px(px(12.0))
                .py(px(10.0))
                .gap(px(10.0))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.0))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .child(SharedString::from(sk.name.clone())),
                                )
                                .children(entry.invalid.is_some().then(|| {
                                    badge(
                                        crate::ui::tr(cx)
                                            .settings()
                                            .page_skills_missing_description(),
                                        Tone::Warning,
                                        cx,
                                    )
                                })),
                        )
                        .children((!sk.description.trim().is_empty()).then(|| {
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(SharedString::from(sk.description.clone()))
                        })),
                )
                .child(div().w(px(170.0)).min_w_0().child(origin_cell))
                .child(
                    div()
                        .w(px(140.0))
                        .child(badge(format!("/{}", sk.name), Tone::Neutral, cx)),
                )
                .child(
                    h_row()
                        .w(px(110.0))
                        .justify_end()
                        .gap(px(6.0))
                        .children(is_managed.then(|| {
                            btn_danger(
                                SharedString::from(format!("skill-rm-{idx}")),
                                crate::ui::Tr(lang).settings().skills_remove().to_owned(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.remove_imported_skill(name_rm.clone(), cx);
                                },
                            ))
                        }))
                        .child(
                            toggle_switch(
                                SharedString::from(format!("skill-en-{idx}")),
                                sk.enabled,
                            )
                            .on_change(cx.listener(
                                move |this, checked: &bool, _, cx| {
                                    this.toggle_skill(name_tog.clone(), *checked, cx);
                                },
                            )),
                        ),
                );
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
            .child(section_card_flush(
                crate::ui::tr(cx)
                    .settings()
                    .page_skills_discovered_skills_2(total),
                None,
                rows,
                cx,
            ))
            .child(
                div()
                    .px(px(4.0))
                    .text_xs()
                    .text_color(to_hsla(p.faint))
                    .child(
                        crate::ui::Tr(lang)
                            .settings()
                            .skills_toggle_hint()
                            .to_owned(),
                    ),
            )
    };

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(sec_folders)
        .child(table_section)
}
