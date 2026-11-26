//! Master sidebar pane listing workspace Environments with search, import, guard badges, and counts.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_icon, btn_primary, empty_inline, h_row,
    icon_xs, list_row, master_pane, meta_pill, palette, pane_footer, pane_header, search_input,
    section_heading, to_hsla, v_col,
};

use super::state::{EnvModal, EnvResourceTab, EnvironmentsView};

pub fn render_env_sidebar(
    view: &EnvironmentsView,
    cx: &mut Context<EnvironmentsView>,
) -> AnyElement {
    let p = palette(cx);
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
                .tooltip(
                    crate::ui::Tr(view.lang)
                        .environments()
                        .sidebar_import_postman_environment_json(),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.import_editor
                        .update(cx, |s, cx| s.set_value("", window, cx));
                    this.active_modal = Some(EnvModal::ImportEnv);
                    cx.notify();
                })),
        )
        .child(
            btn_primary(
                "env-new-btn",
                crate::ui::Tr(view.lang).environments().sidebar_new(),
            )
            .icon(icon_xs(UiIcon::Plus))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_create_env_modal(window, cx);
            })),
        )
        .into_any_element();

    let header = pane_header(
        crate::ui::Tr(view.lang)
            .environments()
            .sidebar_environments(),
        Some(view.environments.len()),
        Some(actions),
        Some(search_input(&view.env_search_input, cx).into_any_element()),
        cx,
    );

    let mut list = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if filtered.is_empty() {
        list = list.child(empty_inline(
            if view.environments.is_empty() {
                crate::ui::Tr(view.lang)
                    .environments()
                    .sidebar_no_environments_yet()
            } else {
                crate::ui::Tr(view.lang)
                    .environments()
                    .sidebar_no_matching_environments()
            },
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .environments()
                    .sidebar_create_dev_staging_prod_environments(),
            )),
            None,
            cx,
        ));
    } else {
        for env in filtered {
            let eid = env.id.clone();
            let active = view.resource_tab == EnvResourceTab::Environments
                && view.selected_env_id.as_deref() == Some(env.id.as_str());
            let is_workspace_active = view.active_env_id.as_deref() == Some(env.id.as_str());
            let secret_count = env.variables.iter().filter(|v| v.is_secret).count();
            let e_tr = crate::ui::Tr(view.lang).environments();
            let sub = if secret_count > 0 {
                e_tr.sidebar_vars_secrets_count(env.variables.len(), secret_count)
            } else {
                e_tr.sidebar_vars_count(env.variables.len())
            };

            let mut row = list_row(
                SharedString::from(format!("env-row-{}", env.id)),
                active,
                cx,
                cx.listener(move |this, _, _, cx| {
                    this.resource_tab = EnvResourceTab::Environments;
                    this.select_env(eid.clone(), cx);
                }),
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
                                    .text_size(px(13.0))
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(env.name.clone()),
                            )
                            .children(is_workspace_active.then(|| {
                                badge(
                                    crate::ui::Tr(view.lang).environments().sidebar_active(),
                                    Tone::Success,
                                    cx,
                                )
                            }))
                            .children(env.confirm_before_send.then(|| {
                                badge(
                                    crate::ui::Tr(view.lang).environments().sidebar_guard(),
                                    Tone::Warning,
                                    cx,
                                )
                            })),
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

    // Unified Workspace Shared resources section (Globals, Cookies, TLS Certificates).
    let shared_items = [
        (
            EnvResourceTab::Globals,
            UiIcon::Globe,
            crate::ui::Tr(view.lang).environments().globals_title(),
            view.globals_draft.len(),
        ),
        (
            EnvResourceTab::Cookies,
            UiIcon::Cookie,
            "Cookies",
            view.cookies.len(),
        ),
        (
            EnvResourceTab::Certificates,
            UiIcon::Shield,
            crate::ui::Tr(view.lang)
                .environments()
                .sidebar_tls_certificates(),
            view.certificates.len(),
        ),
    ];
    list = list
        .child(
            div()
                .my(px(6.0))
                .w_full()
                .h(px(1.0))
                .bg(to_hsla(p.line.composite_over(p.pane))),
        )
        .child(
            div().px(px(6.0)).pb(px(4.0)).child(section_heading(
                crate::ui::Tr(view.lang)
                    .environments()
                    .sidebar_workspace_shared(),
                cx,
            )),
        );
    for (tab, ic, label, count) in shared_items {
        let selected = view.resource_tab == tab;
        list = list.child(
            list_row(
                SharedString::from(format!("env-shared-{}", tab.index())),
                selected,
                cx,
                cx.listener(move |this, _, _, cx| {
                    this.resource_tab = tab;
                    this.vars_page = 0;
                    this.cookies_page = 0;
                    if let Some(ws) = this.workspace_id.clone() {
                        match tab {
                            EnvResourceTab::Globals => this.reload_globals(ws, cx),
                            EnvResourceTab::Cookies => this.reload_cookies(ws, cx),
                            EnvResourceTab::Certificates => this.reload_certificates(ws, cx),
                            EnvResourceTab::Environments => this.refresh_env_analysis(cx),
                        }
                    }
                    cx.notify();
                }),
            )
            .py(px(6.0))
            .child(icon_xs(ic))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(13.0))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(to_hsla(p.ink))
                    .truncate()
                    .child(label),
            )
            .child(meta_pill(count.to_string(), cx)),
        );
    }

    let footer = pane_footer(
        div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.muted))
            .child(
                crate::ui::tr(cx)
                    .environments()
                    .sidebar_environments_2(view.environments.len()),
            ),
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
