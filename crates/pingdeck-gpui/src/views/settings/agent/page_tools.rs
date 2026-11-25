//! Settings › 智能体 › 工具 (`SettingsSection::Tools`, prototype `ai/tools`).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::permissions::PermissionAction;
use pingdeck_desktop::core::agent::tools::ToolClass;
use pingdeck_desktop::core::app::SettingsSecret;
use pingdeck_desktop::core::model::settings::{Language, SearchBackend};

use crate::ui::{
    Anchor, Disableable as _, Popover, Tone, UiIcon, alert_banner, badge, btn_ghost, btn_secondary,
    h_row, icon_xs, list_row, palette, popover, select_pill, text_input, to_hsla, toggle_switch,
    v_col,
};

use super::AgentSettingsView;
use super::widgets::{section_card, section_card_flush, set_row};

pub(super) fn render_tools(
    view: &AgentSettingsView,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let policies = view
        .config
        .as_ref()
        .map(|c| c.tool_policies.as_slice())
        .unwrap_or(&[]);

    // Split built-ins vs internal tools vs MCP tools (built-ins live here; internal listed below).
    let mut builtin_tools: Vec<_> = view
        .tools
        .iter()
        .filter(|e| e.info.source == "builtin" && e.class != ToolClass::Internal)
        .collect();
    builtin_tools.sort_by_key(|e| (!e.info.read_only, e.info.name.clone()));

    let internal_names: Vec<&str> = view
        .tools
        .iter()
        .filter(|e| e.info.source == "builtin" && e.class == ToolClass::Internal)
        .map(|e| e.info.name.as_str())
        .collect();

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
                .w(px(210.0))
                .flex_shrink_0()
                .child(crate::ui::tr(cx).settings().tools_col_tool()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(crate::ui::tr(cx).settings().page_tools_description()),
        )
        .child(
            div()
                .w(px(138.0))
                .flex_shrink_0()
                .text_right()
                .child(crate::ui::tr(cx).settings().page_tools_policy()),
        );

    let mut table_rows = v_col().child(table_head);
    let total = builtin_tools.len();
    for (idx, entry) in builtin_tools.iter().enumerate() {
        let key = entry
            .info
            .policy_key
            .clone()
            .unwrap_or_else(|| entry.info.name.clone());
        let current = policies
            .iter()
            .find(|pol| pol.tool_key == key)
            .map(|pol| pol.policy);
        let desc_key = format!("set.f.tools.{}.d", entry.info.name);
        let desc = pingdeck_desktop::i18n::lookup(lang, &desc_key)
            .map(str::to_owned)
            .unwrap_or_else(|| entry.info.description.clone());

        let picker = render_policy_picker(idx, key, current, entry.allow_offered, lang, cx);

        let mut row =
            h_row()
                .px(px(12.0))
                .py(px(9.0))
                .gap(px(10.0))
                .child(
                    h_row()
                        .w(px(210.0))
                        .flex_shrink_0()
                        .gap(px(5.0))
                        .flex_wrap()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(SharedString::from(entry.info.name.clone())),
                        )
                        .children(entry.info.read_only.then(|| {
                            badge(crate::ui::tr(cx).shared().read_only(), Tone::Neutral, cx)
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(desc)),
                )
                .child(
                    div()
                        .w(px(138.0))
                        .flex_shrink_0()
                        .flex()
                        .justify_end()
                        .child(picker),
                );

        if idx + 1 < total {
            row = row
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)));
        }
        table_rows = table_rows.child(row);
    }

    let mut sec_tools = v_col()
        .w_full()
        .gap(px(8.0))
        .child(section_card_flush(
            crate::ui::tr(cx)
                .settings()
                .page_tools_built_in_tool_approval_policies(),
            None,
            table_rows,
            cx,
        ))
        .child(
            div()
                .px(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(
                    crate::ui::Tr(lang)
                        .settings()
                        .sec_tools_tools_d()
                        .to_owned(),
                ),
        )
        .child(
            div()
                .px(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(
                    crate::ui::Tr(lang)
                        .settings()
                        .tools_group_changes_no_allow()
                        .to_owned(),
                ),
        );

    if !internal_names.is_empty() {
        let sep = crate::ui::Tr(lang).settings().tools_internal_sep();
        let joined = internal_names.join(sep);
        sec_tools = sec_tools.child(
            div()
                .px(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(
                    crate::ui::Tr(lang).settings().tools_internal(&joined),
                )),
        );
    }

    // ── Section 2: Web search (`ai.search.*`) ────────────────────────────────
    let sec_search = render_search_section(view, lang, cx);

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(sec_tools)
        .child(sec_search)
}

fn render_policy_picker(
    idx: usize,
    tool_key: String,
    current: Option<PermissionAction>,
    allow_offered: bool,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> Popover {
    let label = policy_label(current, lang);
    let active = current.is_some();
    let ent = cx.entity().clone();

    let mut opts: Vec<Option<PermissionAction>> = vec![None];
    if allow_offered {
        opts.push(Some(PermissionAction::Allow));
    }
    opts.push(Some(PermissionAction::Ask));
    opts.push(Some(PermissionAction::Block));

    popover(SharedString::from(format!("tool-pol-pop-{idx}")))
        .anchor(Anchor::TopRight)
        .trigger(select_pill(
            SharedString::from(format!("tool-pol-btn-{idx}")),
            label,
            active,
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut col = v_col().w(px(150.0)).gap(px(2.0));
            for (oi, opt) in opts.iter().copied().enumerate() {
                let selected = opt == current;
                let ent_c = ent.clone();
                let key_c = tool_key.clone();
                let pop_h = pop_cx.entity().clone();
                col = col.child(
                    list_row(
                        SharedString::from(format!("tool-pol-opt-{idx}-{oi}")),
                        selected,
                        pop_cx,
                        move |_, window, cx| {
                            ent_c.update(cx, |this, cx| {
                                this.set_tool_policy(key_c.clone(), opt, cx);
                            });
                            pop_h.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(policy_label(opt, lang)))
                    .children(selected.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            col
        })
}

fn policy_label(action: Option<PermissionAction>, lang: Language) -> String {
    match action {
        None => crate::ui::Tr(lang)
            .settings()
            .tools_value_default()
            .to_owned(),
        Some(PermissionAction::Allow) => crate::ui::Tr(lang)
            .settings()
            .tools_value_allow()
            .to_owned(),
        Some(PermissionAction::Ask) => crate::ui::Tr(lang).settings().tools_value_ask().to_owned(),
        Some(PermissionAction::Block) => crate::ui::Tr(lang)
            .settings()
            .tools_value_block()
            .to_owned(),
    }
}

fn render_search_section(
    view: &AgentSettingsView,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let search = &view.settings.ai.search;

    let backend_label = move |b: SearchBackend| -> String {
        let s = crate::ui::Tr(lang).settings();
        match b {
            SearchBackend::Auto => s.opt_ai_search_backend_auto(),
            SearchBackend::DuckDuckGo => s.opt_ai_search_backend_duckduckgo(),
            SearchBackend::Brave => s.opt_ai_search_backend_brave(),
            SearchBackend::Tavily => s.opt_ai_search_backend_tavily(),
            SearchBackend::Searxng => s.opt_ai_search_backend_searxng(),
        }
        .to_owned()
    };

    let current_backend = search.backend;
    let ent_b = cx.entity().clone();
    let backend_picker = popover("agent-search-backend-pop")
        .anchor(Anchor::TopRight)
        .trigger(select_pill(
            "agent-search-backend-btn",
            backend_label(current_backend),
            current_backend != SearchBackend::Auto,
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let choices = [
                (SearchBackend::Auto, "auto"),
                (SearchBackend::DuckDuckGo, "duckduckgo"),
                (SearchBackend::Brave, "brave"),
                (SearchBackend::Tavily, "tavily"),
                (SearchBackend::Searxng, "searxng"),
            ];
            let mut col = v_col().w(px(190.0)).gap(px(2.0));
            for (i, (b, wire)) in choices.into_iter().enumerate() {
                let sel = b == current_backend;
                let ent_c = ent_b.clone();
                let pop_h = pop_cx.entity().clone();
                col = col.child(
                    list_row(
                        SharedString::from(format!("search-backend-opt-{i}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            ent_c.update(cx, |this, cx| {
                                this.patch_ai(
                                    serde_json::json!({ "search": { "backend": wire } }),
                                    None,
                                    cx,
                                );
                            });
                            pop_h.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(backend_label(b)))
                    .children(sel.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            col
        });

    let test_btn = btn_secondary(
        "agent-search-test",
        if view.search_testing {
            crate::ui::tr(cx).settings().network_testing().to_owned()
        } else {
            crate::ui::Tr(lang)
                .settings()
                .agent_search_test()
                .to_owned()
        },
    )
    .disabled(view.search_testing)
    .on_click(cx.listener(|this, _, _, cx| this.test_search_backend(cx)));

    let reset_search_btn = btn_ghost(
        "agent-reset-search-sec",
        crate::ui::tr(cx)
            .settings()
            .page_defaults_restore_defaults(),
    )
    .on_click(cx.listener(|this, _, window, cx| {
        this.reset_ai_keys(
            &[
                "search.backend",
                "search.searxng_base_url",
                "search.use_duckduckgo",
            ],
            window,
            cx,
        );
    }))
    .into_any_element();

    let p = palette(cx);
    let mut rows = v_col().w_full().child(set_row(
        crate::ui::Tr(lang)
            .settings()
            .f_ai_search_backend()
            .to_owned(),
        None,
        Some(SharedString::from(
            crate::ui::Tr(lang)
                .settings()
                .f_ai_search_backend_d()
                .to_owned(),
        )),
        h_row().gap(px(8.0)).child(backend_picker).child(test_btn),
        cx,
    ));

    if let Some(res) = &view.search_test_result {
        let (tone, text) = if res.ok {
            (
                Tone::Success,
                crate::ui::Tr(lang)
                    .settings()
                    .agent_search_works(res.results, res.elapsed_ms),
            )
        } else {
            (
                Tone::Warning,
                res.error.clone().unwrap_or_else(|| {
                    crate::ui::Tr(lang)
                        .settings()
                        .agent_search_no_reason()
                        .to_owned()
                }),
            )
        };
        rows = rows.child(
            div()
                .py(px(10.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(alert_banner(tone, text, None, cx)),
        );
    }

    rows = rows
        .child(render_search_key_row(
            view,
            "Tavily",
            &search.tavily_key_hint,
            SettingsSecret::SearchTavily,
            &view.tavily_key_input,
            lang,
            cx,
        ))
        .child(render_search_key_row(
            view,
            "Brave",
            &search.brave_key_hint,
            SettingsSecret::SearchBrave,
            &view.brave_key_input,
            lang,
            cx,
        ))
        .child(set_row(
            crate::ui::Tr(lang)
                .settings()
                .f_ai_search_searxng_base_url()
                .to_owned(),
            None,
            Some(SharedString::from(
                crate::ui::Tr(lang)
                    .settings()
                    .f_ai_search_searxng_base_url_d()
                    .to_owned(),
            )),
            div()
                .w(px(240.0))
                .child(text_input(&view.searxng_url_input)),
            cx,
        ))
        .child(set_row(
            crate::ui::Tr(lang)
                .settings()
                .f_ai_search_use_duckduckgo()
                .to_owned(),
            None,
            Some(SharedString::from(
                crate::ui::Tr(lang)
                    .settings()
                    .f_ai_search_use_duckduckgo_d()
                    .to_owned(),
            )),
            toggle_switch("agent-search-ddg", search.use_duckduckgo).on_change(cx.listener(
                |this, checked: &bool, _, cx| {
                    this.patch_ai(
                        serde_json::json!({ "search": { "use_duckduckgo": *checked } }),
                        None,
                        cx,
                    );
                },
            )),
            cx,
        ));

    section_card(
        crate::ui::tr(cx).settings().page_tools_web_search(),
        Some(reset_search_btn),
        rows,
        cx,
    )
}

fn render_search_key_row(
    _view: &AgentSettingsView,
    name: &'static str,
    hint: &str,
    secret: SettingsSecret,
    input: &gpui_kit::Entity<crate::ui::InputState>,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let has = !hint.is_empty();
    let label = if name == "Tavily" {
        crate::ui::Tr(lang)
            .settings()
            .f_ai_search_tavily_key_hint()
            .to_owned()
    } else {
        crate::ui::Tr(lang)
            .settings()
            .f_ai_search_brave_key_hint()
            .to_owned()
    };
    let desc = if has {
        crate::ui::Tr(lang).settings().secret_stored_hint(hint)
    } else {
        crate::ui::Tr(lang).settings().secret_empty().to_owned()
    };

    let ctl = h_row()
        .gap(px(6.0))
        .child(div().w(px(176.0)).child(text_input(input)))
        .child(
            btn_secondary(
                SharedString::from(format!("search-key-save-{name}")),
                crate::ui::Tr(lang).settings().secret_save().to_owned(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.save_search_secret(secret, false, window, cx);
            })),
        )
        .children(has.then(|| {
            btn_ghost(
                SharedString::from(format!("search-key-rm-{name}")),
                crate::ui::Tr(lang).settings().secret_remove().to_owned(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.save_search_secret(secret, true, window, cx);
            }))
        }));

    set_row(label, None, Some(SharedString::from(desc)), ctl, cx)
}
