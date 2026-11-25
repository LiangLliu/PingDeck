//! Shared layout primitives and model picker popover matching the PingDeck Settings prototype
//! (`docs/design/prototype/js/data-settings.js` & `styles/modules.css`).

use gpui_kit::{
    App, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use pingdeck_desktop::core::agent::mcp::{AgentMcpServerView, McpServerStatus};
use pingdeck_desktop::core::app::ProviderView;
use pingdeck_desktop::core::llm::store::ProviderModel;
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::t;

use crate::ui::{
    Anchor, InputState, Popover, ScrollableElement as _, StatusShape, Tone, UiIcon,
    dismissible_banner, h_row, icon_xs, list_row, number_field, palette, popover, section_header,
    select_pill, setting_row, to_hsla, v_col,
};

use super::AgentSettingsView;

const MAX_FALLBACKS: usize = 5;

/// Flattened enabled chat model candidate for model pickers and fallback chains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
}

impl ModelChoice {
    /// Canonical id reference (`provider_id/model_id`) stored in `role_models` and `fallbacks`.
    #[must_use]
    pub fn id_ref(&self) -> String {
        format!("{}/{}", self.provider_id, self.model_id)
    }

    /// Human-readable `provider_name / model_id` display label.
    #[must_use]
    pub fn display(&self) -> String {
        format!("{} / {}", self.provider_name, self.model_id)
    }
}

/// Builds enabled model choices from `AiConfigDto` providers and models, sorted by provider order.
pub fn collect_model_choices(
    providers: &[ProviderView],
    models: &[ProviderModel],
) -> Vec<ModelChoice> {
    let mut out = Vec::new();
    for pv in providers {
        let p = &pv.provider;
        if !p.enabled {
            continue;
        }
        let mut p_models: Vec<&ProviderModel> = models
            .iter()
            .filter(|m| m.provider_id == p.id && m.enabled)
            .collect();
        p_models.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        for m in p_models {
            out.push(ModelChoice {
                provider_id: p.id.clone(),
                provider_name: p.name.clone(),
                model_id: m.model_id.clone(),
            });
        }
    }
    out
}

/// Resolves a stored `provider_id/model_id` ref to human-readable `provider_name / model_id`.
#[must_use]
pub fn display_model_ref(raw: &str, providers: &[ProviderView]) -> String {
    if let Some((pid, mid)) = raw.split_once('/')
        && let Some(pv) = providers.iter().find(|p| p.provider.id == pid)
    {
        return format!("{} / {mid}", pv.provider.name);
    }
    raw.to_owned()
}

/// Prototype `.set-section-head`: delegates to unified `crate::ui::section_header`.
pub fn section_head(
    title: impl Into<SharedString>,
    right: Option<gpui_kit::AnyElement>,
    cx: &App,
) -> gpui_kit::Div {
    section_header(title, right, cx)
}

/// Prototype `.set-row`: delegates to unified `crate::ui::setting_row`.
pub fn set_row(
    label: impl Into<SharedString>,
    meta: Option<SharedString>,
    desc: Option<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> gpui_kit::Div {
    setting_row(label, meta, desc, control, cx)
}

/// Compact numeric input paired with a muted trailing unit label: delegates to `crate::ui::number_field`.
pub fn num_box(
    input: &Entity<InputState>,
    unit: Option<&'static str>,
    width_px: f32,
    cx: &App,
) -> gpui_kit::Div {
    number_field(input, unit, width_px, cx)
}

/// Inline dismissible flash banner: delegates to unified `crate::ui::dismissible_banner`.
pub fn flash_banner(
    tone: Tone,
    msg: &str,
    on_dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> gpui_kit::Div {
    dismissible_banner("agent-flash-dismiss", tone, msg.to_owned(), on_dismiss, cx)
}

/// Scrollable bounded-height model picker popover (`provider / model`) with optional clear/auto row.
#[allow(clippy::too_many_arguments)]
pub fn model_picker_popover(
    pop_id: impl Into<SharedString>,
    btn_id: impl Into<SharedString>,
    trigger_label: impl Into<SharedString>,
    active_ref: Option<String>,
    clear_option: Option<SharedString>,
    choices: Vec<ModelChoice>,
    lang: Language,
    on_pick: impl Fn(Option<ModelChoice>, &mut App) + Clone + 'static,
    cx: &App,
) -> Popover {
    let pop_id_s: SharedString = pop_id.into();
    let btn_id_s: SharedString = btn_id.into();
    let active = active_ref.is_some();
    let zh = lang == Language::ZhCn;

    popover(pop_id_s.clone())
        .anchor(Anchor::TopRight)
        .trigger(select_pill(btn_id_s, trigger_label, active, cx))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let extra = usize::from(clear_option.is_some());
            let rows_n = choices.len().min(80) + extra;
            let list_h = ((rows_n as f32) * 30.0 + 12.0).clamp(56.0, 320.0);
            let mut list = v_col().w(px(300.0)).gap(px(2.0)).p(px(4.0));

            if let Some(clear_lbl) = &clear_option {
                let selected = active_ref.is_none();
                let on_pick_c = on_pick.clone();
                let pop_h = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("{pop_id_s}-clear")),
                        selected,
                        pop_cx,
                        move |_, window, cx| {
                            on_pick_c(None, cx);
                            pop_h.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(pop_p.muted))
                            .child(clear_lbl.clone()),
                    )
                    .children(selected.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }

            if choices.is_empty() {
                list = list.child(
                    div()
                        .p(px(10.0))
                        .text_xs()
                        .text_color(to_hsla(pop_p.muted))
                        .child(if zh {
                            "没有可用模型（请先在「服务商」启用服务商与模型）"
                        } else {
                            "No enabled models (add a provider and enable models first)"
                        }),
                );
            } else {
                for (i, choice) in choices.iter().take(80).enumerate() {
                    let id_ref = choice.id_ref();
                    let selected = active_ref.as_deref() == Some(id_ref.as_str());
                    let choice_c = choice.clone();
                    let on_pick_c = on_pick.clone();
                    let pop_h = pop_cx.entity().clone();
                    list = list.child(
                        list_row(
                            SharedString::from(format!("{pop_id_s}-opt-{i}")),
                            selected,
                            pop_cx,
                            move |_, window, cx| {
                                on_pick_c(Some(choice_c.clone()), cx);
                                pop_h.update(cx, |st, cx| st.dismiss(window, cx));
                            },
                        )
                        .justify_between()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .child(SharedString::from(choice.display())),
                        )
                        .children(selected.then(|| {
                            div()
                                .text_color(to_hsla(pop_p.accent))
                                .child(icon_xs(UiIcon::Check))
                        })),
                    );
                }
            }

            div()
                .id(SharedString::from(format!("{pop_id_s}-scroll")))
                .w(px(308.0))
                .h(px(list_h))
                .overflow_y_scrollbar()
                .child(list)
        })
}

