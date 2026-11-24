//! Shared layout primitives and model picker popover matching the PingDeck Settings prototype
//! (`docs/design/prototype/js/data-settings.js` & `styles/modules.css`).

use gpui_kit::{
    App, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use pingdeck_desktop::core::app::ProviderView;
use pingdeck_desktop::core::llm::store::ProviderModel;
use pingdeck_desktop::core::model::settings::Language;

use crate::ui::{
    Anchor, InputState, Popover, ScrollableElement as _, StatusShape, Tone, UiIcon, btn_ghost,
    h_row, icon_xs, list_row, palette, popover, select_pill, status_indicator, text_input, to_hsla,
    v_col,
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

/// Prototype `.set-section-head`: bold section title on left, optional right action/reset button,
/// hairline bottom border.
pub fn section_head(
    title: impl Into<SharedString>,
    right: Option<gpui_kit::AnyElement>,
    cx: &App,
) -> gpui_kit::Div {
    let p = palette(cx);
    h_row()
        .justify_between()
        .gap(px(10.0))
        .pb(px(8.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink))
                .child(title.into()),
        )
        .children(right)
}

/// Prototype `.set-meta-pill` ("仅这台电脑" / "Local only" etc.).
pub fn meta_pill(text: impl Into<SharedString>, cx: &App) -> gpui_kit::Div {
    let p = palette(cx);
    div()
        .px(px(6.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .bg(to_hsla(p.sunken))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .text_size(px(10.5))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(to_hsla(p.muted))
        .child(text.into())
}

/// Prototype `.set-row`: label + optional meta pill + optional description on left, control on right.
pub fn set_row(
    label: impl Into<SharedString>,
    meta: Option<SharedString>,
    desc: Option<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> gpui_kit::Div {
    let p = palette(cx);
    let title_row = h_row()
        .gap(px(7.0))
        .flex_wrap()
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .child(label.into()),
        )
        .children(meta.map(|m| meta_pill(m, cx)));

    let mut left = v_col().flex_1().min_w_0().gap(px(2.0)).child(title_row);
    if let Some(d) = desc
        && !d.is_empty()
    {
        left = left.child(div().text_xs().text_color(to_hsla(p.muted)).child(d));
    }

    h_row()
        .justify_between()
        .items_center()
        .gap(px(18.0))
        .py(px(11.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(left)
        .child(div().flex_shrink_0().child(control))
}

/// Compact numeric input paired with a muted trailing unit label (matching prototype `num()`).
pub fn num_box(
    input: &Entity<InputState>,
    unit: Option<&'static str>,
    width_px: f32,
    cx: &App,
) -> gpui_kit::Div {
    let p = palette(cx);
    h_row()
        .gap(px(6.0))
        .child(div().w(px(width_px)).child(text_input(input)))
        .children(unit.map(|u| {
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .flex_shrink_0()
                .child(u)
        }))
}

/// Inline dismissible flash banner for confirmations, warnings, and validation errors.
pub fn flash_banner(
    tone: Tone,
    msg: &str,
    on_dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> gpui_kit::Div {
    let p = palette(cx);
    let (bg, border, fg, shape) = match tone {
        Tone::Danger => (
            to_hsla(p.bad_soft),
            to_hsla(p.bad_line.composite_over(p.bad_soft)),
            to_hsla(p.bad),
            StatusShape::Bad,
        ),
        Tone::Warning => (
            to_hsla(p.warn_soft),
            to_hsla(p.warn_line.composite_over(p.warn_soft)),
            to_hsla(p.warn),
            StatusShape::Warn,
        ),
        Tone::Success => (
            to_hsla(p.ok_soft),
            to_hsla(p.ok_line.composite_over(p.ok_soft)),
            to_hsla(p.ok),
            StatusShape::Ok,
        ),
        _ => (
            to_hsla(p.accent_soft),
            to_hsla(p.accent_line.composite_over(p.accent_soft)),
            to_hsla(p.accent_ink),
            StatusShape::Live,
        ),
    };
    h_row()
        .justify_between()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(8.0))
                .child(status_indicator(shape, cx))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(fg)
                        .child(SharedString::from(msg.to_owned())),
                ),
        )
        .child(btn_ghost("agent-flash-dismiss", "×").on_click(on_dismiss))
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
