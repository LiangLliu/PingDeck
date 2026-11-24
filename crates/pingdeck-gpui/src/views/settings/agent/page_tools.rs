//! Settings › 智能体 › 工具 (`SettingsSection::Tools`, prototype `ai/tools`).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::permissions::PermissionAction;
use pingdeck_desktop::core::agent::tools::ToolClass;
use pingdeck_desktop::core::app::SettingsSecret;
use pingdeck_desktop::core::model::settings::{Language, SearchBackend};
use pingdeck_desktop::i18n::{t, t_args};

use crate::ui::{
    Anchor, Popover, StatusShape, Tone, UiIcon, badge, btn_ghost, btn_secondary, card, h_row,
    icon_xs, list_row, palette, popover, select_pill, status_indicator, text_input, to_hsla,
    toggle_switch, v_col,
};

use super::AgentSettingsView;
use super::widgets::{section_head, set_row};

pub(super) fn render_tools(
    view: &AgentSettingsView,
    lang: Language,
    zh: bool,
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
                .child(if zh { "工具" } else { "Tool" }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(if zh { "用途说明" } else { "Description" }),
        )
        .child(
            div()
                .w(px(138.0))
                .flex_shrink_0()
                .text_right()
                .child(if zh { "默认策略" } else { "Policy" }),
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
        let localized = t(lang, &desc_key);
        let desc = if localized != desc_key {
            localized.to_owned()
        } else {
            entry.info.description.clone()
        };

        let picker = render_policy_picker(idx, key, current, entry.allow_offered, lang, cx);

        let mut row = h_row()
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
                    .children(
                        entry
                            .info
                            .read_only
                            .then(|| badge(if zh { "只读" } else { "Read" }, Tone::Neutral, cx)),
                    ),
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
        .gap(px(10.0))
        .child(section_head(
            if zh {
                "内置工具审批策略"
            } else {
                "Built-in tool approval policies"
            },
            None,
            cx,
        ))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(t(lang, "set.sec.tools.tools.d").to_owned()),
        )
        .child(card(cx).overflow_hidden().child(table_rows))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(t(lang, "set.tools.group.changes.noAllow").to_owned()),
        );

    if !internal_names.is_empty() {
        let sep = t(lang, "set.tools.internal.sep");
        let joined = internal_names.join(sep);
        sec_tools = sec_tools.child(div().text_xs().text_color(to_hsla(p.faint)).child(
            SharedString::from(t_args(
                lang,
                "set.tools.internal",
                &[("tools", joined.as_str())],
            )),
        ));
    }

    // ── Section 2: Web search (`ai.search.*`) ────────────────────────────────
    let sec_search = render_search_section(view, lang, zh, cx);

    v_col().gap(px(22.0)).child(sec_tools).child(sec_search)
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
        None => t(lang, "set.tools.value.default").to_owned(),
        Some(PermissionAction::Allow) => t(lang, "set.tools.value.allow").to_owned(),
        Some(PermissionAction::Ask) => t(lang, "set.tools.value.ask").to_owned(),
        Some(PermissionAction::Block) => t(lang, "set.tools.value.block").to_owned(),
    }
}

fn render_search_section(
    view: &AgentSettingsView,
    lang: Language,
    zh: bool,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let search = &view.settings.ai.search;

    let backend_label = move |b: SearchBackend| -> String {
        let key = match b {
            SearchBackend::Auto => "set.opt.ai.search.backend.auto",
            SearchBackend::DuckDuckGo => "set.opt.ai.search.backend.duckduckgo",
            SearchBackend::Brave => "set.opt.ai.search.backend.brave",
            SearchBackend::Tavily => "set.opt.ai.search.backend.tavily",
            SearchBackend::Searxng => "set.opt.ai.search.backend.searxng",
        };
        t(lang, key).to_owned()
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
            if zh { "正在测试…" } else { "Testing…" }.to_owned()
        } else {
            t(lang, "set.agent.search.test").to_owned()
        },
    )
    .on_click(cx.listener(|this, _, _, cx| this.test_search_backend(cx)));

    let reset_search_btn = btn_ghost(
        "agent-reset-search-sec",
        if zh {
            "恢复本区默认值"
        } else {
            "Restore defaults"
        },
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

    let mut sec = v_col()
        .child(section_head(
            if zh { "联网搜索" } else { "Web search" },
            Some(reset_search_btn),
            cx,
        ))
        .child(set_row(
            t(lang, "set.f.ai.search.backend").to_owned(),
            None,
            Some(SharedString::from(
                t(lang, "set.f.ai.search.backend.d").to_owned(),
            )),
            h_row().gap(px(8.0)).child(backend_picker).child(test_btn),
            cx,
        ));

    if let Some(res) = &view.search_test_result {
        let (shape, fg, bg, text) = if res.ok {
            let count_s = res.results.to_string();
            let ms_s = res.elapsed_ms.to_string();
            (
                StatusShape::Ok,
                to_hsla(p.ok),
                to_hsla(p.ok_soft),
                t_args(
                    lang,
                    "set.agent.search.works",
                    &[("count", count_s.as_str()), ("ms", ms_s.as_str())],
                ),
            )
        } else {
            (
                StatusShape::Warn,
                to_hsla(p.warn),
                to_hsla(p.warn_soft),
                res.error
                    .clone()
                    .unwrap_or_else(|| t(lang, "set.agent.search.noReason").to_owned()),
            )
        };
        sec = sec.child(
            h_row()
                .mt(px(8.0))
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .bg(bg)
                .text_xs()
                .text_color(fg)
                .child(status_indicator(shape, cx))
                .child(SharedString::from(text)),
        );
    }

    sec = sec
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
            t(lang, "set.f.ai.search.searxng_base_url").to_owned(),
            None,
            Some(SharedString::from(
                t(lang, "set.f.ai.search.searxng_base_url.d").to_owned(),
            )),
            div()
                .w(px(240.0))
                .child(text_input(&view.searxng_url_input)),
            cx,
        ))
        .child(set_row(
            t(lang, "set.f.ai.search.use_duckduckgo").to_owned(),
            None,
            Some(SharedString::from(
                t(lang, "set.f.ai.search.use_duckduckgo.d").to_owned(),
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

    sec
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
        t(lang, "set.f.ai.search.tavily_key_hint").to_owned()
    } else {
        t(lang, "set.f.ai.search.brave_key_hint").to_owned()
    };
    let desc = if has {
        t_args(lang, "set.secret.storedHint", &[("hint", hint)])
    } else {
        t(lang, "set.secret.empty").to_owned()
    };

    let ctl = h_row()
        .gap(px(6.0))
        .child(div().w(px(176.0)).child(text_input(input)))
        .child(
            btn_secondary(
                SharedString::from(format!("search-key-save-{name}")),
                t(lang, "set.secret.save").to_owned(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.save_search_secret(secret, false, window, cx);
            })),
        )
        .children(has.then(|| {
            btn_ghost(
                SharedString::from(format!("search-key-rm-{name}")),
                t(lang, "set.secret.remove").to_owned(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.save_search_secret(secret, true, window, cx);
            }))
        }));

    set_row(label, None, Some(SharedString::from(desc)), ctl, cx)
}