/// Reusable ordered fallback chain chip list + `+ Add fallback` scrollable model picker popover.
pub fn render_fallback_chain(
    view: &AgentSettingsView,
    site: &'static str,
    chain: &[String],
    providers: &[ProviderView],
    lang: Language,
    zh: bool,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut row = h_row().gap(px(6.0)).flex_wrap().justify_end();

    for (idx, id_ref) in chain.iter().enumerate() {
        if idx > 0 {
            row = row.child(div().text_xs().text_color(to_hsla(p.faint)).child("→"));
        }
        let label = display_model_ref(id_ref, providers);
        row = row.child(
            h_row()
                .gap(px(4.0))
                .px(px(7.0))
                .py(px(2.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.sunken)))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(label))
                .child(
                    div()
                        .id(SharedString::from(format!("fb-rm-{site}-{idx}")))
                        .cursor_pointer()
                        .text_color(to_hsla(p.muted))
                        .hover(|s| s.text_color(to_hsla(p.bad)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_fallback(site, idx, cx);
                        }))
                        .child("×"),
                ),
        );
    }

    if chain.len() < MAX_FALLBACKS {
        let remaining: Vec<_> = view
            .model_choices
            .iter()
            .filter(|c| !chain.contains(&c.id_ref()))
            .cloned()
            .collect();
        let ent = cx.entity().clone();
        row = row.child(model_picker_popover(
            format!("fb-add-pop-{site}"),
            format!("fb-add-btn-{site}"),
            if zh {
                "+ 添加备用"
            } else {
                "+ Add fallback"
            },
            None,
            None,
            remaining,
            lang,
            move |picked, cx| {
                if let Some(c) = picked {
                    let id_ref = c.id_ref();
                    ent.update(cx, |this, cx| this.add_fallback(site, id_ref, cx));
                }
            },
            cx,
        ));
    }

    row
}

/// Formats relative time compactly (`刚刚`, `12 分钟前`, `3 天前`).
#[must_use]
pub fn format_ago_short(ts_ms: i64, now_ms: i64, zh: bool) -> String {
    let diff_s = (now_ms - ts_ms).max(0) / 1000;
    if diff_s < 60 {
        if zh { "刚刚" } else { "just now" }.to_owned()
    } else if diff_s < 3600 {
        let m = diff_s / 60;
        if zh {
            format!("{m} 分钟前")
        } else {
            format!("{m}m ago")
        }
    } else if diff_s < 86_400 {
        let h = diff_s / 3600;
        if zh {
            format!("{h} 小时前")
        } else {
            format!("{h}h ago")
        }
    } else {
        let d = diff_s / 86_400;
        if zh {
            format!("{d} 天前")
        } else {
            format!("{d}d ago")
        }
    }
}

pub(super) fn format_mcp_status(
    sv: &AgentMcpServerView,
    lang: Language,
    zh: bool,
    p: &pingdeck_desktop::theme::ResolvedPalette,
) -> (StatusShape, String, gpui_kit::Hsla) {
    match &sv.status {
        McpServerStatus::Running => {
            let txt = match sv.tool_count {
                Some(n) if zh => format!("已连接 · {n} 个工具"),
                Some(n) => format!("Connected · {n} tool(s)"),
                None => t(lang, "set.tools.mcp.state.running").to_owned(),
            };
            (StatusShape::Ok, txt, to_hsla(p.ink_2))
        }
        McpServerStatus::Starting => (
            StatusShape::Run,
            t(lang, "set.tools.mcp.state.starting").to_owned(),
            to_hsla(p.muted),
        ),
        McpServerStatus::NeedsAuth => (
            StatusShape::Warn,
            if zh {
                "需要处理：缺少令牌"
            } else {
                "Needs auth token"
            }
            .to_owned(),
            to_hsla(p.warn),
        ),
        McpServerStatus::NeedsReview => (
            StatusShape::Warn,
            if zh {
                "配置已变，待保存复核"
            } else {
                "Config changed — review"
            }
            .to_owned(),
            to_hsla(p.warn),
        ),
        McpServerStatus::Error { message } => (
            StatusShape::Bad,
            if message.is_empty() {
                t(lang, "set.tools.mcp.state.error").to_owned()
            } else {
                message.clone()
            },
            to_hsla(p.bad),
        ),
        McpServerStatus::Disabled => (
            StatusShape::Idle,
            t(lang, "set.tools.mcp.state.disabled").to_owned(),
            to_hsla(p.muted),
        ),
    }
}
