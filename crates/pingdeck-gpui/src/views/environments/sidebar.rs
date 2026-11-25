//! Master sidebar pane listing workspace Environments with search, import, guard badges, and counts.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_icon, btn_primary, empty_inline, h_row,
    icon_xs, list_row, master_pane, meta_pill, palette, pane_footer, pane_header, search_input,
    to_hsla, v_col,
};

use super::state::{EnvModal, EnvironmentsView};

pub fn render_env_sidebar(
    view: &EnvironmentsView,
    cx: &mut Context<EnvironmentsView>,
) -> AnyElement {
    let p = palette(cx);
    let zh = view.is_zh();
    let q = view.env_search_input.read(cx).value().trim().to_lowercase();

    let filtered: Vec<_> = view
        .environments
        .iter()
        .filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q))
        .collect();

    let actions = h_row()
        .gap(px(4.0))
        .child(
            btn_icon("env-import-btn", UiIcon::Upload)
                .tooltip(view.t("导入 Postman 环境 JSON", "Import Postman Environment JSON"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.import_editor
                        .update(cx, |s, cx| s.set_value("", window, cx));
                    this.active_modal = Some(EnvModal::ImportEnv);
                    cx.notify();
                })),
        )
        .child(
            btn_primary("env-new-btn", view.t("新建", "New"))
                .icon(icon_xs(UiIcon::Plus))
                .on_click(cx.listener(|this, _, _, cx| this.create_env(cx))),
        )
        .into_any_element();

    let header = pane_header(
        view.t("环境列表", "Environments"),
        Some(view.environments.len()),
        Some(actions),
        Some(search_input(&view.env_search_input, cx).into_any_element()),
        cx,
    );

    let mut list = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if filtered.is_empty() {
        list = list.child(empty_inline(
            if view.environments.is_empty() {
                view.t("暂无环境", "No environments yet")
            } else {
                view.t("没有匹配的环境", "No matching environments")
            },
            Some(SharedString::from(view.t(
                "创建 dev / staging / prod 环境管理变量、机密与发送前保护。",
                "Create dev / staging / prod environments for variables, secrets, and guards.",
            ))),
            None,
            cx,
        ));
    } else {
        for env in filtered {
            let eid = env.id.clone();
            let active = view.selected_env_id.as_deref() == Some(env.id.as_str());
            let secret_count = env.variables.iter().filter(|v| v.is_secret).count();
            let sub = if zh {
                format!(
                    "{} 个变量{}",
                    env.variables.len(),
                    if secret_count > 0 {
                        format!(" · {secret_count} 机密")
                    } else {
                        String::new()
                    }
                )
            } else {
                format!(
                    "{} vars{}",
                    env.variables.len(),
                    if secret_count > 0 {
                        format!(" · {secret_count} secrets")
                    } else {
                        String::new()
                    }
                )
            };

            let mut row = list_row(
                SharedString::from(format!("env-row-{}", env.id)),
                active,
                cx,
                cx.listener(move |this, _, _, cx| this.select_env(eid.clone(), cx)),
            )
            .items_start()
            .py(px(7.0))
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.0))
                    .child(
                        h_row()
                            .w_full()
                            .justify_between()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(env.name.clone()),
                            )
                            .children(
                                env.confirm_before_send
                                    .then(|| badge(view.t("保护", "Guard"), Tone::Warning, cx)),
                            ),
                    )
                    .child(meta_pill(sub, cx)),
            );
            if active && view.env_dirty {
                row = row.child(
                    div()
                        .size(px(6.0))
                        .mt(px(5.0))
                        .rounded(px(999.0))
                        .bg(to_hsla(p.accent))
                        .flex_shrink_0(),
                );
            }
            list = list.child(row);
        }
    }

    let footer = pane_footer(
        div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.muted))
            .child(if zh {
                format!("共 {} 个环境", view.environments.len())
            } else {
                format!("{} environments", view.environments.len())
            }),
        Some(
            btn_icon("env-refresh-btn", UiIcon::Refresh)
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(ws) = this.workspace_id.clone() {
                        this.reload_environments(ws, cx);
                    }
                }))
                .into_any_element(),
        ),
        cx,
    );

    master_pane(
        header,
        div()
            .id("env-sidebar-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(list),
        Some(footer.into_any_element()),
        cx,
    )
    .into_any_element()
}
