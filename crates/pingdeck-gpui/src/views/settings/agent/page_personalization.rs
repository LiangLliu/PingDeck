//! Settings › 智能体 › 个性与指令 (`SettingsSection::Personalization`, prototype `ai/personalization`).

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::i18n::t;

use crate::ui::{
    Disableable as _, Tone, badge, btn_ghost, btn_primary, card, h_row, multiline_input, palette,
    to_hsla, v_col,
};

use super::AgentSettingsView;
use super::widgets::section_head;

const MAX_CHARS: usize = 4_000;

pub(super) fn render_personalization(
    view: &AgentSettingsView,
    lang: Language,
    zh: bool,
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
        if zh {
            format!("最多 4,000 个字符，当前 {char_count} 个，未保存。")
        } else {
            format!("At most 4,000 characters; currently {char_count}. Not saved.")
        }
    } else if zh {
        format!("{char_count} / {MAX_CHARS} 字符")
    } else {
        format!("{char_count} / {MAX_CHARS} chars")
    };

    let status_pill = if view.instructions_dirty && !over_limit {
        Some(badge(
            if zh {
                "未保存更改"
            } else {
                "Unsaved changes"
            },
            Tone::Warning,
            cx,
        ))
    } else {
        None
    };

    v_col()
        .gap(px(12.0))
        .child(section_head(
            if zh {
                "我的说明（全局指令）"
            } else {
                "My instructions (global)"
            },
            status_pill.map(gpui_kit::IntoElement::into_any_element),
            cx,
        ))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(t(lang, "set.sec.personalization.instructions.d").to_owned()),
        )
        .child(
            card(cx).p(px(12.0)).child(
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
                            btn_ghost("agent-pz-clear", t(lang, "set.pz.clear").to_owned())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.clear_global_instructions(window, cx);
                                }))
                        }))
                        .child(
                            btn_primary("agent-pz-save", if zh { "立即保存" } else { "Save now" })
                                .disabled(over_limit || !view.instructions_dirty)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.save_global_instructions(cx);
                                })),
                        ),
                ),
        )
        .child(
            div()
                .pt(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(t(lang, "set.pz.chatNote").to_owned()),
        )
}
