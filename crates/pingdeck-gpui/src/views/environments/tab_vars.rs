//! Editable variable table shared by Environment Variables and Workspace Globals.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_icon, btn_primary,
    btn_secondary, card, checkbox, choice_chip, data_row, empty_inline, h_row, icon_xs, meta_pill,
    palette, search_input, section_header, table_header, text_input, to_hsla, v_col,
};

use super::state::{EditableVarRow, EnvDetailTab, EnvModal, EnvironmentsView};

pub fn render_variables_panel(
    view: &EnvironmentsView,
    is_global: bool,
    cx: &mut Context<EnvironmentsView>,
) -> AnyElement {
    let p = palette(cx);
    let rows = if is_global {
        &view.globals_draft
    } else {
        &view.env_draft_vars
    };
    let q = view.var_filter_input.read(cx).value().trim().to_lowercase();
    let owner_env_id = if is_global {
        None
    } else {
        view.selected_env_id.clone()
    };

    let mut content = v_col().w_full().p(px(16.0)).gap(px(12.0));

    if is_global {
        content = content.child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .w(px(240.0))
                        .child(search_input(&view.var_filter_input, cx)),
                )
                .child(if view.globals_dirty {
                    btn_primary("glob-save-btn", view.t("保存全局变量", "Save Globals"))
                        .icon(icon_xs(UiIcon::Check))
                        .on_click(cx.listener(|this, _, _, cx| this.save_globals(cx)))
                } else {
                    btn_secondary("glob-save-btn", view.t("已保存", "Saved"))
                        .on_click(cx.listener(|this, _, _, cx| this.save_globals(cx)))
                }),
        );
    } else {
        content = content.child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .w(px(240.0))
                        .child(search_input(&view.var_filter_input, cx)),
                )
                .child(meta_pill(
                    view.t(
                        "机密变量保存在 0600 权限明文 .secrets 文件，永不向界面回传明文",
                        "Secrets live in owner-only 0600 plaintext .secrets and never echo to UI",
                    ),
                    cx,
                )),
        );
    }

    let mut tbl = card(cx).p(px(12.0)).gap(px(6.0)).child(table_header(
        &[
            ("", Some(26.0)),
            (view.t("变量名 Key", "Key"), Some(180.0)),
            (view.t("值 Value", "Value"), None),
            (view.t("备注 Note", "Note"), Some(140.0)),
            (view.t("操作", "Actions"), Some(100.0)),
        ],
        cx,
    ));

    let mut visible_count = 0usize;
    for (idx, row) in rows.iter().enumerate() {
        if !q.is_empty()
            && !row.key.to_lowercase().contains(&q)
            && !row.note.to_lowercase().contains(&q)
            && !row
                .value
                .as_deref()
                .is_some_and(|v| v.to_lowercase().contains(&q))
        {
            continue;
        }
        visible_count += 1;
        let val_display = if row.is_secret {
            if let Some(new_v) = &row.value
                && !new_v.is_empty()
            {
                view.t("••••（已更新，待保存）", "•••• (pending save)")
                    .to_owned()
            } else if let Some(hint) = &row.secret_hint {
                hint.clone()
            } else if row.has_secret {
                "••••••••".to_owned()
            } else {
                view.t("（未设置机密值）", "(empty secret)").to_owned()
            }
        } else {
            row.value.clone().unwrap_or_default()
        };

        let env_for_copy = owner_env_id.clone();
        let var_id_copy = row.id.clone();
        let is_sec = row.is_secret;
        let can_copy_sec = row.is_secret && row.has_secret && var_id_copy.is_some();
        let row_key_edit = row.key.clone();
        let row_val_edit = if row.is_secret {
            String::new()
        } else {
            row.value.clone().unwrap_or_default()
        };
        let row_note_edit = row.note.clone();

        tbl = tbl.child(
            data_row(cx)
                .child(
                    div().w(px(26.0)).child(
                        checkbox(
                            SharedString::from(format!("var-en-{is_global}-{idx}")),
                            row.enabled,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let list = if is_global {
                                &mut this.globals_draft
                            } else {
                                &mut this.env_draft_vars
                            };
                            if let Some(item) = list.get_mut(idx) {
                                item.enabled = !item.enabled;
                                if is_global {
                                    this.globals_dirty = true;
                                } else {
                                    this.env_dirty = true;
                                }
                                cx.notify();
                            }
                        })),
                    ),
                )
                .child(
                    h_row()
                        .w(px(180.0))
                        .gap(px(4.0))
                        .flex_shrink_0()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(if row.enabled {
                                    to_hsla(p.ink)
                                } else {
                                    to_hsla(p.muted)
                                })
                                .truncate()
                                .child(row.key.clone()),
                        )
                        .children(
                            row.is_secret
                                .then(|| badge(view.t("机密", "Secret"), Tone::Warning, cx)),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(val_display),
                )
                .child(
                    div()
                        .w(px(140.0))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(if row.note.is_empty() {
                            "—".to_owned()
                        } else {
                            row.note.clone()
                        }),
                )
                .child(
                    h_row()
                        .w(px(100.0))
                        .flex_shrink_0()
                        .justify_end()
                        .gap(px(2.0))
                        .children(can_copy_sec.then(|| {
                            let vid = var_id_copy.clone().unwrap_or_default();
                            btn_icon(
                                SharedString::from(format!("var-cpsec-{is_global}-{idx}")),
                                UiIcon::Copy,
                            )
                            .tooltip(view.t("直接复制机密值到剪贴板", "Copy secret to clipboard"))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.copy_secret_variable(
                                        env_for_copy.clone(),
                                        vid.clone(),
                                        cx,
                                    );
                                },
                            ))
                        }))
                        .child(
                            btn_icon(
                                SharedString::from(format!("var-ed-{is_global}-{idx}")),
                                UiIcon::Pencil,
                            )
                            .tooltip(view.t("编辑变量", "Edit variable"))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.edit_var_key.update(cx, |s, cx| {
                                        s.set_value(row_key_edit.clone(), window, cx);
                                    });
                                    this.edit_var_val.update(cx, |s, cx| {
                                        s.set_value(row_val_edit.clone(), window, cx);
                                    });
                                    this.edit_var_note.update(cx, |s, cx| {
                                        s.set_value(row_note_edit.clone(), window, cx);
                                    });
                                    this.active_modal = Some(EnvModal::EditVar {
                                        is_global,
                                        index: idx,
                                        is_secret: is_sec,
                                    });
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            btn_icon(
                                SharedString::from(format!("var-rm-{is_global}-{idx}")),
                                UiIcon::Trash,
                            )
                            .tooltip(view.t("删除变量", "Delete variable"))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let list = if is_global {
                                        &mut this.globals_draft
                                    } else {
                                        &mut this.env_draft_vars
                                    };
                                    if idx < list.len() {
                                        list.remove(idx);
                                        if is_global {
                                            this.globals_dirty = true;
                                        } else {
                                            this.env_dirty = true;
                                        }
                                        cx.notify();
                                    }
                                },
                            )),
                        ),
                ),
        );
    }

    if visible_count == 0 {
        tbl = tbl.child(empty_inline(
            view.t("暂无变量", "No variables yet"),
            Some(SharedString::from(view.t(
                "在下方添加变量后点击保存即可在 {{var}} 模板中生效。",
                "Add variables below and save to resolve {{var}} across requests.",
            ))),
            None,
            cx,
        ));
    }

    // Add new variable strip
    let add_card = card(cx)
        .p(px(12.0))
        .gap(px(8.0))
        .child(section_header(
            view.t("添加新变量", "Add Variable"),
            Some(
                choice_chip(
                    SharedString::from(format!("var-new-sec-{is_global}")),
                    view.t("设为机密变量", "Mark as Secret"),
                    view.new_var_secret,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.new_var_secret = !this.new_var_secret;
                    cx.notify();
                }))
                .into_any_element(),
            ),
            cx,
        ))
        .child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .child(div().w(px(170.0)).child(text_input(&view.new_var_key)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(text_input(&view.new_var_val)),
                )
                .child(div().w(px(150.0)).child(text_input(&view.new_var_note)))
                .child(
                    btn_secondary(
                        SharedString::from(format!("var-add-btn-{is_global}")),
                        view.t("+ 添加", "+ Add"),
                    )
                    .icon(icon_xs(UiIcon::Plus))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let key = this.new_var_key.read(cx).value().trim().to_owned();
                        let val = this.new_var_val.read(cx).value().to_string();
                        let note = this.new_var_note.read(cx).value().trim().to_owned();
                        if key.is_empty() {
                            return;
                        }
                        let is_secret = this.new_var_secret;
                        let list = if is_global {
                            &mut this.globals_draft
                        } else {
                            &mut this.env_draft_vars
                        };
                        if list.iter().any(|r| r.key == key) {
                            this.status_msg = Some((
                                Tone::Warning,
                                this.t(
                                    "同名变量已存在，请直接编辑该行",
                                    "Key already exists — edit the existing row",
                                )
                                .to_owned(),
                            ));
                            cx.notify();
                            return;
                        }
                        list.push(EditableVarRow {
                            id: None,
                            key,
                            value: Some(val.clone()),
                            is_secret,
                            has_secret: is_secret && !val.is_empty(),
                            secret_hint: None,
                            enabled: true,
                            note,
                        });
                        if is_global {
                            this.globals_dirty = true;
                        } else {
                            this.env_dirty = true;
                        }
                        this.new_var_key
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        this.new_var_val
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        this.new_var_note
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        cx.notify();
                    })),
                ),
        );

    let mut page = content.child(tbl).child(add_card);
    if !is_global {
        let secret_cnt = rows.iter().filter(|r| r.is_secret).count();
        let unres_cnt = view.unresolved_vars.len();
        page = page.child(
            h_row()
                .w_full()
                .gap(px(10.0))
                .items_stretch()
                .child(
                    card(cx)
                        .flex_1()
                        .min_w_0()
                        .p(px(12.0))
                        .gap(px(6.0))
                        .child(section_header(
                            view.t("未解析变量扫描", "Unresolved Variables"),
                            Some(
                                badge(
                                    unres_cnt.to_string(),
                                    if unres_cnt == 0 {
                                        Tone::Success
                                    } else {
                                        Tone::Warning
                                    },
                                    cx,
                                )
                                .into_any_element(),
                            ),
                            cx,
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(if unres_cnt == 0 {
                                    view.t(
                                        "工作区所有请求的 {{var}} 均可在此环境下解析。",
                                        "All {{var}} references resolve cleanly.",
                                    )
                                } else {
                                    view.t(
                                        "部分已保存请求引用了未定义的 {{var}}，点击前往一键定义。",
                                        "Some saved requests reference missing {{var}} placeholders.",
                                    )
                                }),
                        )
                        .child(
                            div().pt(px(2.0)).child(
                                btn_secondary(
                                    "env-goto-resolved",
                                    view.t("查看解析顺序与扫描 →", "Open Resolved & Scan →"),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.detail_tab = EnvDetailTab::Resolved;
                                    this.refresh_env_analysis(cx);
                                    cx.notify();
                                })),
                            ),
                        ),
                )
                .child(
                    card(cx)
                        .flex_1()
                        .min_w_0()
                        .p(px(12.0))
                        .gap(px(6.0))
                        .child(section_header(
                            view.t("本地 .secrets 明文库", "Local .secrets Store"),
                            Some(meta_pill(format!("{secret_cnt} secrets"), cx).into_any_element()),
                            cx,
                        ))
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(view.t(
                            "机密变量值保存在本机 0600 权限明文 SQLite (.secrets) 中，不在界面明文回显，仅在发送时由核心注入。",
                            "Secret values live in owner-only 0600 plaintext SQLite (.secrets), never echoed to UI, injected by core at send time.",
                        ))),
                ),
        );
    }
    if (is_global && view.globals_dirty) || (!is_global && view.env_dirty) {
        page = page.child(alert_banner(
            Tone::Accent,
            view.t(
                "有未保存的更改，点击右上角保存按钮写入数据库。",
                "You have unsaved changes — click Save to persist.",
            ),
            None,
            cx,
        ));
    }

    div()
        .id(SharedString::from(format!("env-vars-scroll-{is_global}")))
        .size_full()
        .overflow_y_scrollbar()
        .child(page)
        .into_any_element()
}
