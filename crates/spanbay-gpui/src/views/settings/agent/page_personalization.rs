//! Settings › 智能体 › 个性与指令 (`SettingsSection::Personalization`, prototype `ai/personalization`).

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::settings::Language;

use crate::ui::{
    Disableable as _, Tone, badge, btn_ghost, btn_primary, h_row, multiline_input, palette,
    settings_sub_well, to_hsla, v_col,
};

use super::AgentSettingsView;
use super::widgets::section_card_padded;

const MAX_CHARS: usize = 4_000;

pub(super) fn render_personalization(
    view: &AgentSettingsView,
    lang: Language,
    cx: &mut Context<AgentSettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let current = view.instructions_input.read(cx).value().to_string();
    let char_count = current.trim().chars().count();
    let over_limit = char_count > MAX_CHARS;
    let is_empty = current.trim().is_empty();

    let count_color = if over_limit {
        to_hsla(p.bad)
    } else {
        to_hsla(p.muted)
    };
    let count_text = if over_limit {
        crate::ui::tr(cx)
            .settings()
            .page_personalization_at_most_4_000_characters(char_count)
    } else {
        crate::ui::tr(cx)
            .settings()
            .page_personalization_chars(char_count, MAX_CHARS)
    };

    let status_pill = if view.instructions_dirty && !over_limit {
        Some(badge(
            crate::ui::tr(cx)
                .settings()
                .page_personalization_unsaved_changes(),
            Tone::Warning,
            cx,
        ))
    } else {
        None
    };

    v_col()
        .w_full()
        .gap(px(10.0))
        .child(section_card_padded(
            crate::ui::tr(cx)
                .settings()
                .page_personalization_my_instructions_global(),
            status_pill.map(gpui_kit::IntoElement::into_any_element),
            v_col()
                .w_full()
                .gap(px(12.0))
                .child(
                    div().text_xs().text_color(to_hsla(p.muted)).child(
                        crate::ui::Tr(lang)
                            .settings()
                            .sec_personalization_instructions_d()
                            .to_owned(),
                    ),
                )
                .child(
                    settings_sub_well(cx).p(px(6.0)).child(
                        div()
                            .w_full()
                            .h(px(180.0))
                            .child(multiline_input(&view.instructions_input)),
                    ),
                )
                .child(
                    h_row()
                        .justify_between()
                        .gap(px(12.0))
                        .child(
                            div()
                                .text_xs()
                                .text_color(count_color)
                                .child(SharedString::from(count_text)),
                        )
                        .child(
                            h_row()
                                .gap(px(8.0))
                                .children((!is_empty).then(|| {
                                    btn_ghost(
                                        "agent-pz-clear",
                                        crate::ui::Tr(lang).settings().pz_clear().to_owned(),
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.clear_global_instructions(window, cx);
                                        },
                                    ))
                                }))
                                .child(
                                    btn_primary(
                                        "agent-pz-save",
                                        crate::ui::tr(cx)
                                            .settings()
                                            .page_personalization_save_now(),
                                    )
                                    .disabled(over_limit || !view.instructions_dirty)
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.save_global_instructions(cx);
                                        },
                                    )),
                                ),
                        ),
                ),
            cx,
        ))
        .child(
            div()
                .px(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(crate::ui::Tr(lang).settings().pz_chat_note().to_owned()),
        )
}
